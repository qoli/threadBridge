use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};
use tracing::warn;

use super::*;
use crate::workspace_status::WorkspaceStatusSource;

const TELEGRAM_TOPIC_TITLE_MAX_CHARS: usize = 128;

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
    snapshot: Option<&crate::workspace_status::WorkspaceCurrentStatus>,
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
    if let Some(snapshot) = snapshot
        && !snapshot.phase.is_idle()
    {
        match snapshot.source {
            Some(WorkspaceStatusSource::Cli) => suffix.push_str(" · cli"),
            Some(WorkspaceStatusSource::Bot) => suffix.push_str(" · bot"),
            None => {}
        }
    }
    if record.metadata.session_broken {
        suffix.push_str(" · broken");
    }

    truncate_topic_base(&base, &suffix)
}

pub(crate) async fn refresh_thread_topic_title(
    bot: &Bot,
    state: &AppState,
    record: &ThreadRecord,
) -> Result<()> {
    let Some(message_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    let session = state.repository.read_session_binding(record).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let snapshot = if let Some(path) = workspace_path.as_ref() {
        Some(read_status_with_cache(&state.workspace_status_cache, path).await?)
    } else {
        None
    };
    let title = render_topic_title(record, workspace_path.as_deref(), snapshot.as_ref());
    bot.edit_forum_topic(
        ChatId(record.metadata.chat_id),
        thread_id_from_i32(message_thread_id),
    )
    .name(title)
    .await?;
    Ok(())
}

pub(crate) fn busy_text_message(
    snapshot: &crate::workspace_status::WorkspaceCurrentStatus,
    image_saved: bool,
) -> &'static str {
    match snapshot.source {
        Some(WorkspaceStatusSource::Cli) if image_saved => {
            "Image saved. Analysis will stay pending until local Codex CLI becomes idle."
        }
        Some(WorkspaceStatusSource::Cli) => {
            "Local Codex CLI is active in this workspace. Wait for it to finish before sending a new Telegram request."
        }
        Some(WorkspaceStatusSource::Bot) => {
            "This workspace is already handling another Telegram request. Wait for it to finish before sending a new one."
        }
        None => {
            "This workspace is currently busy. Wait for it to finish before sending a new request."
        }
    }
}

pub(crate) fn busy_command_message(
    snapshot: &crate::workspace_status::WorkspaceCurrentStatus,
) -> &'static str {
    match snapshot.source {
        Some(WorkspaceStatusSource::Cli) => {
            "Local Codex CLI is active in this workspace. Wait for it to finish before changing the Telegram session state."
        }
        Some(WorkspaceStatusSource::Bot) => {
            "This workspace is already handling another Telegram request. Wait for it to finish before changing the Telegram session state."
        }
        None => {
            "This workspace is currently busy. Wait for it to finish before changing the Telegram session state."
        }
    }
}

pub async fn spawn_workspace_status_watcher(bot: Bot, state: AppState) {
    tokio::spawn(async move {
        let mut applied_titles: HashMap<String, String> = HashMap::new();
        loop {
            if let Err(error) = sync_workspace_titles_once(&bot, &state, &mut applied_titles).await
            {
                warn!(event = "workspace_status.sync.failed", error = %error);
            }
            tokio::time::sleep(Duration::from_millis(
                state.config.workspace_status_poll_interval_ms,
            ))
            .await;
        }
    });
}

async fn sync_workspace_titles_once(
    bot: &Bot,
    state: &AppState,
    applied_titles: &mut HashMap<String, String>,
) -> Result<()> {
    let records = state.repository.list_active_threads().await?;
    let mut active_conversations = HashSet::new();
    let mut keep_workspaces = Vec::new();
    let mut snapshot_by_workspace: HashMap<
        String,
        crate::workspace_status::WorkspaceCurrentStatus,
    > = HashMap::new();

    for record in records {
        let Some(message_thread_id) = record.metadata.message_thread_id else {
            continue;
        };
        active_conversations.insert(record.conversation_key.clone());

        let session = state.repository.read_session_binding(&record).await?;
        let workspace_path = session
            .as_ref()
            .and_then(|binding| binding.workspace_cwd.as_deref())
            .map(PathBuf::from);

        let snapshot = if let Some(workspace_path) = workspace_path.as_ref() {
            let key = workspace_path
                .canonicalize()
                .unwrap_or_else(|_| workspace_path.clone())
                .display()
                .to_string();
            keep_workspaces.push(key.clone());
            if let Some(existing) = snapshot_by_workspace.get(&key) {
                Some(existing.clone())
            } else {
                let loaded = crate::workspace_status::read_current_status(workspace_path).await?;
                state.workspace_status_cache.insert(loaded.clone()).await;
                snapshot_by_workspace.insert(key, loaded.clone());
                Some(loaded)
            }
        } else {
            None
        };

        let rendered = render_topic_title(&record, workspace_path.as_deref(), snapshot.as_ref());
        let previous = applied_titles.get(&record.conversation_key);
        if previous.is_some_and(|value| value == &rendered) {
            continue;
        }

        bot.edit_forum_topic(
            ChatId(record.metadata.chat_id),
            thread_id_from_i32(message_thread_id),
        )
        .name(rendered.clone())
        .await?;
        applied_titles.insert(record.conversation_key.clone(), rendered);
    }

    applied_titles.retain(|conversation, _| active_conversations.contains(conversation));
    state
        .workspace_status_cache
        .remove_missing_workspaces(&keep_workspaces)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render_topic_title;
    use crate::repository::{ThreadMetadata, ThreadRecord, ThreadScope, ThreadStatus};
    use crate::workspace_status::{
        WorkspaceCurrentStatus, WorkspaceStatusPhase, WorkspaceStatusSource,
    };
    use std::path::PathBuf;

    fn record(title: Option<&str>, session_broken: bool) -> ThreadRecord {
        ThreadRecord {
            conversation_key: "thread:test".to_owned(),
            folder_name: "folder".to_owned(),
            folder_path: PathBuf::from("/tmp/folder"),
            log_path: PathBuf::from("/tmp/folder/conversations.jsonl"),
            metadata_path: PathBuf::from("/tmp/folder/metadata.json"),
            metadata: ThreadMetadata {
                archived_at: None,
                chat_id: 1,
                created_at: "2026-03-19T00:00:00.000Z".to_owned(),
                last_codex_turn_at: None,
                message_thread_id: Some(123),
                previous_message_thread_ids: Vec::new(),
                scope: ThreadScope::Thread,
                session_broken,
                session_broken_at: None,
                session_broken_reason: None,
                status: ThreadStatus::Active,
                title: title.map(str::to_owned),
                updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
                thread_key: "thread-key".to_owned(),
            },
        }
    }

    fn snapshot(
        source: WorkspaceStatusSource,
        phase: WorkspaceStatusPhase,
    ) -> WorkspaceCurrentStatus {
        WorkspaceCurrentStatus {
            schema_version: 1,
            workspace_cwd: "/tmp/workspace".to_owned(),
            source: Some(source),
            phase,
            shell_pid: Some(42),
            client: Some("codex-cli".to_owned()),
            session_id: None,
            turn_id: None,
            summary: None,
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn render_title_preserves_busy_and_broken_suffixes() {
        let title = render_topic_title(
            &record(Some("Status Sync"), true),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            Some(&snapshot(
                WorkspaceStatusSource::Cli,
                WorkspaceStatusPhase::TurnRunning,
            )),
        );
        assert_eq!(title, "Status Sync · cli · broken");
    }

    #[test]
    fn render_title_falls_back_to_workspace_basename() {
        let title = render_topic_title(
            &record(None, false),
            Some(PathBuf::from("/tmp/example-workspace").as_path()),
            None,
        );
        assert_eq!(title, "example-workspace");
    }
}
