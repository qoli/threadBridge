use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ThreadId};
use tracing::{info, warn};

use super::preview::TurnPreviewController;
use super::*;
use crate::repository::{
    ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
    TranscriptMirrorPhase, TranscriptMirrorRole,
};
use crate::thread_state::{
    BindingStatus, LifecycleStatus, resolve_binding_status, resolve_lifecycle_status,
};
use crate::workspace_status::{
    LocalTuiSessionClaim, SessionActivitySource, SessionCurrentStatus, WorkspaceAggregateStatus,
    WorkspaceStatusEventRecord, events_path, is_hcodex_ingress_client,
    read_local_tui_session_claim, read_session_status, record_bot_status_event,
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

fn workspace_local_conflict(
    aggregate: Option<&WorkspaceAggregateStatus>,
    local_tui_claim: Option<&LocalTuiSessionClaim>,
) -> bool {
    let Some(aggregate) = aggregate else {
        return false;
    };
    if aggregate.live_tui_session_ids.is_empty() {
        return false;
    }
    let Some(local_tui_claim) = local_tui_claim else {
        return true;
    };
    if aggregate.live_tui_session_ids.len() > 1 {
        return true;
    }
    let Some(expected_session_id) = local_tui_claim.session_id.as_deref() else {
        return false;
    };
    aggregate
        .live_tui_session_ids
        .iter()
        .all(|item| item != expected_session_id)
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
    match snapshot.activity_source {
        SessionActivitySource::Tui if image_saved => {
            "Image saved. Analysis will stay pending until the shared TUI session finishes its current turn. Use /stop if you want to interrupt it."
        }
        SessionActivitySource::Tui => {
            "The shared TUI session is already running a turn. Wait for it to finish before sending a new Telegram request, or use /stop to interrupt it."
        }
        SessionActivitySource::ManagedRuntime => {
            "This thread's current Codex session is already handling another Telegram request. Wait for it to finish before sending a new one, or use /stop to interrupt it."
        }
    }
}

pub(crate) fn busy_command_message(snapshot: &SessionCurrentStatus) -> &'static str {
    match snapshot.activity_source {
        SessionActivitySource::Tui => {
            "The shared TUI session is already running a turn. Wait for it to finish before changing this thread's session state, or use /stop to interrupt it."
        }
        SessionActivitySource::ManagedRuntime => {
            "This thread's current Codex session is already handling another Telegram request. Wait for it to finish before changing session state, or use /stop to interrupt it."
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
    snapshot.activity_source == SessionActivitySource::ManagedRuntime
        && snapshot.phase.is_turn_busy()
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
        let binding_status = resolve_binding_status(&record.metadata, Some(&binding));
        let Some(session_id) = usable_bound_session_id(
            crate::thread_state::ResolvedThreadState {
                lifecycle_status: resolve_lifecycle_status(&record.metadata),
                binding_status,
                run_status: crate::thread_state::RunStatus::Idle,
            },
            Some(&binding),
        ) else {
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
        let mut pending_local_user_prompts: HashSet<String> = HashSet::new();
        let mut mirror_previews: HashMap<String, TurnPreviewController> = HashMap::new();
        loop {
            if let Err(error) = sync_workspace_titles_once(&bot, &state, &mut applied_titles).await
            {
                warn!(event = "workspace_status.sync.failed", error = %error);
            }
            if let Err(error) = sync_local_transcript_mirrors_once(
                &bot,
                &state,
                &mut workspace_event_offsets,
                &mut pending_local_user_prompts,
                &mut mirror_previews,
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
        let binding_status = resolve_binding_status(&record.metadata, session.as_ref());
        let rendered = render_topic_title(
            &record,
            workspace_path.as_deref(),
            binding_status == BindingStatus::Broken,
        );
        let previous = applied_titles.get(&record.conversation_key);
        if previous.is_some_and(|value| value == &rendered) {
            continue;
        }

        if let Err(error) = apply_thread_topic_title(
            bot,
            &record,
            workspace_path.as_deref(),
            message_thread_id,
            &rendered,
            "workspace_status_sync",
        )
        .await
        {
            warn!(
                event = "workspace_status.sync.title_apply_failed",
                thread_key = %record.metadata.thread_key,
                conversation_key = %record.conversation_key,
                error = %error,
                "failed to sync topic title for one thread; continuing"
            );
            continue;
        }
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
            .send_message(ChatId(record.metadata.chat_id), tui_adoption_prompt_text())
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

async fn sync_local_transcript_mirrors_once(
    bot: &Bot,
    state: &AppState,
    workspace_event_offsets: &mut HashMap<String, usize>,
    pending_local_user_prompts: &mut HashSet<String>,
    mirror_previews: &mut HashMap<String, TurnPreviewController>,
) -> Result<()> {
    let records = state.repository.list_active_threads().await?;
    let mut by_workspace: HashMap<String, Vec<ThreadRecord>> = HashMap::new();
    let mut active_thread_keys = HashSet::new();
    for record in records {
        if matches!(
            resolve_lifecycle_status(&record.metadata),
            LifecycleStatus::Archived
        ) {
            continue;
        }
        active_thread_keys.insert(record.metadata.thread_key.clone());
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
        let Some(local_tui_claim) = read_local_tui_session_claim(&workspace_path).await? else {
            pending_local_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        };
        let aggregate =
            crate::workspace_status::read_workspace_aggregate_status(&workspace_path).await?;
        if workspace_local_conflict(Some(&aggregate), Some(&local_tui_claim)) {
            pending_local_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        }
        let Some(owner_record) = workspace_records
            .iter()
            .find(|record| record.metadata.thread_key == local_tui_claim.thread_key)
            .cloned()
        else {
            pending_local_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
                continue;
            };
            workspace_event_offsets.insert(workspace_key.clone(), lines.len());
            continue;
        };
        let Some(message_thread_id) = owner_record.metadata.message_thread_id else {
            continue;
        };

        let Some(lines) = read_workspace_event_lines(&workspace_path).await? else {
            continue;
        };
        let previous_offset = match workspace_event_offsets.get(&workspace_key).copied() {
            Some(offset) => offset,
            None => initial_workspace_event_offset(&lines, &local_tui_claim),
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
                            event = "workspace_mirror.local_user_prompt_missing_session",
                            workspace = %workspace_key,
                            thread_key = %owner_record.metadata.thread_key,
                        );
                        continue;
                    };
                    if local_tui_claim
                        .session_id
                        .as_deref()
                        .is_some_and(|expected| expected != session_id)
                    {
                        continue;
                    }
                    let Some(entry) = local_mirror_entry_from_event(
                        &event,
                        local_tui_claim.session_id.as_deref(),
                    ) else {
                        warn!(
                            event = "workspace_mirror.local_user_prompt_missing_text",
                            workspace = %workspace_key,
                            thread_key = %owner_record.metadata.thread_key,
                            session_id = session_id,
                        );
                        continue;
                    };
                    pending_local_user_prompts
                        .insert(local_prompt_tracking_key(&workspace_key, &entry.session_id));
                    ensure_mirror_preview(
                        mirror_previews,
                        bot,
                        state,
                        &owner_record,
                        message_thread_id,
                    )
                    .reset_for_new_turn();
                    let inserted = state
                        .repository
                        .append_transcript_mirror(&owner_record, &entry)
                        .await?;
                    if inserted
                        && entry.delivery == TranscriptMirrorDelivery::Final
                        && let Some(message_thread_id) = owner_record.metadata.message_thread_id
                    {
                        send_scoped_role_message(
                            bot,
                            ChatId(owner_record.metadata.chat_id),
                            Some(thread_id_from_i32(message_thread_id)),
                            TelegramTextRole::User,
                            &entry.text,
                        )
                        .await?;
                    }
                    continue;
                }
                "preview_text" => {
                    let Some(session_id) = event
                        .payload
                        .get("session_id")
                        .and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    if local_tui_claim
                        .session_id
                        .as_deref()
                        .is_some_and(|expected| expected != session_id)
                    {
                        continue;
                    }
                    let Some(text) = event.payload.get("text").and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    ensure_mirror_preview(
                        mirror_previews,
                        bot,
                        state,
                        &owner_record,
                        message_thread_id,
                    )
                    .consume_preview_text(text)
                    .await;
                    continue;
                }
                "turn_completed" => {
                    if let Some(session_id) = event
                        .payload
                        .get("thread-id")
                        .and_then(|value| value.as_str())
                    {
                        if local_tui_claim
                            .session_id
                            .as_deref()
                            .is_none_or(|expected| expected == session_id)
                            && !pending_local_user_prompts
                                .remove(&local_prompt_tracking_key(&workspace_key, session_id))
                        {
                            warn!(
                                event = "workspace_mirror.local_user_prompt_missing",
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
                local_mirror_entry_from_event(&event, local_tui_claim.session_id.as_deref())
            {
                let inserted = state
                    .repository
                    .append_transcript_mirror(&owner_record, &entry)
                    .await?;
                if inserted
                    && entry.delivery == TranscriptMirrorDelivery::Final
                    && let Some(message_thread_id) = owner_record.metadata.message_thread_id
                {
                    ensure_mirror_preview(
                        mirror_previews,
                        bot,
                        state,
                        &owner_record,
                        message_thread_id,
                    )
                    .complete(&entry.text)
                    .await;
                    super::final_reply::send_final_assistant_reply(
                        bot,
                        &owner_record,
                        Some(thread_id_from_i32(message_thread_id)),
                        &entry.text,
                    )
                    .await?;
                }
            }
        }
        workspace_event_offsets.insert(workspace_key, new_offset);
    }
    mirror_previews.retain(|thread_key, _| active_thread_keys.contains(thread_key));
    Ok(())
}

fn ensure_mirror_preview<'a>(
    mirror_previews: &'a mut HashMap<String, TurnPreviewController>,
    bot: &Bot,
    state: &AppState,
    owner_record: &ThreadRecord,
    message_thread_id: i32,
) -> &'a mut TurnPreviewController {
    mirror_previews
        .entry(owner_record.metadata.thread_key.clone())
        .or_insert_with(|| {
            TurnPreviewController::new(
                bot.clone(),
                ChatId(owner_record.metadata.chat_id),
                Some(thread_id_from_i32(message_thread_id)),
                state.config.stream_message_max_chars,
                state.config.command_output_tail_chars,
                state.config.stream_edit_interval_ms,
            )
        })
}

fn local_prompt_tracking_key(workspace_key: &str, session_id: &str) -> String {
    format!("{workspace_key}::{session_id}")
}

fn initial_workspace_event_offset(
    lines: &[String],
    local_tui_claim: &LocalTuiSessionClaim,
) -> usize {
    lines
        .iter()
        .position(|line| {
            serde_json::from_str::<WorkspaceStatusEventRecord>(line)
                .ok()
                .is_some_and(|event| event.occurred_at >= local_tui_claim.started_at)
        })
        .unwrap_or(lines.len())
}

fn transcript_origin_from_event(event: &WorkspaceStatusEventRecord) -> TranscriptMirrorOrigin {
    if is_hcodex_ingress_client(event.payload.get("client").and_then(Value::as_str)) {
        TranscriptMirrorOrigin::Tui
    } else {
        TranscriptMirrorOrigin::Local
    }
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

fn local_mirror_entry_from_event(
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
                origin: transcript_origin_from_event(event),
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
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
                origin: transcript_origin_from_event(event),
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: text.to_owned(),
            })
        }
        "process_transcript" => {
            let session_id = event.payload.get("session_id")?.as_str()?;
            if expected_session_id.is_some_and(|expected| expected != session_id) {
                return None;
            }
            let phase = match event.payload.get("phase")?.as_str()? {
                "plan" => TranscriptMirrorPhase::Plan,
                "tool" => TranscriptMirrorPhase::Tool,
                _ => return None,
            };
            let text = event.payload.get("text")?.as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            Some(TranscriptMirrorEntry {
                timestamp: event.occurred_at.clone(),
                session_id: session_id.to_owned(),
                origin: transcript_origin_from_event(event),
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Process,
                phase: Some(phase),
                text: text.to_owned(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        STARTUP_STALE_BUSY_RECOVERED_LOG, initial_workspace_event_offset,
        local_mirror_entry_from_event, reconcile_stale_bot_busy_sessions_for_repository,
        render_topic_title, topic_title_suffix_label, tui_adoption_prompt_text,
    };
    use crate::repository::{
        SessionBinding, ThreadMetadata, ThreadRecord, ThreadRepository, ThreadScope, ThreadStatus,
        TranscriptMirrorOrigin, TranscriptMirrorRole,
    };
    use crate::thread_state::effective_busy_snapshot_for_binding;
    use crate::workspace_status::{
        LocalTuiSessionClaim, SessionActivitySource, SessionCurrentStatus,
        WorkspaceStatusEventRecord, WorkspaceStatusPhase, ensure_workspace_status_surface,
        is_hcodex_ingress_client, read_session_status, record_bot_status_event,
        session_status_path,
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
                crate::execution_mode::SessionExecutionSnapshot::from_mode(
                    crate::execution_mode::ExecutionMode::FullAuto,
                ),
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
    fn workspace_status_helper_recognizes_hcodex_ingress_client_aliases() {
        assert!(is_hcodex_ingress_client(Some(
            "threadbridge-hcodex-ingress"
        )));
        assert!(is_hcodex_ingress_client(Some("threadbridge-tui-proxy")));
        assert!(!is_hcodex_ingress_client(Some("codex-cli")));
        assert!(!is_hcodex_ingress_client(None));
    }

    #[test]
    fn render_title_uses_plain_base_for_healthy_thread() {
        let title = render_topic_title(
            &record(None, false),
            Some(PathBuf::from("/tmp/example-workspace").as_path()),
            false,
        );
        assert_eq!(title, "example-workspace");
    }

    #[test]
    fn tui_adoption_prompt_uses_question_header_without_prefix() {
        assert_eq!(
            tui_adoption_prompt_text(),
            "Question: 後續對話是否以 TUI session"
        );
    }

    #[test]
    fn render_title_truncates_before_broken_suffix() {
        let long_title = "x".repeat(140);
        let title = render_topic_title(
            &record(Some(&long_title), false),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            true,
        );
        assert!(title.ends_with(" · broken"));
        assert!(title.chars().count() <= 128);
    }

    #[test]
    fn render_title_includes_broken_suffix_only() {
        let title = render_topic_title(
            &record(Some("Broken"), true),
            Some(PathBuf::from("/tmp/workspace").as_path()),
            true,
        );
        assert_eq!(title, "Broken · broken");
    }

    #[test]
    fn title_suffix_label_reports_broken_only() {
        assert_eq!(topic_title_suffix_label(false), "none");
        assert_eq!(topic_title_suffix_label(true), "broken");
    }

    #[test]
    fn local_user_prompt_event_creates_local_user_entry() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "user_prompt_submitted".to_owned(),
            source: crate::workspace_status::SessionActivitySource::Tui,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "session_id": "thr_cli",
                "prompt": "inspect this repo"
            }),
        };
        let entry =
            local_mirror_entry_from_event(&event, Some("thr_cli")).expect("local user entry");
        assert_eq!(entry.origin, TranscriptMirrorOrigin::Local);
        assert_eq!(entry.role, TranscriptMirrorRole::User);
        assert_eq!(entry.text, "inspect this repo");
    }

    #[test]
    fn tui_user_prompt_event_creates_tui_user_entry() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "user_prompt_submitted".to_owned(),
            source: crate::workspace_status::SessionActivitySource::Tui,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "session_id": "thr_tui",
                "prompt": "continue the tui session",
                "client": "threadbridge-hcodex-ingress"
            }),
        };
        let entry = local_mirror_entry_from_event(&event, Some("thr_tui")).expect("tui user entry");
        assert_eq!(entry.origin, TranscriptMirrorOrigin::Tui);
        assert_eq!(entry.role, TranscriptMirrorRole::User);
        assert_eq!(entry.text, "continue the tui session");
    }

    #[test]
    fn initial_offset_starts_from_local_tui_claim_started_at() {
        let local_tui_claim = LocalTuiSessionClaim {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".to_owned(),
            thread_key: "thread-key".to_owned(),
            shell_pid: 42,
            session_id: Some("thr_tui".to_owned()),
            child_pid: Some(77),
            child_pgid: None,
            child_command: Some("codex --remote".to_owned()),
            started_at: "2026-03-19T00:00:10.000Z".to_owned(),
            updated_at: "2026-03-19T00:00:10.000Z".to_owned(),
        };
        let lines = vec![
            serde_json::to_string(&WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "user_prompt_submitted".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
                payload: json!({"session_id": "thr_old", "prompt": "old"}),
            })
            .unwrap(),
            serde_json::to_string(&WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "user_prompt_submitted".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:11.000Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "prompt": "new"}),
            })
            .unwrap(),
        ];

        assert_eq!(initial_workspace_event_offset(&lines, &local_tui_claim), 1);
    }

    #[test]
    fn local_user_prompt_event_without_prompt_is_ignored() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "user_prompt_submitted".to_owned(),
            source: crate::workspace_status::SessionActivitySource::Tui,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "session_id": "thr_cli"
            }),
        };
        assert!(local_mirror_entry_from_event(&event, Some("thr_cli")).is_none());
    }

    #[test]
    fn turn_completed_does_not_fallback_to_input_messages_for_local_user_entry() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "turn_completed".to_owned(),
            source: crate::workspace_status::SessionActivitySource::Tui,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "thread-id": "thr_cli",
                "input-messages": ["hello from cli"]
            }),
        };
        assert!(local_mirror_entry_from_event(&event, Some("thr_cli")).is_none());
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
        assert_eq!(
            session.activity_source,
            SessionActivitySource::ManagedRuntime
        );
        assert_eq!(session.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(session.turn_id, None);

        let log = fs::read_to_string(&record.log_path).await.unwrap();
        assert!(log.contains(STARTUP_STALE_BUSY_RECOVERED_LOG));

        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn startup_reconciliation_does_not_recover_local_busy_session() {
        let (repo, root, workspace, record) = setup_repo_and_workspace("workspace").await;
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let local_session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_test".to_owned(),
            activity_source: SessionActivitySource::Tui,
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
            format!(
                "{}\n",
                serde_json::to_string_pretty(&local_session).unwrap()
            ),
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
        assert_eq!(session.activity_source, SessionActivitySource::Tui);
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
                crate::execution_mode::SessionExecutionSnapshot::from_mode(
                    crate::execution_mode::ExecutionMode::FullAuto,
                ),
            )
            .await
            .unwrap();
        let record_b = repo.create_thread(1, 101, "B".to_owned()).await.unwrap();
        let record_b = repo
            .bind_workspace(
                record_b,
                workspace.display().to_string(),
                "thr_shared".to_owned(),
                crate::execution_mode::SessionExecutionSnapshot::from_mode(
                    crate::execution_mode::ExecutionMode::FullAuto,
                ),
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

    #[tokio::test]
    async fn effective_busy_snapshot_prefers_tui_when_current_is_idle() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let current = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_current".to_owned(),
            activity_source: SessionActivitySource::ManagedRuntime,
            live: false,
            phase: WorkspaceStatusPhase::Idle,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge".to_owned()),
            turn_id: None,
            summary: None,
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        let tui = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_tui".to_owned(),
            activity_source: SessionActivitySource::Tui,
            live: true,
            phase: WorkspaceStatusPhase::TurnRunning,
            shell_pid: Some(0),
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge-hcodex-ingress".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            summary: Some("prompt".to_owned()),
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(&workspace, "thr_current"),
            format!("{}\n", serde_json::to_string_pretty(&current).unwrap()),
        )
        .await
        .unwrap();
        fs::write(
            session_status_path(&workspace, "thr_tui"),
            format!("{}\n", serde_json::to_string_pretty(&tui).unwrap()),
        )
        .await
        .unwrap();

        let binding: SessionBinding = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "current_codex_thread_id": "thr_current",
            "workspace_cwd": workspace.display().to_string(),
            "bound_at": null,
            "initialized_at": null,
            "last_verified_at": null,
            "session_broken": false,
            "session_broken_at": null,
            "session_broken_reason": null,
            "tui_active_codex_thread_id": "thr_tui",
            "tui_session_adoption_pending": false,
            "tui_session_adoption_prompt_message_id": null,
            "updated_at": "2026-03-19T00:00:00.000Z"
        }))
        .unwrap();

        let snapshot = effective_busy_snapshot_for_binding(Some(&binding))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.session_id, "thr_tui");
        assert_eq!(snapshot.phase, WorkspaceStatusPhase::TurnRunning);
    }
}
