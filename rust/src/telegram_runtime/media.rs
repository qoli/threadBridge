use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::payloads::setters::*;
use teloxide::types::{
    CallbackQueryId, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ThreadId,
};
use tokio::sync::Mutex;
use tracing::{error, info};

use super::final_reply::{compose_visible_final_reply, send_final_assistant_reply};
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::status_sync;
use super::*;
use crate::execution_mode::workspace_execution_mode;
use crate::image_artifacts::PendingImageBatch;
use crate::tool_results::TelegramDeliverySurface;

pub(crate) const CALLBACK_IMAGE_BATCH_ANALYZE: &str = "image_batch_analyze";
const TELEGRAM_OUTBOX_FILE: &str = ".threadbridge/tool_results/telegram_outbox.json";
const TELEGRAM_BOT_PHOTO_LIMIT_BYTES: u64 = 10 * 1024 * 1024;
const TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboxFileKind {
    Photo,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutboxFileDispatchPlan {
    SendAsPhoto,
    SendAsDocument { notice: Option<String> },
    Skip { notice: String },
}

#[derive(Clone)]
pub(crate) struct IncomingImage {
    caption: Option<String>,
    file_id: FileId,
    file_name: String,
    mime_type: String,
}

fn file_extension_for_image(mime_type: &str, file_name: Option<&str>) -> String {
    if let Some(file_name) = file_name {
        if let Some((_, ext)) = file_name.rsplit_once('.') {
            if !ext.is_empty() {
                return format!(".{}", ext.to_lowercase());
            }
        }
    }
    match mime_type {
        "image/jpeg" => ".jpg".to_owned(),
        "image/gif" => ".gif".to_owned(),
        "image/webp" => ".webp".to_owned(),
        _ => ".png".to_owned(),
    }
}

pub(crate) fn extract_incoming_image(msg: &Message) -> Option<IncomingImage> {
    let caption = msg
        .caption()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(photo) = msg.photo() {
        let best = photo.last()?;
        return Some(IncomingImage {
            caption,
            file_id: best.file.id.clone(),
            file_name: format!("photo_{}.jpg", msg.id.0),
            mime_type: "image/jpeg".to_owned(),
        });
    }
    if let Some(document) = msg.document() {
        let mime_type = document.mime_type.as_ref()?.essence_str().to_owned();
        if !mime_type.starts_with("image/") {
            return None;
        }
        let ext = file_extension_for_image(&mime_type, document.file_name.as_deref());
        let stem = document
            .file_name
            .as_deref()
            .and_then(|name| name.rsplit_once('.').map(|(left, _)| left.to_owned()))
            .unwrap_or_else(|| format!("document_{}", msg.id.0));
        return Some(IncomingImage {
            caption,
            file_id: document.file.id.clone(),
            file_name: format!("{stem}{ext}"),
            mime_type,
        });
    }
    None
}

async fn download_telegram_file(state: &AppState, bot: &Bot, file_id: FileId) -> Result<Vec<u8>> {
    let file = bot.get_file(file_id).await?;
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        state.config.telegram.telegram_token, file.path
    );
    let response = reqwest::get(url).await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("Failed to download Telegram file: HTTP {status}");
    }
    Ok(response.bytes().await?.to_vec())
}

async fn upsert_pending_image_batch_message(
    bot: &Bot,
    state: &AppState,
    chat_id: ChatId,
    thread_id: ThreadId,
    record: &ThreadRecord,
    batch: crate::image_artifacts::PendingImageBatch,
) -> Result<crate::image_artifacts::PendingImageBatch> {
    let text = format_system_text(
        TelegramSystemIntent::Info,
        &render_pending_image_batch(&batch),
    );
    let markup = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "直接分析",
        format!("{CALLBACK_IMAGE_BATCH_ANALYZE}:{}", batch.batch_id),
    )]]);
    if let Some(control_message_id) = batch.control_message_id {
        if bot
            .edit_message_text(
                chat_id,
                teloxide::types::MessageId(control_message_id),
                text.clone(),
            )
            .link_preview_options(disabled_link_preview_options())
            .reply_markup(markup.clone())
            .await
            .is_ok()
        {
            return Ok(batch);
        }
    }
    let message = bot
        .send_message(chat_id, text)
        .link_preview_options(disabled_link_preview_options())
        .message_thread_id(thread_id)
        .reply_markup(markup)
        .await?;
    state
        .repository
        .set_pending_image_batch_control_message_id(record, batch, message.id.0)
        .await
}

async fn read_file_or_none(path: impl Into<PathBuf>) -> Result<Option<String>> {
    let path = path.into();
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub(crate) async fn read_telegram_outbox(
    workspace_path: &std::path::Path,
) -> Result<Option<crate::tool_results::TelegramOutbox>> {
    let Some(result_text) = read_file_or_none(workspace_path.join(TELEGRAM_OUTBOX_FILE)).await?
    else {
        return Ok(None);
    };
    Ok(Some(parse_telegram_outbox(&result_text)?))
}

async fn remove_file_if_exists(path: impl Into<PathBuf>) -> Result<()> {
    let path = path.into();
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn resolve_workspace_file_path(workspace_path: &std::path::Path, relative_path: &str) -> PathBuf {
    workspace_path.join(relative_path)
}

async fn file_size_bytes(path: &std::path::Path) -> Result<u64> {
    Ok(tokio::fs::metadata(path)
        .await
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len())
}

fn outbox_notice_text(surface: TelegramDeliverySurface, message: impl AsRef<str>) -> String {
    match surface {
        TelegramDeliverySurface::Content => message.as_ref().to_owned(),
        other => format!(
            "[{}] {}",
            format!("{other:?}").to_lowercase(),
            message.as_ref()
        ),
    }
}

async fn send_outbox_notice(
    bot: &Bot,
    record: &ThreadRecord,
    thread_id: ThreadId,
    surface: TelegramDeliverySurface,
    message: impl AsRef<str>,
) -> Result<()> {
    send_scoped_warning_message(
        bot,
        ChatId(record.metadata.chat_id),
        Some(thread_id),
        outbox_notice_text(surface, message),
    )
    .await?;
    Ok(())
}

fn plan_outbox_file_dispatch(
    kind: OutboxFileKind,
    size_bytes: u64,
    relative_path: &str,
) -> OutboxFileDispatchPlan {
    match kind {
        OutboxFileKind::Photo if size_bytes > TELEGRAM_BOT_PHOTO_LIMIT_BYTES => {
            if size_bytes <= TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES {
                OutboxFileDispatchPlan::SendAsDocument {
                    notice: Some(format!(
                        "Photo `{relative_path}` exceeded Telegram's bot photo limit; sending it as a document instead."
                    )),
                }
            } else {
                OutboxFileDispatchPlan::Skip {
                    notice: format!(
                        "Photo `{relative_path}` exceeded Telegram's bot upload limit and was not delivered."
                    ),
                }
            }
        }
        OutboxFileKind::Document if size_bytes > TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES => {
            OutboxFileDispatchPlan::Skip {
                notice: format!(
                    "Document `{relative_path}` exceeded Telegram's bot upload limit and was not delivered."
                ),
            }
        }
        OutboxFileKind::Photo => OutboxFileDispatchPlan::SendAsPhoto,
        OutboxFileKind::Document => OutboxFileDispatchPlan::SendAsDocument { notice: None },
    }
}

async fn send_outbox_document(
    bot: &Bot,
    record: &ThreadRecord,
    thread_id: ThreadId,
    path: &std::path::Path,
    caption: Option<&String>,
) -> Result<()> {
    let request = bot
        .send_document(
            ChatId(record.metadata.chat_id),
            InputFile::file(path.to_path_buf()),
        )
        .message_thread_id(thread_id);
    if let Some(caption) = caption {
        request.caption(caption.clone()).await?;
    } else {
        request.await?;
    }
    Ok(())
}

async fn dispatch_outbox_file_item(
    bot: &Bot,
    record: &ThreadRecord,
    thread_id: ThreadId,
    workspace_path: &std::path::Path,
    relative_path: &str,
    caption: Option<&String>,
    surface: TelegramDeliverySurface,
    kind: OutboxFileKind,
) -> Result<()> {
    let absolute_path = resolve_workspace_file_path(workspace_path, relative_path);
    let size_bytes = match file_size_bytes(&absolute_path).await {
        Ok(size) => size,
        Err(error) => {
            send_outbox_notice(
                bot,
                record,
                thread_id,
                surface,
                format!("Attachment `{relative_path}` is missing or unreadable: {error}"),
            )
            .await?;
            return Ok(());
        }
    };

    match plan_outbox_file_dispatch(kind, size_bytes, relative_path) {
        OutboxFileDispatchPlan::SendAsPhoto => {
            let request = bot
                .send_photo(
                    ChatId(record.metadata.chat_id),
                    InputFile::file(absolute_path),
                )
                .message_thread_id(thread_id);
            if let Some(caption) = caption {
                request.caption(caption.clone()).await?;
            } else {
                request.await?;
            }
        }
        OutboxFileDispatchPlan::SendAsDocument { notice } => {
            if let Some(notice) = notice {
                send_outbox_notice(bot, record, thread_id, surface, notice).await?;
            }
            send_outbox_document(bot, record, thread_id, &absolute_path, caption).await?;
        }
        OutboxFileDispatchPlan::Skip { notice } => {
            send_outbox_notice(bot, record, thread_id, surface, notice).await?;
        }
    }

    Ok(())
}

pub(crate) async fn dispatch_workspace_telegram_outbox(
    bot: &Bot,
    state: &AppState,
    record: &ThreadRecord,
    thread_id: ThreadId,
) -> Result<()> {
    let session = state.repository.read_session_binding(record).await?;
    let Some(binding) = session.as_ref() else {
        return Ok(());
    };
    let workspace_path = workspace_path_from_binding(binding)?;
    let Some(outbox) = read_telegram_outbox(&workspace_path).await? else {
        return Ok(());
    };
    if outbox.items.is_empty() {
        remove_file_if_exists(workspace_path.join(TELEGRAM_OUTBOX_FILE)).await?;
        return Ok(());
    }

    for item in &outbox.items {
        match item {
            TelegramOutboxItem::Text { text, surface } => {
                send_scoped_system_message_with_intent(
                    bot,
                    ChatId(record.metadata.chat_id),
                    Some(thread_id),
                    match surface {
                        TelegramDeliverySurface::Status => TelegramSystemIntent::Warning,
                        _ => TelegramSystemIntent::Info,
                    },
                    text.clone(),
                )
                .await?;
            }
            TelegramOutboxItem::Photo {
                path,
                caption,
                surface,
            } => {
                dispatch_outbox_file_item(
                    bot,
                    record,
                    thread_id,
                    &workspace_path,
                    path,
                    caption.as_ref(),
                    *surface,
                    OutboxFileKind::Photo,
                )
                .await?;
            }
            TelegramOutboxItem::Document {
                path,
                caption,
                surface,
            } => {
                dispatch_outbox_file_item(
                    bot,
                    record,
                    thread_id,
                    &workspace_path,
                    path,
                    caption.as_ref(),
                    *surface,
                    OutboxFileKind::Document,
                )
                .await?;
            }
        }
    }

    remove_file_if_exists(workspace_path.join(TELEGRAM_OUTBOX_FILE)).await?;
    state
        .repository
        .append_log(
            record,
            LogDirection::System,
            format!(
                "Dispatched {} Telegram outbox item(s) from workspace runtime.",
                outbox.items.len()
            ),
            None,
        )
        .await?;
    Ok(())
}

pub(crate) async fn queue_image_for_thread(
    bot: &Bot,
    msg: &Message,
    state: &AppState,
    image: IncomingImage,
) -> Result<()> {
    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    let session = state.repository.read_session_binding(&record).await?;
    let (record, session) = state
        .control
        .session_routing_service()
        .maybe_route_telegram_input_to_tui_session(record, session)
        .await?
        .into_record_session();
    let (resolved_state, blocking_snapshot) =
        resolve_busy_gate_state(state, &record, session.as_ref()).await?;
    if resolved_state.is_archived() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This thread is archived.",
        )
        .await?;
        return Ok(());
    }
    if usable_bound_session_id(resolved_state, session.as_ref()).is_none() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint_for_state(resolved_state, session.as_ref()),
        )
        .await?;
        return Ok(());
    }
    let _workspace_path = state
        .control
        .workspace_runtime_service()
        .ensure_bound_workspace_runtime(session.as_ref().context("missing session binding")?)
        .await?;
    let pending = state
        .repository
        .get_or_create_pending_image_batch(&record)
        .await?;
    info!(
        event = "telegram.thread.image.received",
        thread_key = %record.metadata.thread_key,
        chat_id = record.metadata.chat_id,
        message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
        batch_id = %pending.batch_id,
        file_name = %image.file_name,
        "received image for thread batch"
    );
    let data = download_telegram_file(state, bot, image.file_id.clone()).await?;
    let updated = state
        .repository
        .append_image_to_pending_batch(
            &record,
            pending,
            AppendPendingImageInput {
                caption: image.caption.clone(),
                data,
                file_name: image.file_name.clone(),
                mime_type: image.mime_type.clone(),
                source_message_id: msg.id.0,
                telegram_file_id: image.file_id.0,
            },
        )
        .await?;
    let persisted =
        upsert_pending_image_batch_message(bot, state, msg.chat.id, thread_id, &record, updated)
            .await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            match image.caption {
                Some(caption) => format!("[image] {} | caption: {}", image.file_name, caption),
                None => format!("[image] {}", image.file_name),
            },
            msg.from.as_ref().map(|user| user.id.0 as i64),
        )
        .await?;
    if persisted.control_message_id.is_some() {
        state
            .repository
            .append_log(
                &record,
                LogDirection::System,
                format!(
                    "Image batch updated: {} image(s) waiting for analysis.",
                    persisted.images.len()
                ),
                None,
            )
            .await?;
    }
    if let Some(busy) = blocking_snapshot.as_ref() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::busy_text_message(busy, true),
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn analyze_pending_image_batch(
    bot: &Bot,
    state: &AppState,
    record: ThreadRecord,
    thread_id: ThreadId,
    batch_id: &str,
    user_prompt: Option<&str>,
    callback_query_id: Option<&CallbackQueryId>,
) -> Result<()> {
    let session = state.repository.read_session_binding(&record).await?;
    let (resolved_state, _) = resolve_busy_gate_state(state, &record, session.as_ref()).await?;
    if resolved_state.is_archived() {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text("This thread is archived.")
                .show_alert(true)
                .await?;
        }
        return Ok(());
    }
    let (record, session) = state
        .control
        .session_routing_service()
        .maybe_route_telegram_input_to_tui_session(record, session)
        .await?
        .into_record_session();
    let (resolved_state, blocking_snapshot) =
        resolve_busy_gate_state(state, &record, session.as_ref()).await?;
    let Some(existing_thread_id) = usable_bound_session_id(resolved_state, session.as_ref()) else {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text(session_binding_hint_for_state(
                    resolved_state,
                    session.as_ref(),
                ))
                .show_alert(true)
                .await?;
        }
        return Ok(());
    };
    let workspace_path = state
        .control
        .workspace_runtime_service()
        .ensure_bound_workspace_runtime(session.as_ref().context("missing session binding")?)
        .await?;
    if let Some(busy) = blocking_snapshot.as_ref() {
        let text = status_sync::busy_text_message(busy, false);
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text(text)
                .show_alert(true)
                .await?;
        } else {
            send_scoped_message(bot, ChatId(record.metadata.chat_id), Some(thread_id), text)
                .await?;
        }
        return Ok(());
    }
    let Some(batch) = state.repository.read_pending_image_batch(&record).await? else {
        return Ok(());
    };
    if batch.batch_id != batch_id || batch.images.is_empty() {
        return Ok(());
    }
    if let Some(callback_query_id) = callback_query_id {
        bot.answer_callback_query(callback_query_id.clone())
            .text("Starting image analysis...")
            .await?;
    }
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(user_prompt.unwrap_or("Analyze pending image batch")),
    )
    .await?;
    spawn_image_analysis_turn(
        bot.clone(),
        state.clone(),
        record,
        thread_id,
        workspace_path,
        existing_thread_id.to_owned(),
        batch,
        user_prompt.map(str::to_owned),
    );
    Ok(())
}

fn spawn_image_analysis_turn(
    bot: Bot,
    state: AppState,
    record: ThreadRecord,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: String,
    batch: PendingImageBatch,
    user_prompt: Option<String>,
) {
    let chat_id = ChatId(record.metadata.chat_id);
    tokio::spawn(async move {
        if let Err(error) = execute_image_analysis_turn(
            &bot,
            &state,
            record,
            thread_id,
            workspace_path,
            &existing_thread_id,
            batch,
            user_prompt.as_deref(),
        )
        .await
        {
            error!(
                event = "telegram.thread.image_analysis.background_failed",
                chat_id = chat_id.0,
                message_thread_id = thread_id_to_i32(thread_id),
                error = %error,
                "background image analysis failed"
            );
            let _ = send_scoped_warning_message(
                &bot,
                chat_id,
                Some(thread_id),
                format!("Image handling failed: {error}"),
            )
            .await;
        }
    });
}

async fn execute_image_analysis_turn(
    bot: &Bot,
    state: &AppState,
    record: ThreadRecord,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: &str,
    batch: PendingImageBatch,
    user_prompt: Option<&str>,
) -> Result<()> {
    let chat_id = ChatId(record.metadata.chat_id);
    let typing = TypingHeartbeat::start(bot.clone(), chat_id, Some(thread_id));
    let codex_workspace = state
        .control
        .workspace_runtime_service()
        .shared_codex_workspace(workspace_path.clone())
        .await?;
    let prompt = build_image_analysis_prompt(&batch, user_prompt);
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        chat_id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    let mut input = vec![CodexInputItem::Text {
        text: prompt.clone(),
    }];
    for image in &batch.images {
        input.push(CodexInputItem::LocalImage {
            path: record
                .folder_path
                .join(&image.relative_path)
                .display()
                .to_string(),
        });
    }
    let execution_mode = workspace_execution_mode(&workspace_path).await?;
    let turn_workspace_path = workspace_path.clone();
    let turn_session_id = existing_thread_id.to_owned();
    let result = state
        .codex
        .run_locked_with_events_and_mode(
            &codex_workspace,
            existing_thread_id,
            Some(execution_mode),
            input,
            |event| {
                let preview = preview.clone();
                let turn_workspace_path = turn_workspace_path.clone();
                let turn_session_id = turn_session_id.clone();
                async move {
                    if let crate::codex::CodexThreadEvent::TurnStarted {
                        turn_id: Some(turn_id),
                    } = &event
                    {
                        let _ = record_bot_status_event(
                            &turn_workspace_path,
                            "bot_turn_started",
                            Some(&turn_session_id),
                            Some(turn_id),
                            None,
                        )
                        .await;
                    }
                    preview.lock().await.consume(&event).await;
                }
            },
        )
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = record_bot_status_event(
                &workspace_path,
                "bot_turn_failed",
                Some(existing_thread_id),
                None,
                Some("image analysis failed"),
            )
            .await;
            error!(
                event = "telegram.thread.image_analysis.codex_failed",
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                codex_thread_id = existing_thread_id,
                batch_id = %batch.batch_id,
                error = %error,
                "codex image analysis failed"
            );
            preview_heartbeat.stop().await;
            typing.stop().await;
            let record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!("Codex image analysis failed: {error}"),
                    None,
                )
                .await?;
            let _ = status_sync::refresh_thread_topic_title(
                bot,
                &state.repository,
                &record,
                "image_analysis_codex_failed",
            )
            .await;
            return Err(error);
        }
    };
    let visible_final_text =
        compose_visible_final_reply(&result.final_response, result.final_plan_text.as_deref());
    record_bot_status_event(
        &workspace_path,
        "bot_turn_completed",
        Some(existing_thread_id),
        None,
        visible_final_text.as_deref(),
    )
    .await?;
    let record = state
        .repository
        .mark_session_binding_verified(record)
        .await?;
    let record = state
        .repository
        .update_session_execution_snapshot(record, &result.execution)
        .await?;
    let preview_finalized = if let Some(final_text) = visible_final_text.as_deref() {
        preview.lock().await.complete(final_text).await
    } else {
        false
    };
    let artifact_result_text = if let Some(final_text) = visible_final_text {
        final_text
    } else {
        let fallback = preview
            .lock()
            .await
            .fallback_final_response()
            .trim()
            .to_owned();
        if fallback.is_empty() {
            "Codex completed image analysis without a final answer.".to_owned()
        } else {
            fallback
        }
    };
    preview_heartbeat.stop().await;
    typing.stop().await;
    let artifact = ImageAnalysisArtifact {
        batch_id: batch.batch_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        image_count: batch.images.len(),
        images: batch
            .images
            .iter()
            .map(|image| ImageAnalysisImage {
                file_name: image.file_name.clone(),
                mime_type: image.mime_type.clone(),
                relative_path: image.relative_path.clone(),
                source_message_id: image.source_message_id,
            })
            .collect(),
        prompt,
        result_text: artifact_result_text,
    };
    state
        .repository
        .write_image_analysis(&record, &artifact)
        .await?;
    state.repository.clear_pending_image_batch(&record).await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::Assistant,
            artifact.result_text.clone(),
            None,
        )
        .await?;
    let _ = state
        .repository
        .append_transcript_mirror(
            &record,
            &crate::repository::TranscriptMirrorEntry {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                session_id: existing_thread_id.to_owned(),
                origin: crate::repository::TranscriptMirrorOrigin::Telegram,
                role: crate::repository::TranscriptMirrorRole::Assistant,
                delivery: crate::repository::TranscriptMirrorDelivery::Final,
                phase: None,
                text: artifact.result_text.clone(),
            },
        )
        .await?;
    if !preview_finalized {
        send_final_assistant_reply(bot, &record, Some(thread_id), &artifact.result_text).await?;
    }
    dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        OutboxFileDispatchPlan, OutboxFileKind, TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES,
        TELEGRAM_BOT_PHOTO_LIMIT_BYTES, plan_outbox_file_dispatch,
    };

    #[test]
    fn photo_over_limit_falls_back_to_document() {
        let plan = plan_outbox_file_dispatch(
            OutboxFileKind::Photo,
            TELEGRAM_BOT_PHOTO_LIMIT_BYTES + 1,
            "images/generated/test.png",
        );
        assert!(matches!(
            plan,
            OutboxFileDispatchPlan::SendAsDocument { notice: Some(_) }
        ));
    }

    #[test]
    fn oversized_document_is_skipped_with_notice() {
        let plan = plan_outbox_file_dispatch(
            OutboxFileKind::Document,
            TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES + 1,
            "artifacts/report.zip",
        );
        assert!(matches!(plan, OutboxFileDispatchPlan::Skip { .. }));
    }
}
