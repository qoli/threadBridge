use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};

use crate::execution_mode::workspace_execution_mode;
use crate::repository::{LogDirection, SessionBinding, ThreadRecord};
use crate::telegram_runtime::{
    AppState, ensure_bound_workspace_runtime, prepare_workspace_runtime_for_control,
    send_scoped_message, status_sync, thread_id_to_i32,
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

#[derive(Debug, Clone)]
pub enum AddWorkspaceOutcome {
    Created(ThreadRecord),
    Existing(ThreadRecord),
}

impl AddWorkspaceOutcome {
    pub fn record(&self) -> &ThreadRecord {
        match self {
            Self::Created(record) | Self::Existing(record) => record,
        }
    }

    pub fn created(&self) -> bool {
        matches!(self, Self::Created(_))
    }
}

impl LocalControlHandle {
    pub fn new(bot: Bot, state: AppState) -> Self {
        Self { bot, state }
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
            "Thread created from local management UI.",
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

    pub async fn add_workspace(&self, workspace_cwd: &str) -> Result<AddWorkspaceOutcome> {
        let workspace_path = resolve_workspace_argument(workspace_cwd).await?;
        let canonical_workspace = workspace_path.display().to_string();
        let active_threads = self
            .state
            .repository
            .find_active_threads_by_workspace(&canonical_workspace)
            .await?;
        if active_threads.len() > 1 {
            bail!(
                "Workspace already has multiple active thread bindings: {}",
                canonical_workspace
            );
        }
        if let Some(record) = active_threads.into_iter().next() {
            return Ok(AddWorkspaceOutcome::Existing(record));
        }

        let title = workspace_thread_title(&workspace_path);
        let record = self
            .create_thread_and_bind(Some(title), &canonical_workspace)
            .await?;
        Ok(AddWorkspaceOutcome::Created(record))
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
        let execution_mode = workspace_execution_mode(&workspace_path).await?;
        let binding = self
            .state
            .codex
            .start_thread_with_mode(&codex_workspace, execution_mode)
            .await?;
        let updated = self
            .state
            .repository
            .bind_workspace(record, binding.cwd, binding.thread_id, binding.execution)
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
        if let Some(message_thread_id) = updated.metadata.message_thread_id {
            send_scoped_message(
                &self.bot,
                ChatId(updated.metadata.chat_id),
                Some(ThreadId(MessageId(message_thread_id))),
                format!(
                    "Bound workspace: `{}`\n\nFor the managed local TUI path in this workspace, run:\n`{}/.threadbridge/bin/hcodex`",
                    workspace_path.display(),
                    workspace_path.display()
                ),
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(
                &self.bot,
                &self.state,
                &updated,
                "local_bind_workspace",
            )
            .await;
        }
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
        let existing_thread_id = reconnect_target_thread_id(binding).context(
            "This workspace is missing a usable Codex session id. Use New Session first.",
        )?;
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
        send_scoped_message(
            &self.bot,
            ChatId(restored.metadata.chat_id),
            None,
            format!("Restored into \"{}\". Continue there.", topic.name),
        )
        .await?;
        send_scoped_message(
            &self.bot,
            ChatId(restored.metadata.chat_id),
            Some(topic.thread_id),
            "This thread has been restored from archive.",
        )
        .await?;
        let _ = status_sync::refresh_thread_topic_title(
            &self.bot,
            &self.state,
            &restored,
            "local_restore",
        )
        .await;
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

fn workspace_thread_title(workspace_path: &Path) -> String {
    workspace_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| workspace_path.display().to_string())
}

fn reconnect_target_thread_id(binding: &SessionBinding) -> Option<&str> {
    binding
        .current_codex_thread_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{reconnect_target_thread_id, workspace_thread_title};
    use crate::repository::SessionBinding;
    use std::path::Path;

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

    #[test]
    fn reconnect_target_thread_id_allows_broken_binding_with_current_id() {
        let binding: SessionBinding = serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "current_codex_thread_id": "thread-123",
            "workspace_cwd": "/tmp/workspace",
            "bound_at": null,
            "initialized_at": null,
            "last_verified_at": null,
            "session_broken": true,
            "session_broken_at": null,
            "session_broken_reason": "probe failed",
            "tui_active_codex_thread_id": null,
            "tui_session_adoption_pending": false,
            "tui_session_adoption_prompt_message_id": null,
            "updated_at": "2026-03-21T00:00:00.000Z"
        }))
        .expect("valid session binding fixture");
        assert_eq!(reconnect_target_thread_id(&binding), Some("thread-123"));
    }
}
