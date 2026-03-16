use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::payloads::setters::*;
use tokio::sync::Mutex;

use super::media::{self, dispatch_workspace_telegram_outbox};
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::restore;
use super::*;

pub(crate) async fn run_command(
    bot: &Bot,
    msg: &Message,
    command: Command,
    state: &AppState,
) -> Result<()> {
    match command {
        Command::Start => {
            let text = if is_control_chat(msg) {
                "Control console.\nUse /new_thread to create a thread."
            } else {
                "Thread workspace.\nUse /bind_session <session_id> after you pick a Codex session."
            };
            send_scoped_message(bot, msg.chat.id, msg.thread_id, text).await?;
        }
        Command::NewThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /new_thread from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let title = format!("Thread {}", chrono::Local::now().format("%m-%d %H:%M"));
            let topic = bot.create_forum_topic(msg.chat.id, title.clone()).await?;
            let record = state
                .repository
                .create_thread(
                    msg.chat.id.0,
                    thread_id_to_i32(topic.thread_id),
                    title.clone(),
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                None,
                format!("Created thread \"{}\".", topic.name),
            )
            .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    "Telegram thread created. Awaiting Codex session binding.",
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(topic.thread_id),
                "Thread created.\n\nUse /list_sessions to inspect local Codex sessions, then /bind_session <session_id> in this thread.",
            )
            .await?;
        }
        Command::ListSessions => {
            let sessions = state.codex_home.list_recent_sessions(8)?;
            send_scoped_message(
                bot,
                msg.chat.id,
                msg.thread_id,
                format_session_list_text(&sessions),
            )
            .await?;
        }
        Command::BindSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /bind_session <session_id> inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let Some(session_id) = command_argument_text(msg, "bind_session") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Usage: /bind_session <session_id>",
                )
                .await?;
                return Ok(());
            };
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
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let resolved = match state.codex_home.resolve_session(session_id)? {
                Some(session) => session,
                None => {
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("Codex session not found: `{session_id}`"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let workspace_path = record.linked_workspace_path();
            ensure_linked_workspace_runtime(
                &state.config.runtime.codex_working_directory,
                &state.seed_template_path,
                &record.folder_path,
                &workspace_path,
                &resolved.cwd,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let verification = state
                .codex
                .reconnect_session(
                    &CodexWorkspace {
                        agents_path: record.agents_path(),
                        working_directory: workspace_path.clone(),
                    },
                    &resolved.id,
                )
                .await;
            typing.stop().await;
            let record = state
                .repository
                .bind_session(
                    record,
                    resolved.id.clone(),
                    Some(resolved.title.clone()),
                    resolved.cwd.display().to_string(),
                )
                .await?;
            match verification {
                Ok(_) => {
                    let record = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Bound Telegram thread to Codex session {} ({})",
                                resolved.id,
                                resolved.cwd.display()
                            ),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!(
                            "Bound to Codex session `{}`.\nWorkspace: `{}`",
                            resolved.id,
                            resolved.cwd.display()
                        ),
                    )
                    .await?;
                }
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!("Codex session bind verification failed: {error}"),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!(
                            "Session was linked but verification failed.\nUse /reconnect_codex to retry or /bind_session <session_id> to attach another one.\n\nError: {error}"
                        ),
                    )
                    .await?;
                }
            }
        }
        Command::ReconnectCodex => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /reconnect_codex inside a thread.",
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
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnect = state
                .codex
                .reconnect_session(
                    &CodexWorkspace {
                        agents_path: record.agents_path(),
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            match reconnect {
                Ok(_) => {
                    let updated = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            "Codex session revalidated using the bound session id.",
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session reconnected for this thread.",
                    )
                    .await?;
                }
                Err(error) => {
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            format!("Codex reconnect failed: {error}"),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session revalidation failed. Use /bind_session <session_id> to reattach or /reconnect_codex to retry the current one.",
                    )
                    .await?;
                }
            }
        }
        Command::GenerateTitle => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /generate_title inside a thread.",
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
                    "This thread is archived.",
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
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .generate_thread_title_from_session(
                    &CodexWorkspace {
                        agents_path: record.agents_path(),
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let mut updated = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let title = result.final_response.trim().to_owned();
            updated.metadata.title = Some(title.clone());
            let updated = state.repository.update_metadata(updated).await?;
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!("Generated title: {title}"),
                    None,
                )
                .await?;
            let _ = bot
                .edit_forum_topic(msg.chat.id, thread_id)
                .name(title.clone())
                .await;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Title updated: {title}"),
            )
            .await?;
        }
        Command::ArchiveThread => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /archive_thread inside a thread.",
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
                .append_log(&record, LogDirection::System, "Thread archived.", None)
                .await?;
        }
        Command::RestoreThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /restore_thread from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let (text, markup) = restore::render_restore_page(state, msg.chat.id.0, 0).await?;
            bot.send_message(msg.chat.id, text)
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
            "Main private chat is the control console. Use /new_thread first.",
        )
        .await?;
        return Ok(());
    }

    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let mut record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if matches!(record.metadata.status, ThreadStatus::Archived) {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This thread is archived.",
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
    let workspace_path = ensure_bound_workspace_runtime(
        state,
        &record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;

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

    let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        msg.chat.id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());

    let result = state
        .codex
        .run_locked_prompt_with_events(
            &CodexWorkspace {
                agents_path: record.agents_path(),
                working_directory: workspace_path,
            },
            existing_thread_id,
            text,
            |event| {
                let preview = preview.clone();
                async move {
                    preview.lock().await.consume(&event).await;
                }
            },
        )
        .await;
    preview_heartbeat.stop().await;
    typing.stop().await;

    match result {
        Ok(result) => {
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
                    send_scoped_message(bot, msg.chat.id, Some(thread_id), final_text).await?;
                }
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
        }
        Err(error) => {
            record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            let _ = record;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                "Codex session is unavailable. Use /reconnect_codex to retry or /bind_session <session_id> to attach another one.",
            )
            .await?;
        }
    }

    Ok(())
}
