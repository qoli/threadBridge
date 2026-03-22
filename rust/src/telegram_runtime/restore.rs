use anyhow::Result;
use teloxide::payloads::setters::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup};

use super::*;
use crate::thread_state::{LifecycleStatus, resolve_lifecycle_status};

pub(crate) const CALLBACK_RESTORE_PICK: &str = "restore_pick";
pub(crate) const CALLBACK_RESTORE_PAGE: &str = "restore_page";
const RESTORE_PAGE_SIZE: usize = 8;

fn restored_thread_title(title: Option<&str>, fallback_thread_id: Option<i32>) -> String {
    let base = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Thread {}", fallback_thread_id.unwrap_or_default()));
    format!("{base} · 已恢復")
}

pub(crate) async fn render_restore_page(
    state: &AppState,
    chat_id: i64,
    offset: usize,
) -> Result<(String, InlineKeyboardMarkup)> {
    let archived = state.repository.list_archived_threads(chat_id).await?;
    if archived.is_empty() {
        return Ok((
            format_system_text(
                TelegramSystemIntent::Info,
                "No archived threads are available.",
            ),
            InlineKeyboardMarkup::default(),
        ));
    }

    let slice = archived.iter().skip(offset).take(RESTORE_PAGE_SIZE);
    let mut lines = Vec::new();
    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    for record in slice {
        let label = record.metadata.title.clone().unwrap_or_else(|| {
            format!(
                "Thread {}",
                record.metadata.message_thread_id.unwrap_or_default()
            )
        });
        lines.push(format!("- {} [{}]", label, record.metadata.thread_key));
        keyboard.push(vec![InlineKeyboardButton::callback(
            format!("Restore: {}", label),
            format!(
                "{CALLBACK_RESTORE_PICK}:{}:{offset}",
                record.metadata.thread_key
            ),
        )]);
    }
    let mut pagination = Vec::new();
    if offset > 0 {
        pagination.push(InlineKeyboardButton::callback(
            "Previous",
            format!(
                "{CALLBACK_RESTORE_PAGE}:{}",
                offset.saturating_sub(RESTORE_PAGE_SIZE)
            ),
        ));
    }
    if offset + RESTORE_PAGE_SIZE < archived.len() {
        pagination.push(InlineKeyboardButton::callback(
            "Next",
            format!("{CALLBACK_RESTORE_PAGE}:{}", offset + RESTORE_PAGE_SIZE),
        ));
    }
    if !pagination.is_empty() {
        keyboard.push(pagination);
    }
    Ok((
        format_system_text(
            TelegramSystemIntent::Info,
            &format!(
                "Archived threads:\n{}\n\nChoose one to restore.",
                lines.join("\n")
            ),
        ),
        InlineKeyboardMarkup::new(keyboard),
    ))
}

pub(crate) async fn restore_thread(
    bot: &Bot,
    message: &Message,
    query: &CallbackQuery,
    state: &AppState,
    thread_key: &str,
    offset: usize,
) -> Result<()> {
    let Some(thread_record) = state
        .repository
        .get_thread_by_key(message.chat.id.0, thread_key)
        .await?
    else {
        bot.answer_callback_query(query.id.clone())
            .text("That archived thread binding no longer exists.")
            .await?;
        return Ok(());
    };

    if !matches!(
        resolve_lifecycle_status(&thread_record.metadata),
        LifecycleStatus::Archived
    ) {
        bot.answer_callback_query(query.id.clone())
            .text("That thread binding is already active.")
            .await?;
        return Ok(());
    }

    let topic = bot
        .create_forum_topic(
            message.chat.id,
            restored_thread_title(
                thread_record.metadata.title.as_deref(),
                thread_record.metadata.message_thread_id,
            ),
        )
        .await?;
    let restored_record = state
        .repository
        .restore_thread(
            thread_record,
            thread_id_to_i32(topic.thread_id),
            topic.name.clone(),
        )
        .await?;
    state
        .repository
        .append_log(
            &restored_record,
            LogDirection::System,
            format!(
                "Thread restored from archive into Telegram thread \"{}\" (message_thread_id {}).",
                topic.name,
                thread_id_to_i32(topic.thread_id)
            ),
            Some(query.from.id.0 as i64),
        )
        .await?;
    bot.answer_callback_query(query.id.clone())
        .text("Thread restored.")
        .await?;
    send_scoped_message(
        bot,
        message.chat.id,
        None,
        format!("Restored into \"{}\". Continue there.", topic.name),
    )
    .await?;
    send_scoped_message(
        bot,
        message.chat.id,
        Some(topic.thread_id),
        "This thread has been restored from archive.",
    )
    .await?;
    let (text, markup) = render_restore_page(state, message.chat.id.0, offset).await?;
    bot.edit_message_text(message.chat.id, message.id, text)
        .link_preview_options(disabled_link_preview_options())
        .reply_markup(markup)
        .await?;
    Ok(())
}
