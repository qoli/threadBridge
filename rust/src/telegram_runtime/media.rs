use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::payloads::setters::*;
use teloxide::types::{
    CallbackQueryId, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ThreadId,
};
use tokio::sync::Mutex;
use tracing::{error, info};

use super::final_reply::send_final_assistant_reply;
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::status_sync;
use super::*;

pub(crate) const CALLBACK_IMAGE_BATCH_ANALYZE: &str = "image_batch_analyze";
const TELEGRAM_OUTBOX_FILE: &str = ".threadbridge/tool_results/telegram_outbox.json";

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
        state.config.telegram_token, file.path
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
    let text = render_pending_image_batch(&batch);
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
            TelegramOutboxItem::Text { text } => {
                send_scoped_message(
                    bot,
                    ChatId(record.metadata.chat_id),
                    Some(thread_id),
                    text.clone(),
                )
                .await?;
            }
            TelegramOutboxItem::Photo { path, caption } => {
                let request = bot
                    .send_photo(
                        ChatId(record.metadata.chat_id),
                        InputFile::file(resolve_workspace_file_path(&workspace_path, path)),
                    )
                    .message_thread_id(thread_id);
                if let Some(caption) = caption {
                    request.caption(caption.clone()).await?;
                } else {
                    request.await?;
                }
            }
            TelegramOutboxItem::Document { path, caption } => {
                let request = bot
                    .send_document(
                        ChatId(record.metadata.chat_id),
                        InputFile::file(resolve_workspace_file_path(&workspace_path, path)),
                    )
                    .message_thread_id(thread_id);
                if let Some(caption) = caption {
                    request.caption(caption.clone()).await?;
                } else {
                    request.await?;
                }
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
    if usable_bound_session_id(session.as_ref()).is_none() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint(session.as_ref()),
        )
        .await?;
        return Ok(());
    }
    let workspace_path =
        ensure_bound_workspace_runtime(state, session.as_ref().context("missing session binding")?)
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
    if let Some(session_id) = usable_bound_session_id(session.as_ref())
        && let Some(busy) =
            busy_selected_session_status(&state.workspace_status_cache, &workspace_path, session_id)
                .await?
    {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::busy_text_message(&busy.snapshot, true),
        )
        .await?;
    }
    if let Some(binding) = session.as_ref()
        && super::thread_flow::selected_live_cli_owned_session(state, binding)
            .await?
            .is_some()
    {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::cli_owned_text_message(true),
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
    if matches!(record.metadata.status, ThreadStatus::Archived) {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text("This thread is archived.")
                .show_alert(true)
                .await?;
        }
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text(session_binding_hint(session.as_ref()))
                .show_alert(true)
                .await?;
        }
        return Ok(());
    };
    let workspace_path =
        ensure_bound_workspace_runtime(state, session.as_ref().context("missing session binding")?)
            .await?;
    if let Some(busy) = busy_selected_session_status(
        &state.workspace_status_cache,
        &workspace_path,
        existing_thread_id,
    )
    .await?
    {
        let text = status_sync::busy_text_message(&busy.snapshot, false);
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
    if let Some(binding) = session.as_ref()
        && super::thread_flow::selected_live_cli_owned_session(state, binding)
            .await?
            .is_some()
    {
        let text = status_sync::cli_owned_text_message(true);
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
    let typing = TypingHeartbeat::start(
        bot.clone(),
        ChatId(record.metadata.chat_id),
        Some(thread_id),
    );
    let prompt = build_image_analysis_prompt(&batch, user_prompt);
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        ChatId(record.metadata.chat_id),
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(user_prompt.unwrap_or("Analyze pending image batch")),
    )
    .await?;
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
    let result = state
        .codex
        .run_locked_with_events(
            &CodexWorkspace {
                working_directory: workspace_path.clone(),
            },
            existing_thread_id,
            input,
            |event| {
                let preview = preview.clone();
                async move {
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
            let _ = status_sync::refresh_thread_topic_title(bot, state, &record).await;
            return Err(error);
        }
    };
    record_bot_status_event(
        &workspace_path,
        "bot_turn_completed",
        Some(existing_thread_id),
        None,
        Some(&result.final_response),
    )
    .await?;
    let record = state
        .repository
        .mark_session_binding_verified(record)
        .await?;
    let preview_finalized = preview.lock().await.complete(&result.final_response).await;
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
        result_text: if result.final_response.trim().is_empty() {
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
        } else {
            result.final_response.trim().to_owned()
        },
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
    if !preview_finalized {
        send_final_assistant_reply(bot, &record, Some(thread_id), &artifact.result_text).await?;
    }
    dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
    Ok(())
}
