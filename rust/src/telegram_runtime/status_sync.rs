use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};
use tracing::warn;

use super::*;
use crate::repository::SessionAttachmentState;
use crate::workspace_status::{SessionCurrentStatus, SessionStatusOwner, WorkspaceAggregateStatus};

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
    session: Option<&SessionBinding>,
    aggregate: Option<&WorkspaceAggregateStatus>,
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
    if session.is_some_and(|binding| binding.attachment_state == SessionAttachmentState::CliHandoff)
    {
        suffix.push_str(" · attach");
    } else if let Some(aggregate) = aggregate
        && !aggregate.live_cli_session_ids.is_empty()
    {
        suffix.push_str(" · cli");
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
    let aggregate = if let Some(path) = workspace_path.as_ref() {
        Some(read_workspace_status_with_cache(&state.workspace_status_cache, path).await?)
    } else {
        None
    };
    let title = render_topic_title(
        record,
        workspace_path.as_deref(),
        session.as_ref(),
        aggregate.as_ref(),
    );
    bot.edit_forum_topic(
        ChatId(record.metadata.chat_id),
        thread_id_from_i32(message_thread_id),
    )
    .name(title)
    .await?;
    Ok(())
}

pub(crate) fn busy_text_message(
    snapshot: &SessionCurrentStatus,
    image_saved: bool,
) -> &'static str {
    match snapshot.owner {
        SessionStatusOwner::Cli if image_saved => {
            "Image saved. Analysis will stay pending until the attached CLI session finishes its current turn."
        }
        SessionStatusOwner::Cli => {
            "The attached CLI session is already running a turn. Wait for it to finish before sending a new Telegram request."
        }
        SessionStatusOwner::Bot => {
            "This thread's selected Codex session is already handling another Telegram request. Wait for it to finish before sending a new one."
        }
    }
}

pub(crate) fn busy_command_message(snapshot: &SessionCurrentStatus) -> &'static str {
    match snapshot.owner {
        SessionStatusOwner::Cli => {
            "The attached CLI session is already running a turn. Wait for it to finish before changing this thread's session selection."
        }
        SessionStatusOwner::Bot => {
            "This thread's selected Codex session is already handling another Telegram request. Wait for it to finish before changing session state."
        }
    }
}

pub(crate) fn cli_owned_text_message(image_saved: bool) -> &'static str {
    if image_saved {
        "Image saved. Local Codex CLI currently owns this session. Run /attach_cli_session to take it over before starting analysis."
    } else {
        "Local Codex CLI currently owns this session. Run /attach_cli_session to take it over in Telegram."
    }
}

pub(crate) fn cli_owned_command_message() -> &'static str {
    "Local Codex CLI currently owns this session. Run /attach_cli_session to take it over before changing thread session state."
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
    let mut aggregate_by_workspace: HashMap<String, WorkspaceAggregateStatus> = HashMap::new();

    for record in records {
        let Some(message_thread_id) = record.metadata.message_thread_id else {
            continue;
        };
        active_conversations.insert(record.conversation_key.clone());

        let mut session = state.repository.read_session_binding(&record).await?;
        let workspace_path = session
            .as_ref()
            .and_then(|binding| binding.workspace_cwd.as_deref())
            .map(PathBuf::from);

        let aggregate = if let Some(workspace_path) = workspace_path.as_ref() {
            let key = workspace_path
                .canonicalize()
                .unwrap_or_else(|_| workspace_path.clone())
                .display()
                .to_string();
            keep_workspaces.push(key.clone());
            if let Some(existing) = aggregate_by_workspace.get(&key) {
                Some(existing.clone())
            } else {
                let loaded =
                    crate::workspace_status::read_workspace_aggregate_status(workspace_path)
                        .await?;
                state.workspace_status_cache.insert(loaded.clone()).await;
                aggregate_by_workspace.insert(key, loaded.clone());
                Some(loaded)
            }
        } else {
            None
        };

        if let (Some(binding), Some(aggregate)) = (session.as_ref(), aggregate.as_ref())
            && binding.attachment_state == SessionAttachmentState::CliHandoff
        {
            let selected_session_id = usable_bound_session_id(session.as_ref());
            if selected_session_id.is_some_and(|session_id| {
                aggregate
                    .live_cli_session_ids
                    .iter()
                    .any(|item| item == session_id)
            }) {
                let released = state
                    .repository
                    .clear_cli_handoff_attachment(record.clone())
                    .await?;
                session = state.repository.read_session_binding(&released).await?;
            }
        }

        let rendered = render_topic_title(
            &record,
            workspace_path.as_deref(),
            session.as_ref(),
            aggregate.as_ref(),
        );
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
    use crate::repository::{
        SessionAttachmentState, SessionBinding, ThreadMetadata, ThreadRecord, ThreadScope,
        ThreadStatus,
    };
    use crate::workspace_status::WorkspaceAggregateStatus;
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

    fn aggregate(session_ids: &[&str]) -> WorkspaceAggregateStatus {
        WorkspaceAggregateStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".to_owned(),
            live_cli_session_ids: session_ids
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            active_shell_pids: vec![42],
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        }
    }

    fn binding(
        selected_session_id: Option<&str>,
        attachment_state: SessionAttachmentState,
    ) -> SessionBinding {
        SessionBinding {
            schema_version: 2,
            codex_thread_id: selected_session_id.map(str::to_owned),
            selected_session_id: selected_session_id.map(str::to_owned),
            attachment_state,
            workspace_cwd: Some("/tmp/workspace".to_owned()),
            bound_at: None,
            initialized_at: None,
            last_verified_at: None,
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn render_title_uses_attach_for_cli_handoff_binding() {
        let title = render_topic_title(
            &record(Some("Status Sync"), true),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            Some(&binding(
                Some("thr_cli"),
                SessionAttachmentState::CliHandoff,
            )),
            Some(&aggregate(&["thr_cli"])),
        );
        assert_eq!(title, "Status Sync · attach · broken");
    }

    #[test]
    fn render_title_uses_cli_for_other_live_workspace_session() {
        let title = render_topic_title(
            &record(None, false),
            Some(PathBuf::from("/tmp/example-workspace").as_path()),
            Some(&binding(Some("thr_bot"), SessionAttachmentState::None)),
            Some(&aggregate(&["thr_cli"])),
        );
        assert_eq!(title, "example-workspace · cli");
    }

    #[test]
    fn render_title_truncates_before_attach_suffix() {
        let long_title = "x".repeat(140);
        let title = render_topic_title(
            &record(Some(&long_title), false),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            Some(&binding(
                Some("thr_cli"),
                SessionAttachmentState::CliHandoff,
            )),
            Some(&aggregate(&["thr_cli"])),
        );
        assert!(title.ends_with(" · attach"));
        assert!(title.chars().count() <= 128);
    }
}
