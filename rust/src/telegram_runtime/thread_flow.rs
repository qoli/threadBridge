use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::payloads::setters::*;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::local_control::LocalControlHandle;
use crate::process_transcript::process_entry_from_codex_event;

use super::final_reply::send_final_assistant_reply;
use super::media::{self, dispatch_workspace_telegram_outbox};
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::restore;
use super::status_sync;
use super::*;

async fn start_fresh_binding(
    state: &AppState,
    record: ThreadRecord,
    workspace_path: PathBuf,
) -> Result<ThreadRecord> {
    ensure_workspace_runtime(
        &state.config.runtime.codex_working_directory,
        &state.config.runtime.data_root_path,
        &state.seed_template_path,
        &workspace_path,
    )
    .await?;
    let codex_workspace = prepare_workspace_runtime_for_control(state, workspace_path).await?;
    let binding = state.codex.start_thread(&codex_workspace).await?;
    state
        .repository
        .bind_workspace(record, binding.cwd, binding.thread_id)
        .await
}

async fn busy_snapshot_for_binding(
    state: &AppState,
    binding: &SessionBinding,
) -> Result<Option<crate::workspace_status::BusySelectedSessionStatus>> {
    let workspace_path = workspace_path_from_binding(binding)?;
    if let Some(session_id) = usable_bound_session_id(Some(binding))
        && let Some(busy) =
            busy_selected_session_status(&state.workspace_status_cache, &workspace_path, session_id)
                .await?
    {
        return Ok(Some(busy));
    }
    let Some(tui_session_id) = binding.tui_active_codex_thread_id.as_deref() else {
        return Ok(None);
    };
    if Some(tui_session_id) == usable_bound_session_id(Some(binding)) {
        return Ok(None);
    }
    busy_selected_session_status(
        &state.workspace_status_cache,
        &workspace_path,
        tui_session_id,
    )
    .await
}

async fn render_thread_info(state: &AppState, record: &ThreadRecord) -> Result<String> {
    let session = state.repository.read_session_binding(record).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let current_codex_thread_id = usable_bound_session_id(session.as_ref())
        .map(str::to_owned)
        .unwrap_or_else(|| "none".to_owned());
    let current_snapshot = if let Some(path) = workspace_path.as_ref() {
        read_session_status(path, &current_codex_thread_id).await?
    } else {
        None
    };
    let marker = status_sync::topic_marker_for_snapshot(current_snapshot.as_ref());
    let tui_active_codex_thread_id = session
        .as_ref()
        .and_then(|binding| binding.tui_active_codex_thread_id.as_deref())
        .unwrap_or("none");
    let adoption_state = session
        .as_ref()
        .map(|binding| {
            if binding.tui_session_adoption_pending {
                "pending"
            } else {
                "none"
            }
        })
        .unwrap_or("none");
    let workspace = workspace_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unbound".to_owned());
    let current_phase = current_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.phase))
        .unwrap_or_else(|| "none".to_owned());
    let current_owner = current_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.owner))
        .unwrap_or_else(|| "none".to_owned());

    Ok(format!(
        "thread_key: `{}`\nworkspace: `{}`\ncurrent_codex_thread_id: `{}`\ntui_active_codex_thread_id: `{}`\nadoption_state: `{}`\nmarker: `{}`\ncurrent_phase: `{}`\ncurrent_owner: `{}`",
        record.metadata.thread_key,
        workspace,
        current_codex_thread_id,
        tui_active_codex_thread_id,
        adoption_state,
        status_sync::topic_activity_marker_label(marker),
        current_phase,
        current_owner,
    ))
}

pub(crate) async fn run_command(
    bot: &Bot,
    msg: &Message,
    command: Command,
    state: &AppState,
) -> Result<()> {
    match command {
        Command::Start => {
            let text = if is_control_chat(msg) {
                let record = state.repository.get_main_thread(msg.chat.id.0).await?;
                state
                    .repository
                    .append_log(
                        &record,
                        LogDirection::System,
                        "Control chat initialized from /start.",
                        None,
                    )
                    .await?;
                "Control console.\nUse /add_workspace <absolute-path> for the workspace-first flow."
            } else {
                "Workspace thread.\nUse /new_session, /repair_session, /archive_workspace, /workspace_info, or /rename_workspace here."
            };
            send_scoped_message(bot, msg.chat.id, msg.thread_id, text).await?;
        }
        Command::AddWorkspace => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /add_workspace <absolute-path> from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let Some(argument) = command_argument_text(msg, "add_workspace") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Usage: /add_workspace <absolute-path>",
                )
                .await?;
                return Ok(());
            };
            let control = LocalControlHandle::new(bot.clone(), state.clone());
            match control.add_workspace(argument).await {
                Ok(_) => {}
                Err(error) => {
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        None,
                        format!("Add workspace failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::NewSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /new_session inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet. Archive it and re-add the workspace from the control chat with /add_workspace <absolute-path>.",
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = busy_snapshot_for_binding(state, binding).await? {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            let workspace_path = workspace_path_from_binding(binding)?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = start_fresh_binding(state, record.clone(), workspace_path.clone()).await;
            typing.stop().await;

            match result {
                Ok(record) => {
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Started a fresh Codex session for workspace {}.",
                                workspace_path.display()
                            ),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Started a fresh Codex session for this workspace.",
                    )
                    .await?;
                    let _ =
                        status_sync::refresh_thread_topic_title(bot, state, &record, "new").await;
                }
                Err(error) => {
                    let _ = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("New session failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::RepairSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /repair_session inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(binding) = session.as_ref()
                && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            let workspace_path =
                ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?)
                    .await?;
            let codex_workspace =
                prepare_workspace_runtime_for_control(state, workspace_path).await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnect = state
                .codex
                .reconnect_session(&codex_workspace, existing_thread_id)
                .await;
            typing.stop().await;

            match reconnect {
                Ok(()) => {
                    let updated = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            "Codex session revalidated for the current workspace binding.",
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session continuity verified for this workspace.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        state,
                        &updated,
                        "reconnect_codex_verified",
                    )
                    .await;
                }
                Err(error) => {
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session repair failed. Use /new_session to start fresh or /repair_session to retry.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        state,
                        &updated,
                        "reconnect_codex_broken",
                    )
                    .await;
                }
            }
        }
        Command::WorkspaceInfo => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /workspace_info inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                render_thread_info(state, &record).await?,
            )
            .await?;
        }
        Command::RenameWorkspace => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /rename_workspace inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ThreadStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(binding) = session.as_ref()
                && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            let workspace_path =
                ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?)
                    .await?;
            let codex_workspace = shared_codex_workspace(state, workspace_path.clone()).await?;
            record_bot_status_event(
                &workspace_path,
                "bot_turn_started",
                Some(existing_thread_id),
                None,
                Some("Generate Telegram topic title from conversation"),
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .generate_thread_title_from_session(&codex_workspace, existing_thread_id)
                .await;
            typing.stop().await;

            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let _ = record_bot_status_event(
                        &workspace_path,
                        "bot_turn_failed",
                        Some(existing_thread_id),
                        None,
                        Some("generate_title failed"),
                    )
                    .await;
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /repair_session or /new_session.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        state,
                        &updated,
                        "generate_title_broken",
                    )
                    .await;
                    return Ok(());
                }
            };

            let mut updated = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let title = result.final_response.trim().to_owned();
            record_bot_status_event(
                &workspace_path,
                "bot_turn_completed",
                Some(existing_thread_id),
                None,
                Some(&title),
            )
            .await?;
            updated.metadata.title = Some(title.clone());
            let updated = state.repository.update_metadata(updated).await?;
            info!(
                event = "telegram.generate_title.completed",
                thread_key = %updated.metadata.thread_key,
                chat_id = updated.metadata.chat_id,
                message_thread_id = updated.metadata.message_thread_id.unwrap_or_default(),
                codex_thread_id = existing_thread_id,
                generated_title = %title,
                "generated Telegram topic title from Codex conversation"
            );
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!("Generated title: {title}"),
                    None,
                )
                .await?;
            let _ = status_sync::refresh_thread_topic_title(bot, state, &updated, "generate_title")
                .await;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Workspace renamed: {title}"),
            )
            .await?;
        }
        Command::ArchiveWorkspace => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /archive_workspace inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let _ = bot.delete_forum_topic(msg.chat.id, thread_id).await;
            let record = state.repository.archive_thread(record).await?;
            state
                .repository
                .append_log(&record, LogDirection::System, "Workspace archived.", None)
                .await?;
        }
        Command::RestoreWorkspace => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /restore_workspace from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let (text, markup) = restore::render_restore_page(state, msg.chat.id.0, 0).await?;
            bot.send_message(msg.chat.id, text)
                .link_preview_options(disabled_link_preview_options())
                .reply_markup(markup)
                .await?;
        }
    }
    Ok(())
}

pub(crate) async fn run_text_message(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &AppState,
) -> Result<()> {
    if is_control_chat(msg) {
        send_scoped_message(
            bot,
            msg.chat.id,
            None,
            "Main private chat is the control console. Use /add_workspace <absolute-path> first.",
        )
        .await?;
        return Ok(());
    }

    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if matches!(record.metadata.status, ThreadStatus::Archived) {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This workspace is archived.",
        )
        .await?;
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let (record, session) =
        maybe_route_telegram_input_to_tui_session(state, record, session).await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint(session.as_ref()),
        )
        .await?;
        return Ok(());
    };
    let workspace_path =
        ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?).await?;
    if let Some(binding) = session.as_ref()
        && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
    {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::busy_text_message(&busy.snapshot, false),
        )
        .await?;
        return Ok(());
    }
    info!(
        event = "telegram.thread.message.received",
        thread_key = %record.metadata.thread_key,
        chat_id = record.metadata.chat_id,
        message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
        codex_thread_id = existing_thread_id,
        text = text,
        "received thread text message"
    );

    if let Some(batch) = state.repository.read_pending_image_batch(&record).await? {
        if !batch.images.is_empty() {
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::User,
                    text.to_owned(),
                    msg.from.as_ref().map(|user| user.id.0 as i64),
                )
                .await?;
            media::analyze_pending_image_batch(
                bot,
                state,
                record,
                thread_id,
                &batch.batch_id,
                Some(text),
                None,
            )
            .await?;
            return Ok(());
        }
    }

    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            text.to_owned(),
            msg.from.as_ref().map(|user| user.id.0 as i64),
        )
        .await?;
    let _ = state
        .repository
        .append_transcript_mirror(
            &record,
            &TranscriptMirrorEntry {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                session_id: existing_thread_id.to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: text.to_owned(),
            },
        )
        .await?;
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(text),
    )
    .await?;

    spawn_text_turn(
        bot.clone(),
        state.clone(),
        record,
        msg.chat.id,
        thread_id,
        workspace_path,
        existing_thread_id.to_owned(),
        text.to_owned(),
    );

    Ok(())
}

fn spawn_text_turn(
    bot: Bot,
    state: AppState,
    record: ThreadRecord,
    chat_id: ChatId,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: String,
    text: String,
) {
    tokio::spawn(async move {
        if let Err(error) = execute_text_turn(
            &bot,
            &state,
            record,
            chat_id,
            thread_id,
            workspace_path,
            &existing_thread_id,
            &text,
        )
        .await
        {
            error!(
                event = "telegram.thread.message.background_failed",
                chat_id = chat_id.0,
                message_thread_id = thread_id_to_i32(thread_id),
                error = %error,
                "background text turn failed"
            );
            let _ = send_scoped_message(
                &bot,
                chat_id,
                Some(thread_id),
                format!("Request failed: {error}"),
            )
            .await;
        }
    });
}

async fn execute_text_turn(
    bot: &Bot,
    state: &AppState,
    mut record: ThreadRecord,
    chat_id: ChatId,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: &str,
    text: &str,
) -> Result<()> {
    let typing = TypingHeartbeat::start(bot.clone(), chat_id, Some(thread_id));
    let codex_workspace = shared_codex_workspace(state, workspace_path.clone()).await?;
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        chat_id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    let mirror_record = record.clone();
    let mirror_repository = state.repository.clone();
    let mirror_session_id = existing_thread_id.to_owned();

    let result = state
        .codex
        .run_locked_prompt_with_events(&codex_workspace, existing_thread_id, text, |event| {
            let preview = preview.clone();
            let mirror_record = mirror_record.clone();
            let mirror_repository = mirror_repository.clone();
            let mirror_session_id = mirror_session_id.clone();
            async move {
                preview.lock().await.consume(&event).await;
                if let Some(entry) = process_entry_from_codex_event(
                    &event,
                    &mirror_session_id,
                    TranscriptMirrorOrigin::Telegram,
                ) {
                    preview.lock().await.consume_process_entry(&entry).await;
                    let _ = mirror_repository
                        .append_transcript_mirror(&mirror_record, &entry)
                        .await;
                }
            }
        })
        .await;
    preview_heartbeat.stop().await;
    typing.stop().await;

    match result {
        Ok(result) => {
            record_bot_status_event(
                &workspace_path,
                "bot_turn_completed",
                Some(existing_thread_id),
                None,
                Some(&result.final_response),
            )
            .await?;
            record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::Assistant,
                    result.final_response.clone(),
                    None,
                )
                .await?;
            let _ = state
                .repository
                .append_transcript_mirror(
                    &record,
                    &TranscriptMirrorEntry {
                        timestamp: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        session_id: existing_thread_id.to_owned(),
                        origin: TranscriptMirrorOrigin::Telegram,
                        role: TranscriptMirrorRole::Assistant,
                        delivery: TranscriptMirrorDelivery::Final,
                        phase: None,
                        text: result.final_response.clone(),
                    },
                )
                .await?;
            if !preview.lock().await.complete(&result.final_response).await {
                let final_text = if result.final_response.trim().is_empty() {
                    preview
                        .lock()
                        .await
                        .fallback_final_response()
                        .trim()
                        .to_owned()
                } else {
                    result.final_response
                };
                if !final_text.trim().is_empty() {
                    send_final_assistant_reply(bot, &record, Some(thread_id), &final_text).await?;
                }
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
        }
        Err(error) => {
            let _ = record_bot_status_event(
                &workspace_path,
                "bot_turn_failed",
                Some(existing_thread_id),
                None,
                Some("text turn failed"),
            )
            .await;
            error!(
                event = "telegram.thread.message.codex_failed",
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                codex_thread_id = existing_thread_id,
                error = %error,
                "codex turn failed for thread message"
            );
            let record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!("Codex turn failed: {error}"),
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                chat_id,
                Some(thread_id),
                "Codex session is unavailable. Use /repair_session to retry or /new_session to start a fresh one.",
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(
                bot,
                state,
                &record,
                "thread_message_codex_failed",
            )
            .await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn thread_flow_module_compiles_without_attach_helpers() {}
}
