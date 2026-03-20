use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ThreadId};
use tracing::{info, warn};

use super::*;
use crate::repository::{
    ThreadStatus, TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
    TranscriptMirrorRole,
};
use crate::workspace_status::{
    CliOwnerClaim, SessionCurrentStatus, SessionStatusOwner, WorkspaceAggregateStatus,
    WorkspaceStatusEventRecord, events_path, read_cli_owner_claim, read_session_status,
    record_bot_status_event,
};

const TELEGRAM_TOPIC_TITLE_MAX_CHARS: usize = 128;
const STARTUP_STALE_BUSY_RECOVERED_LOG: &str =
    "Recovered stale busy state from previous threadBridge process during startup.";
pub(crate) const CALLBACK_TUI_ADOPT_ACCEPT: &str = "tui_adopt_accept";
pub(crate) const CALLBACK_TUI_ADOPT_REJECT: &str = "tui_adopt_reject";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaleBusyReconciliationReport {
    pub scanned_threads: usize,
    pub unique_sessions: usize,
    pub recovered_sessions: usize,
    pub recovered_threads: usize,
    pub skipped_threads: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliTopicMarker {
    None,
    Busy,
}

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

pub(crate) fn topic_marker_for_snapshot(snapshot: Option<&SessionCurrentStatus>) -> CliTopicMarker {
    if snapshot.is_some_and(|snapshot| snapshot.phase.is_turn_busy()) {
        CliTopicMarker::Busy
    } else {
        CliTopicMarker::None
    }
}

fn workspace_cli_conflict(
    aggregate: Option<&WorkspaceAggregateStatus>,
    owner_claim: Option<&CliOwnerClaim>,
) -> bool {
    let Some(aggregate) = aggregate else {
        return false;
    };
    if aggregate.live_cli_session_ids.is_empty() {
        return false;
    }
    let Some(owner_claim) = owner_claim else {
        return true;
    };
    if aggregate.live_cli_session_ids.len() > 1 {
        return true;
    }
    let Some(expected_session_id) = owner_claim.session_id.as_deref() else {
        return false;
    };
    aggregate
        .live_cli_session_ids
        .iter()
        .all(|item| item != expected_session_id)
}

pub(crate) fn render_topic_title(
    record: &ThreadRecord,
    workspace_path: Option<&Path>,
    marker: CliTopicMarker,
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
    match marker {
        CliTopicMarker::Busy => suffix.push_str(" · busy"),
        CliTopicMarker::None => {}
    }
    if record.metadata.session_broken {
        suffix.push_str(" · broken");
    }

    truncate_topic_base(&base, &suffix)
}

pub(crate) fn cli_marker_label(marker: CliTopicMarker) -> &'static str {
    match marker {
        CliTopicMarker::None => "none",
        CliTopicMarker::Busy => "busy",
    }
}

pub(crate) fn tui_adoption_prompt_text() -> &'static str {
    "詢問：後續對話是否以 TUI session"
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
    state: &AppState,
    record: &ThreadRecord,
    source: &'static str,
) -> Result<()> {
    let Some(message_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    let session = state.repository.read_session_binding(record).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let current_snapshot = if let Some(path) = workspace_path.as_ref() {
        let _ = read_workspace_status_with_cache(&state.workspace_status_cache, path).await?;
        let current_session_id = usable_bound_session_id(session.as_ref());
        if let Some(current_session_id) = current_session_id {
            read_session_status(path, current_session_id).await?
        } else {
            None
        }
    } else {
        None
    };
    let title = render_topic_title(
        record,
        workspace_path.as_deref(),
        topic_marker_for_snapshot(current_snapshot.as_ref()),
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

pub(crate) fn busy_text_message(
    snapshot: &SessionCurrentStatus,
    image_saved: bool,
) -> &'static str {
    match snapshot.owner {
        SessionStatusOwner::Cli if image_saved => {
            "Image saved. Analysis will stay pending until the shared TUI session finishes its current turn."
        }
        SessionStatusOwner::Cli => {
            "The shared TUI session is already running a turn. Wait for it to finish before sending a new Telegram request."
        }
        SessionStatusOwner::Bot => {
            "This thread's current Codex session is already handling another Telegram request. Wait for it to finish before sending a new one."
        }
    }
}

pub(crate) fn busy_command_message(snapshot: &SessionCurrentStatus) -> &'static str {
    match snapshot.owner {
        SessionStatusOwner::Cli => {
            "The shared TUI session is already running a turn. Wait for it to finish before changing this thread's session state."
        }
        SessionStatusOwner::Bot => {
            "This thread's current Codex session is already handling another Telegram request. Wait for it to finish before changing session state."
        }
    }
}

fn session_reconciliation_key(workspace_path: &Path, session_id: &str) -> String {
    let workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string();
    format!("{workspace}::{session_id}")
}

fn should_recover_stale_bot_busy(snapshot: &SessionCurrentStatus) -> bool {
    snapshot.owner == SessionStatusOwner::Bot && snapshot.phase.is_turn_busy()
}

pub async fn reconcile_stale_bot_busy_sessions(
    state: &AppState,
) -> Result<StaleBusyReconciliationReport> {
    reconcile_stale_bot_busy_sessions_for_repository(&state.repository).await
}

async fn reconcile_stale_bot_busy_sessions_for_repository(
    repository: &ThreadRepository,
) -> Result<StaleBusyReconciliationReport> {
    let records = repository.list_active_threads().await?;
    let mut report = StaleBusyReconciliationReport::default();
    let mut recovery_by_session: HashMap<String, bool> = HashMap::new();

    for record in records {
        report.scanned_threads += 1;
        let Some(binding) = repository.read_session_binding(&record).await? else {
            report.skipped_threads += 1;
            continue;
        };
        let Some(session_id) = usable_bound_session_id(Some(&binding)) else {
            report.skipped_threads += 1;
            continue;
        };
        let Some(workspace_cwd) = binding.workspace_cwd.as_deref() else {
            report.skipped_threads += 1;
            continue;
        };

        let workspace_path = PathBuf::from(workspace_cwd);
        let session_key = session_reconciliation_key(&workspace_path, session_id);
        let recovered = if let Some(known) = recovery_by_session.get(&session_key).copied() {
            known
        } else {
            report.unique_sessions += 1;
            let recovered =
                if let Some(snapshot) = read_session_status(&workspace_path, session_id).await? {
                    if should_recover_stale_bot_busy(&snapshot) {
                        info!(
                            event = "workspace_status.reconcile_stale_bot_busy.recovered",
                            thread_key = %record.metadata.thread_key,
                            workspace = %workspace_path.display(),
                            session_id,
                            previous_phase = ?snapshot.phase,
                            previous_turn_id = snapshot.turn_id.as_deref().unwrap_or(""),
                            "recovered stale bot-owned busy session snapshot"
                        );
                        record_bot_status_event(
                            &workspace_path,
                            "bot_turn_recovered",
                            Some(session_id),
                            snapshot.turn_id.as_deref(),
                            None,
                        )
                        .await?;
                        report.recovered_sessions += 1;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
            recovery_by_session.insert(session_key.clone(), recovered);
            recovered
        };

        if recovered {
            repository
                .append_log(
                    &record,
                    LogDirection::System,
                    STARTUP_STALE_BUSY_RECOVERED_LOG,
                    None,
                )
                .await?;
            report.recovered_threads += 1;
        } else {
            report.skipped_threads += 1;
        }
    }

    Ok(report)
}

pub async fn spawn_workspace_status_watcher(bot: Bot, state: AppState) {
    tokio::spawn(async move {
        let mut applied_titles: HashMap<String, String> = HashMap::new();
        let mut workspace_event_offsets: HashMap<String, usize> = HashMap::new();
        let mut pending_cli_user_prompts: HashSet<String> = HashSet::new();
        loop {
            if let Err(error) = sync_workspace_titles_once(&bot, &state, &mut applied_titles).await
            {
                warn!(event = "workspace_status.sync.failed", error = %error);
            }
            if let Err(error) = sync_cli_transcript_mirrors_once(
                &bot,
                &state,
                &mut workspace_event_offsets,
                &mut pending_cli_user_prompts,
            )
            .await
            {
                warn!(event = "workspace_mirror.sync.failed", error = %error);
            }
            if let Err(error) = sync_tui_adoption_prompts_once(&bot, &state).await {
                warn!(event = "tui_adoption.sync.failed", error = %error);
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

        let session = state.repository.read_session_binding(&record).await?;
        let workspace_path = session
            .as_ref()
            .and_then(|binding| binding.workspace_cwd.as_deref())
            .map(PathBuf::from);

        let _aggregate = if let Some(workspace_path) = workspace_path.as_ref() {
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
        let current_snapshot = if let (Some(workspace_path), Some(session_id)) =
            (workspace_path.as_ref(), usable_bound_session_id(session.as_ref()))
        {
            read_session_status(workspace_path, session_id).await?
        } else {
            None
        };
        let marker = topic_marker_for_snapshot(current_snapshot.as_ref());
        let rendered = render_topic_title(&record, workspace_path.as_deref(), marker);
        let previous = applied_titles.get(&record.conversation_key);
        if previous.is_some_and(|value| value == &rendered) {
            continue;
        }

        apply_thread_topic_title(
            bot,
            &record,
            workspace_path.as_deref(),
            message_thread_id,
            &rendered,
            "workspace_status_sync",
        )
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

async fn sync_tui_adoption_prompts_once(bot: &Bot, state: &AppState) -> Result<()> {
    let records = state.repository.list_active_threads().await?;
    for record in records {
        let Some(thread_id) = record.metadata.message_thread_id else {
            continue;
        };
        let Some(binding) = state.repository.read_session_binding(&record).await? else {
            continue;
        };
        if !binding.tui_session_adoption_pending
            || binding.tui_session_adoption_prompt_message_id.is_some()
        {
            continue;
        }
        let message = bot
            .send_message(
                ChatId(record.metadata.chat_id),
                tui_adoption_prompt_text().to_owned(),
            )
            .message_thread_id(thread_id_from_i32(thread_id))
            .reply_markup(tui_adoption_prompt_markup(&record.metadata.thread_key))
            .await?;
        let _ = state
            .repository
            .set_tui_adoption_prompt_message_id(record, message.id.0)
            .await?;
    }
    Ok(())
}

async fn sync_cli_transcript_mirrors_once(
    bot: &Bot,
    state: &AppState,
    workspace_event_offsets: &mut HashMap<String, usize>,
    pending_cli_user_prompts: &mut HashSet<String>,
) -> Result<()> {
    let records = state.repository.list_active_threads().await?;
    let mut by_workspace: HashMap<String, Vec<ThreadRecord>> = HashMap::new();
    for record in records {
        if matches!(record.metadata.status, ThreadStatus::Archived) {
            continue;
        }
        let Some(binding) = state.repository.read_session_binding(&record).await? else {
            continue;
        };
        let Some(workspace_cwd) = binding.workspace_cwd else {
            continue;
        };
        by_workspace.entry(workspace_cwd).or_default().push(record);
    }

    for (workspace_key, workspace_records) in by_workspace {
        let workspace_path = PathBuf::from(&workspace_key);
        let Some(owner_claim) = read_cli_owner_claim(&workspace_path).await? else {
            pending_cli_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        };
        let aggregate =
            crate::workspace_status::read_workspace_aggregate_status(&workspace_path).await?;
        if workspace_cli_conflict(Some(&aggregate), Some(&owner_claim)) {
            pending_cli_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        }
        let Some(owner_record) = workspace_records
            .iter()
            .find(|record| record.metadata.thread_key == owner_claim.thread_key)
            .cloned()
        else {
            pending_cli_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        };

        let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
            continue;
        };
        let Some(previous_offset) = workspace_event_offsets.get(&workspace_key).copied() else {
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        };
        let new_offset = lines.len();
        for line in lines.iter().skip(previous_offset) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let event: WorkspaceStatusEventRecord = match serde_json::from_str(trimmed) {
                Ok(event) => event,
                Err(error) => {
                    warn!(event = "workspace_mirror.event_parse_failed", error = %error);
                    continue;
                }
            };
            match event.event.as_str() {
                "user_prompt_submitted" => {
                    let Some(session_id) = event
                        .payload
                        .get("session_id")
                        .and_then(|value| value.as_str())
                    else {
                        warn!(
                            event = "workspace_mirror.cli_user_prompt_missing_session",
                            workspace = %workspace_key,
                            thread_key = %owner_record.metadata.thread_key,
                        );
                        continue;
                    };
                    if owner_claim
                        .session_id
                        .as_deref()
                        .is_some_and(|expected| expected != session_id)
                    {
                        continue;
                    }
                    let Some(entry) =
                        cli_mirror_entry_from_event(&event, owner_claim.session_id.as_deref())
                    else {
                        warn!(
                            event = "workspace_mirror.cli_user_prompt_missing_text",
                            workspace = %workspace_key,
                            thread_key = %owner_record.metadata.thread_key,
                            session_id = session_id,
                        );
                        continue;
                    };
                    pending_cli_user_prompts
                        .insert(cli_prompt_tracking_key(&workspace_key, &entry.session_id));
                    state
                        .repository
                        .append_transcript_mirror(&owner_record, &entry)
                        .await?;
                    if let Some(message_thread_id) = owner_record.metadata.message_thread_id {
                        send_scoped_message(
                            bot,
                            ChatId(owner_record.metadata.chat_id),
                            Some(thread_id_from_i32(message_thread_id)),
                            format!("CLI: {}", entry.text),
                        )
                        .await?;
                    }
                    continue;
                }
                "turn_completed" => {
                    if let Some(session_id) = event
                        .payload
                        .get("thread-id")
                        .and_then(|value| value.as_str())
                    {
                        if owner_claim
                            .session_id
                            .as_deref()
                            .is_none_or(|expected| expected == session_id)
                            && !pending_cli_user_prompts
                                .remove(&cli_prompt_tracking_key(&workspace_key, session_id))
                        {
                            warn!(
                                event = "workspace_mirror.cli_user_prompt_missing",
                                workspace = %workspace_key,
                                thread_key = %owner_record.metadata.thread_key,
                                session_id = session_id,
                            );
                        }
                    }
                }
                _ => {}
            }
            if let Some(entry) =
                cli_mirror_entry_from_event(&event, owner_claim.session_id.as_deref())
            {
                state
                    .repository
                    .append_transcript_mirror(&owner_record, &entry)
                    .await?;
                if let Some(message_thread_id) = owner_record.metadata.message_thread_id {
                    let prefix = match (entry.origin.clone(), entry.role.clone()) {
                        (TranscriptMirrorOrigin::Cli, TranscriptMirrorRole::User) => "CLI",
                        (TranscriptMirrorOrigin::Cli, TranscriptMirrorRole::Assistant) => "Codex",
                        _ => continue,
                    };
                    send_scoped_message(
                        bot,
                        ChatId(owner_record.metadata.chat_id),
                        Some(thread_id_from_i32(message_thread_id)),
                        format!("{prefix}: {}", entry.text),
                    )
                    .await?;
                }
            }
        }
        workspace_event_offsets.insert(workspace_key, new_offset);
    }
    Ok(())
}

fn cli_prompt_tracking_key(workspace_key: &str, session_id: &str) -> String {
    format!("{workspace_key}::{session_id}")
}

async fn read_workspace_event_lines(workspace_path: &Path) -> Result<Option<Vec<String>>> {
    let path = events_path(workspace_path);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(content.lines().map(str::to_owned).collect()))
}

fn cli_mirror_entry_from_event(
    event: &WorkspaceStatusEventRecord,
    expected_session_id: Option<&str>,
) -> Option<TranscriptMirrorEntry> {
    match event.event.as_str() {
        "user_prompt_submitted" => {
            let session_id = event.payload.get("session_id")?.as_str()?;
            if expected_session_id.is_some_and(|expected| expected != session_id) {
                return None;
            }
            let text = event.payload.get("prompt")?.as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            Some(TranscriptMirrorEntry {
                timestamp: event.occurred_at.clone(),
                session_id: session_id.to_owned(),
                origin: TranscriptMirrorOrigin::Cli,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                text: text.to_owned(),
            })
        }
        "turn_completed" => {
            let session_id = event.payload.get("thread-id")?.as_str()?;
            if expected_session_id.is_some_and(|expected| expected != session_id) {
                return None;
            }
            let text = event
                .payload
                .get("last-assistant-message")?
                .as_str()?
                .trim();
            if text.is_empty() {
                return None;
            }
            Some(TranscriptMirrorEntry {
                timestamp: event.occurred_at.clone(),
                session_id: session_id.to_owned(),
                origin: TranscriptMirrorOrigin::Cli,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                text: text.to_owned(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliTopicMarker, STARTUP_STALE_BUSY_RECOVERED_LOG, cli_mirror_entry_from_event,
        reconcile_stale_bot_busy_sessions_for_repository, render_topic_title,
        topic_marker_for_snapshot,
    };
    use crate::repository::{
        ThreadMetadata, ThreadRecord, ThreadRepository, ThreadScope, ThreadStatus,
        TranscriptMirrorOrigin, TranscriptMirrorRole,
    };
    use crate::workspace_status::{
        SessionCurrentStatus, SessionStatusOwner, WorkspaceStatusEventRecord,
        WorkspaceStatusPhase, ensure_workspace_status_surface, read_session_status,
        record_bot_status_event, session_status_path,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-status-sync-test-{}", Uuid::new_v4()))
    }

    async fn setup_repo_and_workspace(
        workspace_name: &str,
    ) -> (ThreadRepository, PathBuf, PathBuf, ThreadRecord) {
        let root = temp_path();
        let data_root = root.join("data");
        let workspace = root.join(workspace_name);
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&data_root).await.unwrap();
        let record = repo
            .create_thread(1, 100, "status".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_test".to_owned(),
            )
            .await
            .unwrap();
        (repo, root, workspace, record)
    }

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

    #[test]
    fn render_title_uses_busy_suffix_for_running_snapshot() {
        let snapshot = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".to_owned(),
            session_id: "thr_bot".to_owned(),
            owner: SessionStatusOwner::Bot,
            live: false,
            phase: WorkspaceStatusPhase::TurnRunning,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            summary: Some("hello".to_owned()),
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        let title = render_topic_title(
            &record(None, false),
            Some(PathBuf::from("/tmp/example-workspace").as_path()),
            topic_marker_for_snapshot(Some(&snapshot)),
        );
        assert_eq!(title, "example-workspace · busy");
    }

    #[test]
    fn render_title_truncates_before_busy_suffix() {
        let long_title = "x".repeat(140);
        let title = render_topic_title(
            &record(Some(&long_title), false),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            CliTopicMarker::Busy,
        );
        assert!(title.ends_with(" · busy"));
        assert!(title.chars().count() <= 128);
    }

    #[test]
    fn render_title_includes_broken_and_busy() {
        let snapshot = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".to_owned(),
            session_id: "thr_bot".to_owned(),
            owner: SessionStatusOwner::Bot,
            live: false,
            phase: WorkspaceStatusPhase::TurnRunning,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            summary: None,
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        let title = render_topic_title(
            &record(Some("Broken"), true),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            topic_marker_for_snapshot(Some(&snapshot)),
        );
        assert_eq!(title, "Broken · busy · broken");
    }

    #[test]
    fn cli_user_prompt_event_creates_cli_user_entry() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "user_prompt_submitted".to_owned(),
            source: crate::workspace_status::SessionStatusOwner::Cli,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "session_id": "thr_cli",
                "prompt": "inspect this repo"
            }),
        };
        let entry = cli_mirror_entry_from_event(&event, Some("thr_cli")).expect("cli user entry");
        assert_eq!(entry.origin, TranscriptMirrorOrigin::Cli);
        assert_eq!(entry.role, TranscriptMirrorRole::User);
        assert_eq!(entry.text, "inspect this repo");
    }

    #[test]
    fn cli_user_prompt_event_without_prompt_is_ignored() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "user_prompt_submitted".to_owned(),
            source: crate::workspace_status::SessionStatusOwner::Cli,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "session_id": "thr_cli"
            }),
        };
        assert!(cli_mirror_entry_from_event(&event, Some("thr_cli")).is_none());
    }

    #[test]
    fn turn_completed_does_not_fallback_to_input_messages_for_cli_user_entry() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "turn_completed".to_owned(),
            source: crate::workspace_status::SessionStatusOwner::Cli,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "thread-id": "thr_cli",
                "input-messages": ["hello from cli"]
            }),
        };
        assert!(cli_mirror_entry_from_event(&event, Some("thr_cli")).is_none());
    }

    #[tokio::test]
    async fn startup_reconciliation_recovers_bot_busy_session() {
        let (repo, root, workspace, record) = setup_repo_and_workspace("workspace").await;
        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_test"),
            Some("turn-1"),
            Some("hello"),
        )
        .await
        .unwrap();

        let report = reconcile_stale_bot_busy_sessions_for_repository(&repo)
            .await
            .unwrap();
        assert_eq!(report.scanned_threads, 1);
        assert_eq!(report.unique_sessions, 1);
        assert_eq!(report.recovered_sessions, 1);
        assert_eq!(report.recovered_threads, 1);
        assert_eq!(report.skipped_threads, 0);

        let session = read_session_status(&workspace, "thr_test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.owner, SessionStatusOwner::Bot);
        assert_eq!(session.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(session.turn_id, None);

        let log = fs::read_to_string(&record.log_path).await.unwrap();
        assert!(log.contains(STARTUP_STALE_BUSY_RECOVERED_LOG));

        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn startup_reconciliation_does_not_recover_cli_busy_session() {
        let (repo, root, workspace, record) = setup_repo_and_workspace("workspace").await;
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let cli_session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_test".to_owned(),
            owner: SessionStatusOwner::Cli,
            live: true,
            phase: WorkspaceStatusPhase::TurnRunning,
            shell_pid: Some(42),
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("codex-cli".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            summary: Some("cli run".to_owned()),
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(&workspace, "thr_test"),
            format!("{}\n", serde_json::to_string_pretty(&cli_session).unwrap()),
        )
        .await
        .unwrap();

        let report = reconcile_stale_bot_busy_sessions_for_repository(&repo)
            .await
            .unwrap();
        assert_eq!(report.recovered_sessions, 0);
        assert_eq!(report.recovered_threads, 0);
        assert_eq!(report.skipped_threads, 1);

        let session = read_session_status(&workspace, "thr_test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.owner, SessionStatusOwner::Cli);
        assert_eq!(session.phase, WorkspaceStatusPhase::TurnRunning);

        let log = fs::read_to_string(&record.log_path)
            .await
            .unwrap_or_default();
        assert!(!log.contains(STARTUP_STALE_BUSY_RECOVERED_LOG));

        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn startup_reconciliation_recovers_shared_session_once_and_logs_all_threads() {
        let root = temp_path();
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&data_root).await.unwrap();

        let record_a = repo.create_thread(1, 100, "A".to_owned()).await.unwrap();
        let record_a = repo
            .bind_workspace(
                record_a,
                workspace.display().to_string(),
                "thr_shared".to_owned(),
            )
            .await
            .unwrap();
        let record_b = repo.create_thread(1, 101, "B".to_owned()).await.unwrap();
        let record_b = repo
            .bind_workspace(
                record_b,
                workspace.display().to_string(),
                "thr_shared".to_owned(),
            )
            .await
            .unwrap();

        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_shared"),
            Some("turn-1"),
            Some("pending"),
        )
        .await
        .unwrap();

        let report = reconcile_stale_bot_busy_sessions_for_repository(&repo)
            .await
            .unwrap();
        assert_eq!(report.scanned_threads, 2);
        assert_eq!(report.unique_sessions, 1);
        assert_eq!(report.recovered_sessions, 1);
        assert_eq!(report.recovered_threads, 2);
        assert_eq!(report.skipped_threads, 0);

        let log_a = fs::read_to_string(&record_a.log_path).await.unwrap();
        let log_b = fs::read_to_string(&record_b.log_path).await.unwrap();
        assert!(log_a.contains(STARTUP_STALE_BUSY_RECOVERED_LOG));
        assert!(log_b.contains(STARTUP_STALE_BUSY_RECOVERED_LOG));

        let _ = fs::remove_dir_all(root).await;
    }
}
