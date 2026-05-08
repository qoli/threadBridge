use std::path::{Path, PathBuf};

use anyhow::Result;
use teloxide::payloads::setters::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ThreadId};
use tracing::{debug, warn};

use super::{Bot, ChatId, Requester, TelegramSystemIntent, ThreadRecord, format_system_text};
use crate::repository::ThreadRepository;
use crate::thread_state::{BindingStatus, resolve_binding_status};

const TELEGRAM_TOPIC_TITLE_MAX_CHARS: usize = 128;
pub(crate) const CALLBACK_TUI_ADOPT_ACCEPT: &str = "tui_adopt_accept";
pub(crate) const CALLBACK_TUI_ADOPT_REJECT: &str = "tui_adopt_reject";

fn thread_id_from_i32(value: i32) -> ThreadId {
    ThreadId(MessageId(value))
}

fn workspace_basename(workspace_path: Option<&Path>) -> Option<String> {
    workspace_path
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn truncate_topic_base(base: &str, suffix: &str) -> String {
    let suffix_len = suffix.chars().count();
    if suffix_len >= TELEGRAM_TOPIC_TITLE_MAX_CHARS {
        return suffix
            .chars()
            .take(TELEGRAM_TOPIC_TITLE_MAX_CHARS)
            .collect::<String>();
    }
    let max_base_len = TELEGRAM_TOPIC_TITLE_MAX_CHARS - suffix_len;
    let base_len = base.chars().count();
    if base_len <= max_base_len {
        return format!("{base}{suffix}");
    }
    let ellipsis = "...";
    let keep_len = max_base_len.saturating_sub(ellipsis.chars().count());
    let mut truncated = base.chars().take(keep_len).collect::<String>();
    truncated.push_str(ellipsis);
    format!("{truncated}{suffix}")
}

pub(crate) fn render_topic_title(
    record: &ThreadRecord,
    workspace_path: Option<&Path>,
    broken: bool,
) -> String {
    let base = record
        .metadata
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| workspace_basename(workspace_path))
        .unwrap_or_else(|| "Unbound".to_owned());

    let mut suffix = String::new();
    if broken {
        suffix.push_str(" · broken");
    }

    truncate_topic_base(&base, &suffix)
}

pub(crate) fn topic_title_suffix_label(broken: bool) -> &'static str {
    if broken { "broken" } else { "none" }
}

pub(crate) fn tui_adoption_prompt_text() -> String {
    format_system_text(TelegramSystemIntent::Question, "後續對話是否以 TUI session")
}

pub(crate) fn tui_adoption_prompt_markup(thread_key: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            "繼續 TUI 對話 (默認)",
            format!("{CALLBACK_TUI_ADOPT_ACCEPT}:{thread_key}"),
        ),
        InlineKeyboardButton::callback(
            "恢復原對話",
            format!("{CALLBACK_TUI_ADOPT_REJECT}:{thread_key}"),
        ),
    ]])
}

pub(crate) async fn refresh_thread_topic_title(
    bot: &Bot,
    repository: &ThreadRepository,
    record: &ThreadRecord,
    source: &'static str,
) -> Result<()> {
    let Some(message_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    let session = repository.read_session_binding(record).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let binding_status = resolve_binding_status(&record.metadata, session.as_ref());
    let title = render_topic_title(
        record,
        workspace_path.as_deref(),
        binding_status == BindingStatus::Broken,
    );
    apply_thread_topic_title(
        bot,
        record,
        workspace_path.as_deref(),
        message_thread_id,
        &title,
        source,
    )
    .await
}

async fn apply_thread_topic_title(
    bot: &Bot,
    record: &ThreadRecord,
    workspace_path: Option<&Path>,
    message_thread_id: i32,
    title: &str,
    source: &'static str,
) -> Result<()> {
    match bot
        .edit_forum_topic(
            ChatId(record.metadata.chat_id),
            thread_id_from_i32(message_thread_id),
        )
        .name(title.to_owned())
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            if is_telegram_topic_management_unavailable_error_text(&error.to_string()) {
                debug!(
                    event = "telegram.topic_title.refresh_skipped",
                    source = source,
                    thread_key = %record.metadata.thread_key,
                    chat_id = record.metadata.chat_id,
                    message_thread_id,
                    error = %error,
                    "Telegram topic management is unavailable for this chat; skipping title refresh"
                );
                return Ok(());
            }
            let workspace = workspace_path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unbound".to_owned());
            warn!(
                event = "telegram.topic_title.refresh_failed",
                source = source,
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id,
                workspace = %workspace,
                stored_title = record.metadata.title.as_deref().unwrap_or(""),
                desired_title = %title,
                session_broken = record.metadata.session_broken,
                error = %error,
                "failed to update Telegram forum topic title"
            );
            Err(error.into())
        }
    }
}

fn is_telegram_topic_management_unavailable_error_text(error_text: &str) -> bool {
    error_text.contains("Bad Request: BOT_FORUM_CREATE_FORBIDDEN")
        || error_text.contains("Bad Request: message thread not found")
}

#[cfg(test)]
mod tests {
    use super::is_telegram_topic_management_unavailable_error_text;

    #[test]
    fn topic_management_unavailable_matches_threaded_mode_create_forbidden() {
        assert!(is_telegram_topic_management_unavailable_error_text(
            "A Telegram's error: Unknown error: \"Bad Request: BOT_FORUM_CREATE_FORBIDDEN\""
        ));
    }

    #[test]
    fn topic_management_unavailable_matches_stale_topic() {
        assert!(is_telegram_topic_management_unavailable_error_text(
            "A Telegram's error: Unknown error: \"Bad Request: message thread not found\""
        ));
    }

    #[test]
    fn topic_management_unavailable_rejects_unrelated_error() {
        assert!(!is_telegram_topic_management_unavailable_error_text(
            "A Telegram's error: Unknown error: \"Too Many Requests\""
        ));
    }
}
