use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};

use crate::repository::{LogDirection, ThreadRecord};
use crate::telegram_runtime::{
    AppState, ensure_bound_workspace_runtime, prepare_workspace_runtime_for_control, status_sync,
    thread_id_to_i32,
};
use crate::workspace::ensure_workspace_runtime;

#[derive(Clone)]
pub struct LocalControlHandle {
    bot: Bot,
    state: AppState,
}

#[derive(Debug, Clone)]
pub struct CreatedThread {
    pub record: ThreadRecord,
    pub title: String,
}

impl LocalControlHandle {
    pub fn new(bot: Bot, state: AppState) -> Self {
        Self { bot, state }
    }

    pub fn app_state(&self) -> &AppState {
        &self.state
    }

    pub async fn create_thread(&self, title: Option<String>) -> Result<CreatedThread> {
        let main_thread = self
            .state
            .repository
            .find_main_thread()
            .await?
            .context("Control chat is not ready yet. Send /start to the bot from the target Telegram chat first.")?;
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Thread {}", chrono::Local::now().format("%m-%d %H:%M")));
        let topic = self
            .bot
            .create_forum_topic(ChatId(main_thread.metadata.chat_id), title.clone())
            .await?;
        let record = self
            .state
            .repository
            .create_thread(
                main_thread.metadata.chat_id,
                thread_id_to_i32(topic.thread_id),
                topic.name.clone(),
            )
            .await?;
        self.state
            .repository
            .append_log(
                &record,
                LogDirection::System,
                "Telegram thread created from local management UI.",
                None,
            )
            .await?;
        Ok(CreatedThread {
            record,
            title: topic.name,
        })
    }

    pub async fn create_thread_and_bind(
        &self,
        title: Option<String>,
        workspace_cwd: &str,
    ) -> Result<ThreadRecord> {
        let created = self.create_thread(title).await?;
        self.bind_workspace(&created.record.metadata.thread_key, workspace_cwd)
            .await
    }

    pub async fn bind_workspace(
        &self,
        thread_key: &str,
        workspace_cwd: &str,
    ) -> Result<ThreadRecord> {
        let record = self
            .state
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")?;
        let workspace_path = resolve_workspace_argument(workspace_cwd).await?;
        let conflicting_threads = self
            .state
            .repository
            .find_active_threads_by_workspace(&workspace_path.display().to_string())
            .await?;
        let has_conflict = conflicting_threads
            .iter()
            .any(|candidate| candidate.metadata.thread_key != record.metadata.thread_key);
        if has_conflict {
            bail!(
                "Workspace bind failed: another active thread is already bound to `{}`.",
                workspace_path.display()
            );
        }

        ensure_workspace_runtime(
            &self.state.config.runtime.codex_working_directory,
            &self.state.config.runtime.data_root_path,
            &self.state.seed_template_path,
            &workspace_path,
        )
        .await?;
        let codex_workspace =
            prepare_workspace_runtime_for_control(&self.state, workspace_path.clone()).await?;
        let binding = self.state.codex.start_thread(&codex_workspace).await?;
        let updated = self
            .state
            .repository
            .bind_workspace(record, binding.cwd, binding.thread_id)
            .await?;
        self.state
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                format!(
                    "Bound Telegram thread to workspace {} from local management UI.",
                    workspace_path.display()
                ),
                None,
            )
            .await?;
        Ok(updated)
    }

    pub async fn reconnect_codex(&self, thread_key: &str) -> Result<ThreadRecord> {
        let record = self
            .state
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")?;
        let session = self.state.repository.read_session_binding(&record).await?;
        let Some(binding) = session.as_ref() else {
            bail!("This thread is not bound to a workspace yet.");
        };
        let existing_thread_id = binding
            .current_codex_thread_id
            .as_deref()
            .filter(|_| !binding.session_broken)
            .context("This thread is missing a usable Codex thread id. Use Launch New first.")?;
        let workspace_path = ensure_bound_workspace_runtime(&self.state, binding).await?;
        let codex_workspace =
            prepare_workspace_runtime_for_control(&self.state, workspace_path).await?;
        match self
            .state
            .codex
            .reconnect_session(&codex_workspace, existing_thread_id)
            .await
        {
            Ok(()) => {
                let updated = self
                    .state
                    .repository
                    .mark_session_binding_verified(record)
                    .await?;
                self.state
                    .repository
                    .append_log(
                        &updated,
                        LogDirection::System,
                        "Codex session revalidated from local management UI.",
                        None,
                    )
                    .await?;
                Ok(updated)
            }
            Err(error) => {
                let updated = self
                    .state
                    .repository
                    .mark_session_binding_broken(record, error.to_string())
                    .await?;
                self.state
                    .repository
                    .append_log(
                        &updated,
                        LogDirection::System,
                        format!(
                            "Codex session revalidation failed from local management UI: {error}"
                        ),
                        None,
                    )
                    .await?;
                Err(error)
            }
        }
    }

    pub async fn archive_thread(&self, thread_key: &str) -> Result<ThreadRecord> {
        let record = self
            .state
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")?;
        if let Some(thread_id) = record.metadata.message_thread_id {
            let _ = self
                .bot
                .delete_forum_topic(
                    ChatId(record.metadata.chat_id),
                    ThreadId(MessageId(thread_id)),
                )
                .await;
        }
        let archived = self.state.repository.archive_thread(record).await?;
        self.state
            .repository
            .append_log(
                &archived,
                LogDirection::System,
                "Thread archived from local management UI.",
                None,
            )
            .await?;
        Ok(archived)
    }

    pub async fn adopt_tui_session(&self, thread_key: &str) -> Result<ThreadRecord> {
        let record = self
            .state
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")?;
        let updated = self
            .state
            .repository
            .adopt_tui_active_session(record)
            .await?;
        self.state
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                "Adopted the active TUI session from local management UI.",
                None,
            )
            .await?;
        let _ = status_sync::refresh_thread_topic_title(
            &self.bot,
            &self.state,
            &updated,
            "local_tui_adopt_accept",
        )
        .await;
        Ok(updated)
    }

    pub async fn reject_tui_session(&self, thread_key: &str) -> Result<ThreadRecord> {
        let record = self
            .state
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")?;
        let updated = self
            .state
            .repository
            .clear_tui_adoption_state(record)
            .await?;
        self.state
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                "Rejected the active TUI session from local management UI.",
                None,
            )
            .await?;
        let _ = status_sync::refresh_thread_topic_title(
            &self.bot,
            &self.state,
            &updated,
            "local_tui_adopt_reject",
        )
        .await;
        Ok(updated)
    }

    pub async fn restore_thread(&self, thread_key: &str) -> Result<ThreadRecord> {
        let archived = self
            .state
            .repository
            .get_thread_by_key(self.control_chat_id().await?, thread_key)
            .await?
            .context("thread_key is not a known thread")?;
        if !matches!(
            archived.metadata.status,
            crate::repository::ThreadStatus::Archived
        ) {
            bail!("thread_key is not archived");
        }
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
        let restored = self
            .state
            .repository
            .restore_thread(
                archived,
                thread_id_to_i32(topic.thread_id),
                topic.name.clone(),
            )
            .await?;
        self.state
            .repository
            .append_log(
                &restored,
                LogDirection::System,
                format!(
                    "Thread restored from local management UI into Telegram thread \"{}\" (message_thread_id {}).",
                    topic.name,
                    thread_id_to_i32(topic.thread_id)
                ),
                None,
            )
            .await?;
        Ok(restored)
    }

    async fn control_chat_id(&self) -> Result<i64> {
        Ok(self
            .state
            .repository
            .find_main_thread()
            .await?
            .context("Control chat is not ready yet. Send /start to the bot from the target Telegram chat first.")?
            .metadata
            .chat_id)
    }
}

async fn resolve_workspace_argument(raw: &str) -> Result<PathBuf> {
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

fn restored_thread_title(title: Option<&str>, fallback_thread_id: Option<i32>) -> String {
    let base = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Thread {}", fallback_thread_id.unwrap_or_default()));
    format!("{base} · 已恢復")
}
