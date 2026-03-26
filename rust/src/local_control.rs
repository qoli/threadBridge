use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};

use crate::repository::{ThreadRecord, ThreadRepository, ThreadStatus};
use crate::telegram_runtime::{send_scoped_message, status_sync, thread_id_to_i32};

#[derive(Clone)]
pub struct TelegramControlBridgeHandle {
    bot: Bot,
    repository: ThreadRepository,
}

#[derive(Debug, Clone)]
pub struct CreatedTelegramThread {
    pub chat_id: i64,
    pub message_thread_id: i32,
    pub title: String,
}

impl TelegramControlBridgeHandle {
    pub fn new(bot: Bot, repository: ThreadRepository) -> Self {
        Self { bot, repository }
    }

    pub async fn create_workspace_thread(
        &self,
        title: Option<String>,
        origin: &str,
    ) -> Result<CreatedTelegramThread> {
        let main_thread = self
            .repository
            .find_main_thread()
            .await?
            .context(
                "Control chat is not ready yet. Send /start to the bot from the target Telegram chat first.",
            )?;
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Thread {}", chrono::Local::now().format("%m-%d %H:%M")));
        let topic = self
            .bot
            .create_forum_topic(ChatId(main_thread.metadata.chat_id), title)
            .await?;
        send_scoped_message(
            &self.bot,
            ChatId(main_thread.metadata.chat_id),
            None,
            format!("Created thread \"{}\".", topic.name),
        )
        .await?;
        send_scoped_message(
            &self.bot,
            ChatId(main_thread.metadata.chat_id),
            Some(topic.thread_id),
            format!("Thread created from {origin}."),
        )
        .await?;
        Ok(CreatedTelegramThread {
            chat_id: main_thread.metadata.chat_id,
            message_thread_id: thread_id_to_i32(topic.thread_id),
            title: topic.name,
        })
    }

    pub async fn notify_workspace_bound(
        &self,
        record: &ThreadRecord,
        workspace_path: &Path,
        source: &'static str,
    ) -> Result<()> {
        let Some(message_thread_id) = record.metadata.message_thread_id else {
            return Ok(());
        };
        send_scoped_message(
            &self.bot,
            ChatId(record.metadata.chat_id),
            Some(ThreadId(MessageId(message_thread_id))),
            format!(
                "Bound workspace: `{}`\n\nFor the managed local TUI path in this workspace, run:\n`{}/.threadbridge/bin/hcodex`",
                workspace_path.display(),
                workspace_path.display()
            ),
        )
        .await?;
        let _ = status_sync::refresh_thread_topic_title(&self.bot, &self.repository, record, source)
            .await;
        Ok(())
    }

    pub async fn refresh_thread_title(
        &self,
        record: &ThreadRecord,
        source: &'static str,
    ) -> Result<()> {
        let _ = status_sync::refresh_thread_topic_title(&self.bot, &self.repository, record, source)
            .await;
        Ok(())
    }

    pub async fn delete_thread_topic(&self, record: &ThreadRecord) -> Result<()> {
        let Some(thread_id) = record.metadata.message_thread_id else {
            return Ok(());
        };
        let _ = self
            .bot
            .delete_forum_topic(
                ChatId(record.metadata.chat_id),
                ThreadId(MessageId(thread_id)),
            )
            .await;
        Ok(())
    }

    pub async fn create_restored_thread(&self, archived: &ThreadRecord) -> Result<CreatedTelegramThread> {
        let topic = self
            .bot
            .create_forum_topic(
                ChatId(archived.metadata.chat_id),
                restored_thread_title(
                    archived.metadata.title.as_deref(),
                    archived.metadata.message_thread_id,
                ),
            )
            .await?;
        Ok(CreatedTelegramThread {
            chat_id: archived.metadata.chat_id,
            message_thread_id: thread_id_to_i32(topic.thread_id),
            title: topic.name,
        })
    }

    pub async fn notify_restored(&self, restored: &ThreadRecord, title: &str) -> Result<()> {
        let Some(message_thread_id) = restored.metadata.message_thread_id else {
            return Ok(());
        };
        send_scoped_message(
            &self.bot,
            ChatId(restored.metadata.chat_id),
            None,
            format!("Restored into \"{}\". Continue there.", title),
        )
        .await?;
        send_scoped_message(
            &self.bot,
            ChatId(restored.metadata.chat_id),
            Some(ThreadId(MessageId(message_thread_id))),
            "This thread has been restored from archive.",
        )
        .await?;
        let _ =
            status_sync::refresh_thread_topic_title(&self.bot, &self.repository, restored, "restore")
                .await;
        Ok(())
    }

    pub async fn control_chat_id(&self) -> Result<i64> {
        Ok(self
            .repository
            .find_main_thread()
            .await?
            .context(
                "Control chat is not ready yet. Send /start to the bot from the target Telegram chat first.",
            )?
            .metadata
            .chat_id)
    }
}

pub async fn resolve_workspace_argument(raw: &str) -> Result<PathBuf> {
    let input = PathBuf::from(raw.trim());
    if !input.is_absolute() {
        bail!("Workspace path must be absolute.");
    }
    let metadata = tokio::fs::metadata(&input)
        .await
        .with_context(|| format!("workspace path does not exist: {}", input.display()))?;
    if !metadata.is_dir() {
        bail!("Workspace path must point to a directory.");
    }
    Ok(input.canonicalize().unwrap_or(input))
}

pub fn restored_thread_title(title: Option<&str>, fallback_thread_id: Option<i32>) -> String {
    let base = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Thread {}", fallback_thread_id.unwrap_or_default()));
    format!("{base} · 已恢復")
}

pub fn ensure_archived_thread(record: ThreadRecord) -> Result<ThreadRecord> {
    if !matches!(record.metadata.status, ThreadStatus::Archived) {
        bail!("thread_key is not archived");
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::runtime_control::workspace_thread_title;

    #[test]
    fn workspace_thread_title_prefers_folder_name() {
        let title = workspace_thread_title(Path::new("/tmp/threadBridge/workspaces/Trackly"));
        assert_eq!(title, "Trackly");
    }

    #[test]
    fn workspace_thread_title_falls_back_to_full_path() {
        let title = workspace_thread_title(Path::new("/"));
        assert_eq!(title, "/");
    }
}
