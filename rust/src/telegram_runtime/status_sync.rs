use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};
use tracing::warn;

#[allow(unused_imports)]
pub(crate) use super::busy_copy::{busy_command_message, busy_text_message};
use super::preview::TurnPreviewController;
#[allow(unused_imports)]
pub(crate) use super::title_sync::{
    CALLBACK_TUI_ADOPT_ACCEPT, CALLBACK_TUI_ADOPT_REJECT, refresh_thread_topic_title,
    render_topic_title, topic_title_suffix_label, tui_adoption_prompt_markup,
    tui_adoption_prompt_text,
};
use super::*;
use crate::delivery_bus::{
    ClaimStatus, DeliveryAttempt, DeliveryChannel, DeliveryClaim, DeliveryKind,
    provisional_key_for_text,
};
use crate::repository::{
    TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorPhase,
    TranscriptMirrorRole,
};
use crate::runtime_busy_reconcile::reconcile_stale_bot_busy_sessions as reconcile_stale_bot_busy_sessions_for_repository_owner;
#[allow(unused_imports)]
pub(crate) use crate::runtime_busy_reconcile::{
    STARTUP_STALE_BUSY_RECOVERED_LOG, StaleBusyReconciliationReport,
};
use crate::thread_state::{
    BindingStatus, LifecycleStatus, resolve_binding_status, resolve_lifecycle_status,
};
use crate::workspace_status::{
    LocalTuiSessionClaim, WorkspaceAggregateStatus, WorkspaceEventLogRead,
    WorkspaceStatusEventRecord, events_path, is_hcodex_ingress_client,
    read_local_tui_session_claim, read_workspace_event_log_repairing,
    record_tui_mirror_preview_sync,
};

struct MirrorPreviewState {
    preview: TurnPreviewController,
    active_turn_id: Option<String>,
    active_item_id: Option<String>,
    owns_active_turn: bool,
    latest_preview_text: String,
}

impl MirrorPreviewState {
    fn new(preview: TurnPreviewController) -> Self {
        Self {
            preview,
            active_turn_id: None,
            active_item_id: None,
            owns_active_turn: false,
            latest_preview_text: String::new(),
        }
    }

    fn reset_for_new_turn(&mut self) {
        self.preview.reset_for_new_turn();
        self.active_turn_id = None;
        self.active_item_id = None;
        self.owns_active_turn = false;
        self.latest_preview_text.clear();
    }

    fn begin_turn(&mut self, turn_id: Option<&str>) -> bool {
        if self.active_turn_id.as_deref() == turn_id {
            return false;
        }
        self.preview.reset_for_new_turn();
        self.active_turn_id = turn_id.map(str::to_owned);
        self.active_item_id = None;
        self.owns_active_turn = false;
        self.latest_preview_text.clear();
        true
    }

    fn set_ownership(&mut self, turn_id: Option<&str>, owns_active_turn: bool) {
        if self.active_turn_id.as_deref() != turn_id {
            return;
        }
        self.owns_active_turn = owns_active_turn;
    }

    fn owns_turn(&self, turn_id: Option<&str>) -> bool {
        self.owns_active_turn && self.active_turn_id.as_deref() == turn_id
    }

    fn should_skip_regressive_preview(
        &self,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        text: &str,
    ) -> bool {
        let text = text.trim();
        !text.is_empty()
            && self.active_turn_id.as_deref() == turn_id
            && self.active_item_id.as_deref() == item_id
            && self.latest_preview_text != text
            && self.latest_preview_text.starts_with(text)
    }

    async fn consume_preview_text(
        &mut self,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        text: &str,
    ) {
        let text = text.trim();
        if text.is_empty() || self.should_skip_regressive_preview(turn_id, item_id, text) {
            return;
        }
        self.active_item_id = item_id.map(str::to_owned);
        self.latest_preview_text.clear();
        self.latest_preview_text.push_str(text);
        self.preview
            .consume_preview_text_for_item(turn_id, item_id, text)
            .await;
    }
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

fn thread_id_from_i32(value: i32) -> ThreadId {
    ThreadId(MessageId(value))
}

pub async fn reconcile_stale_bot_busy_sessions(
    state: &AppState,
) -> Result<StaleBusyReconciliationReport> {
    reconcile_stale_bot_busy_sessions_for_repository_owner(&state.repository).await
}

#[cfg(test)]
async fn reconcile_stale_bot_busy_sessions_for_repository(
    repository: &crate::repository::ThreadRepository,
) -> Result<StaleBusyReconciliationReport> {
    reconcile_stale_bot_busy_sessions_for_repository_owner(repository).await
}

pub async fn spawn_workspace_status_watcher(bot: Bot, state: AppState) {
    tokio::spawn(async move {
        let mut applied_titles: HashMap<String, String> = HashMap::new();
        let mut workspace_event_offsets: HashMap<String, usize> = HashMap::new();
        let mut pending_local_user_prompts: HashSet<String> = HashSet::new();
        let mut mirror_previews: HashMap<String, MirrorPreviewState> = HashMap::new();
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
        if record.metadata.message_thread_id.is_none() {
            continue;
        }
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

        if let Err(error) =
            refresh_thread_topic_title(bot, &state.repository, &record, "workspace_status_sync")
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
    mirror_previews: &mut HashMap<String, MirrorPreviewState>,
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
            let Some(event_log) = read_workspace_event_log_repairing(&workspace_path).await? else {
                continue;
            };
            log_workspace_event_log_diagnostics(&workspace_path, &workspace_key, &event_log);
            workspace_event_offsets.insert(workspace_key.clone(), event_log.events.len());
            continue;
        };
        let aggregate =
            crate::workspace_status::read_workspace_aggregate_status(&workspace_path).await?;
        if workspace_local_conflict(Some(&aggregate), Some(&local_tui_claim)) {
            pending_local_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(event_log) = read_workspace_event_log_repairing(&workspace_path).await? else {
                continue;
            };
            log_workspace_event_log_diagnostics(&workspace_path, &workspace_key, &event_log);
            workspace_event_offsets.insert(workspace_key.clone(), event_log.events.len());
            continue;
        }
        let Some(owner_record) = workspace_records
            .iter()
            .find(|record| record.metadata.thread_key == local_tui_claim.thread_key)
            .cloned()
        else {
            pending_local_user_prompts.retain(|key| !key.starts_with(&workspace_key));
            let Some(event_log) = read_workspace_event_log_repairing(&workspace_path).await? else {
                continue;
            };
            log_workspace_event_log_diagnostics(&workspace_path, &workspace_key, &event_log);
            workspace_event_offsets.insert(workspace_key.clone(), event_log.events.len());
            continue;
        };
        let Some(message_thread_id) = owner_record.metadata.message_thread_id else {
            continue;
        };

        let Some(event_log) = read_workspace_event_log_repairing(&workspace_path).await? else {
            continue;
        };
        log_workspace_event_log_diagnostics(&workspace_path, &workspace_key, &event_log);
        let previous_offset = match workspace_event_offsets.get(&workspace_key).copied() {
            Some(offset) => offset,
            None => initial_workspace_event_offset(&event_log.events, &local_tui_claim),
        };
        let new_offset = event_log.events.len();
        let new_events = &event_log.events[previous_offset..new_offset];
        let mut event_index = 0usize;
        while event_index < new_events.len() {
            let mut event = &new_events[event_index];
            let mut next_event_index = event_index + 1;
            if event.event == "preview_text" {
                let run_end = preview_event_run_end(new_events, event_index);
                // TUI ingress emits cumulative preview snapshots; only the newest
                // preview in a consecutive run should be mirrored into Telegram.
                event = &new_events[run_end - 1];
                next_event_index = run_end;
            }
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
                    let Some(entry) =
                        local_mirror_entry_from_event(event, local_tui_claim.session_id.as_deref())
                    else {
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
                        let provisional_key = provisional_key_for_text(
                            &entry.session_id,
                            DeliveryKind::UserEcho,
                            &entry.text,
                            &event.occurred_at,
                        );
                        let claim = state
                            .control
                            .delivery_bus
                            .claim_delivery(DeliveryClaim {
                                thread_key: owner_record.metadata.thread_key.clone(),
                                session_id: entry.session_id.clone(),
                                turn_id: None,
                                provisional_key: Some(provisional_key.clone()),
                                channel: DeliveryChannel::Telegram,
                                kind: DeliveryKind::UserEcho,
                                owner: "status_sync".to_owned(),
                            })
                            .await?;
                        if matches!(claim, ClaimStatus::Claimed(_)) {
                            send_scoped_role_message(
                                bot,
                                ChatId(owner_record.metadata.chat_id),
                                Some(thread_id_from_i32(message_thread_id)),
                                TelegramTextRole::User,
                                &entry.text,
                            )
                            .await?;
                            let _ = state
                                .control
                                .delivery_bus
                                .commit_delivery(DeliveryAttempt {
                                    thread_key: owner_record.metadata.thread_key.clone(),
                                    session_id: entry.session_id.clone(),
                                    turn_id: None,
                                    provisional_key: Some(provisional_key),
                                    channel: DeliveryChannel::Telegram,
                                    kind: DeliveryKind::UserEcho,
                                    executor: "telegram_status_sync".to_owned(),
                                    transport_ref: None,
                                    report_json: serde_json::json!({
                                        "targets": [{
                                            "type": "telegram_user_echo",
                                            "target_ref": format!(
                                                "chat:{}/thread:{}",
                                                owner_record.metadata.chat_id,
                                                message_thread_id
                                            ),
                                            "state": "committed",
                                        }]
                                    }),
                                })
                                .await;
                        }
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
                    let turn_id = turn_id_from_event_payload(&event.payload);
                    let item_id = item_id_from_event_payload(&event.payload);
                    let preview = ensure_mirror_preview(
                        mirror_previews,
                        bot,
                        state,
                        &owner_record,
                        message_thread_id,
                    );
                    let previous_turn_id = preview.active_turn_id.clone();
                    let previous_item_id = preview.active_item_id.clone();
                    let previous_latest_preview_text = preview.latest_preview_text.clone();
                    let turn_transition = preview.begin_turn(turn_id.as_deref());
                    let item_transition = previous_item_id.as_deref() != item_id.as_deref();
                    let mut decision = "applied";
                    let mut claim_status = None;
                    if let Some(turn_id) = turn_id.as_deref() {
                        if !preview.owns_turn(Some(turn_id)) {
                            let claim = state
                                .control
                                .delivery_bus
                                .claim_delivery(DeliveryClaim {
                                    thread_key: owner_record.metadata.thread_key.clone(),
                                    session_id: session_id.to_owned(),
                                    turn_id: Some(turn_id.to_owned()),
                                    provisional_key: None,
                                    channel: DeliveryChannel::Telegram,
                                    kind: DeliveryKind::PreviewDraft,
                                    owner: "status_sync".to_owned(),
                                })
                                .await?;
                            claim_status = Some(match &claim {
                                ClaimStatus::Claimed(_) => "claimed",
                                ClaimStatus::Existing(_) => "existing",
                            });
                            preview.set_ownership(
                                Some(turn_id),
                                matches!(claim, ClaimStatus::Claimed(_)),
                            );
                        } else {
                            claim_status = Some("already_owned");
                        }
                        if !preview.owns_turn(Some(turn_id)) {
                            decision = "skipped_claim_denied";
                            record_tui_mirror_preview_sync(
                                &workspace_path,
                                session_id,
                                Some(turn_id),
                                item_id.as_deref(),
                                &event.occurred_at,
                                decision,
                                claim_status,
                                previous_turn_id.as_deref(),
                                preview.active_turn_id.as_deref(),
                                previous_item_id.as_deref(),
                                preview.active_item_id.as_deref(),
                                turn_transition,
                                item_transition,
                                preview.owns_active_turn,
                                text,
                                &previous_latest_preview_text,
                                preview.preview.draft_id(),
                            )
                            .await?;
                            continue;
                        }
                    } else {
                        if preview.active_turn_id.is_some() {
                            decision = "skipped_missing_turn_id";
                            record_tui_mirror_preview_sync(
                                &workspace_path,
                                session_id,
                                None,
                                item_id.as_deref(),
                                &event.occurred_at,
                                decision,
                                claim_status,
                                previous_turn_id.as_deref(),
                                preview.active_turn_id.as_deref(),
                                previous_item_id.as_deref(),
                                preview.active_item_id.as_deref(),
                                turn_transition,
                                item_transition,
                                preview.owns_active_turn,
                                text,
                                &previous_latest_preview_text,
                                preview.preview.draft_id(),
                            )
                            .await?;
                            continue;
                        }
                        preview.set_ownership(None, true);
                        claim_status = Some("not_applicable");
                    }
                    if preview.should_skip_regressive_preview(
                        turn_id.as_deref(),
                        item_id.as_deref(),
                        text,
                    ) {
                        decision = "skipped_regressive";
                    } else {
                        preview
                            .consume_preview_text(turn_id.as_deref(), item_id.as_deref(), text)
                            .await;
                    }
                    record_tui_mirror_preview_sync(
                        &workspace_path,
                        session_id,
                        turn_id.as_deref(),
                        item_id.as_deref(),
                        &event.occurred_at,
                        decision,
                        claim_status,
                        previous_turn_id.as_deref(),
                        preview.active_turn_id.as_deref(),
                        previous_item_id.as_deref(),
                        preview.active_item_id.as_deref(),
                        turn_transition,
                        item_transition,
                        preview.owns_active_turn,
                        text,
                        &previous_latest_preview_text,
                        preview.preview.draft_id(),
                    )
                    .await?;
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
                local_mirror_entry_from_event(event, local_tui_claim.session_id.as_deref())
            {
                let inserted = state
                    .repository
                    .append_transcript_mirror(&owner_record, &entry)
                    .await?;
                if entry.delivery == TranscriptMirrorDelivery::Final
                    && let Some(message_thread_id) = owner_record.metadata.message_thread_id
                {
                    let turn_id = entry.turn_id.clone();
                    let provisional_key = turn_id.is_none().then(|| {
                        provisional_key_for_text(
                            &entry.session_id,
                            DeliveryKind::AssistantFinal,
                            &entry.text,
                            &event.occurred_at,
                        )
                    });
                    let claim = state
                        .control
                        .delivery_bus
                        .claim_delivery(DeliveryClaim {
                            thread_key: owner_record.metadata.thread_key.clone(),
                            session_id: entry.session_id.clone(),
                            turn_id: turn_id.clone(),
                            provisional_key: provisional_key.clone(),
                            channel: DeliveryChannel::Telegram,
                            kind: DeliveryKind::AssistantFinal,
                            owner: "status_sync".to_owned(),
                        })
                        .await?;
                    let preview = ensure_mirror_preview(
                        mirror_previews,
                        bot,
                        state,
                        &owner_record,
                        message_thread_id,
                    );
                    preview.begin_turn(turn_id.as_deref());
                    let preview_completed = if preview.owns_turn(turn_id.as_deref())
                        || (turn_id.is_none() && preview.owns_turn(None))
                    {
                        preview.preview.complete(&entry.text).await
                    } else {
                        false
                    };
                    if matches!(claim, ClaimStatus::Claimed(_)) {
                        super::final_reply::send_final_assistant_reply(
                            bot,
                            &owner_record,
                            Some(thread_id_from_i32(message_thread_id)),
                            &entry.text,
                        )
                        .await?;
                        let _ = state
                            .control
                            .delivery_bus
                            .commit_delivery(DeliveryAttempt {
                                thread_key: owner_record.metadata.thread_key.clone(),
                                session_id: entry.session_id.clone(),
                                turn_id,
                                provisional_key,
                                channel: DeliveryChannel::Telegram,
                                kind: DeliveryKind::AssistantFinal,
                                executor: "telegram_status_sync".to_owned(),
                                transport_ref: None,
                                report_json: serde_json::json!({
                                    "targets": [{
                                        "type": "telegram_assistant_final",
                                        "target_ref": format!(
                                            "chat:{}/thread:{}",
                                            owner_record.metadata.chat_id,
                                            message_thread_id
                                        ),
                                        "state": "committed",
                                        "preview_completed": preview_completed,
                                        "mirror_inserted": inserted,
                                    }]
                                }),
                            })
                            .await;
                    }
                }
            }
            event_index = next_event_index;
        }
        workspace_event_offsets.insert(workspace_key, new_offset);
    }
    mirror_previews.retain(|thread_key, _| active_thread_keys.contains(thread_key));
    let preview_thread_keys = mirror_previews.keys().cloned().collect::<Vec<_>>();
    for thread_key in preview_thread_keys {
        if let Some(preview) = mirror_previews.get_mut(&thread_key) {
            if preview.owns_active_turn {
                preview.preview.heartbeat().await;
            }
        }
    }
    Ok(())
}

fn ensure_mirror_preview<'a>(
    mirror_previews: &'a mut HashMap<String, MirrorPreviewState>,
    bot: &Bot,
    state: &AppState,
    owner_record: &ThreadRecord,
    message_thread_id: i32,
) -> &'a mut MirrorPreviewState {
    mirror_previews
        .entry(owner_record.metadata.thread_key.clone())
        .or_insert_with(|| {
            MirrorPreviewState::new(TurnPreviewController::new(
                bot.clone(),
                ChatId(owner_record.metadata.chat_id),
                Some(thread_id_from_i32(message_thread_id)),
                state.config.stream_message_max_chars,
                state.config.command_output_tail_chars,
                state.config.stream_edit_interval_ms,
            ))
        })
}

fn local_prompt_tracking_key(workspace_key: &str, session_id: &str) -> String {
    format!("{workspace_key}::{session_id}")
}

fn initial_workspace_event_offset(
    events: &[WorkspaceStatusEventRecord],
    local_tui_claim: &LocalTuiSessionClaim,
) -> usize {
    events
        .iter()
        .position(|event| event.occurred_at >= local_tui_claim.started_at)
        .unwrap_or(events.len())
}

fn preview_event_run_end(events: &[WorkspaceStatusEventRecord], start: usize) -> usize {
    let Some(first) = events.get(start) else {
        return start;
    };
    if first.event != "preview_text" {
        return start + 1;
    }
    let Some(session_id) = first.payload.get("session_id").and_then(Value::as_str) else {
        return start + 1;
    };
    let turn_id = turn_id_from_event_payload(&first.payload);
    let mut index = start + 1;
    while let Some(candidate) = events.get(index) {
        if candidate.event != "preview_text" {
            break;
        }
        let Some(candidate_session_id) =
            candidate.payload.get("session_id").and_then(Value::as_str)
        else {
            break;
        };
        if candidate_session_id != session_id
            || turn_id_from_event_payload(&candidate.payload) != turn_id
        {
            break;
        }
        index += 1;
    }
    index
}

fn transcript_origin_from_event(event: &WorkspaceStatusEventRecord) -> TranscriptMirrorOrigin {
    if is_hcodex_ingress_client(event.payload.get("client").and_then(Value::as_str)) {
        TranscriptMirrorOrigin::Tui
    } else {
        TranscriptMirrorOrigin::Local
    }
}

fn item_id_from_event_payload(payload: &Value) -> Option<String> {
    payload
        .get("item_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn log_workspace_event_log_diagnostics(
    workspace_path: &Path,
    workspace_key: &str,
    event_log: &WorkspaceEventLogRead,
) {
    if !event_log.recovered_line_numbers.is_empty() {
        warn!(
            event = "workspace_mirror.event_log_recovered",
            workspace = %workspace_key,
            path = %events_path(workspace_path).display(),
            recovered_lines = ?event_log.recovered_line_numbers,
            rewritten = event_log.rewritten,
        );
    }
    if !event_log.malformed_line_numbers.is_empty() {
        warn!(
            event = "workspace_mirror.event_parse_failed",
            workspace = %workspace_key,
            path = %events_path(workspace_path).display(),
            malformed_lines = ?event_log.malformed_line_numbers,
        );
    }
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
                turn_id: turn_id_from_event_payload(&event.payload),
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
                turn_id: turn_id_from_event_payload(&event.payload),
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
                turn_id: turn_id_from_event_payload(&event.payload),
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

fn turn_id_from_event_payload(payload: &Value) -> Option<String> {
    payload
        .get("turn-id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("turn_id").and_then(Value::as_str))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        MirrorPreviewState, STARTUP_STALE_BUSY_RECOVERED_LOG, busy_command_message,
        busy_text_message, initial_workspace_event_offset, item_id_from_event_payload,
        local_mirror_entry_from_event, preview_event_run_end,
        reconcile_stale_bot_busy_sessions_for_repository, render_topic_title, thread_id_from_i32,
        topic_title_suffix_label, tui_adoption_prompt_text, turn_id_from_event_payload,
    };
    use crate::repository::{
        SessionBinding, ThreadMetadata, ThreadRecord, ThreadRepository, ThreadScope, ThreadStatus,
        TranscriptMirrorOrigin, TranscriptMirrorRole,
    };
    use crate::telegram_runtime::preview::TurnPreviewController;
    use crate::thread_state::effective_busy_snapshot_for_binding;
    use crate::workspace_status::{
        LocalTuiSessionClaim, SessionActivitySource, SessionCurrentStatus,
        WorkspaceStatusEventRecord, WorkspaceStatusPhase, default_local_tui_session_claim,
        ensure_workspace_status_surface, is_hcodex_ingress_client, read_session_status,
        record_bot_status_event, session_status_path, write_local_tui_session_claim,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use teloxide::Bot;
    use teloxide::types::ChatId;
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
    fn busy_messages_distinguish_turn_finalizing() {
        let snapshot = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".to_owned(),
            session_id: "thr_current".to_owned(),
            activity_source: SessionActivitySource::ManagedRuntime,
            live: true,
            phase: WorkspaceStatusPhase::TurnFinalizing,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: None,
            turn_id: Some("turn-1".to_owned()),
            summary: None,
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
            updated_at: "2026-03-27T00:00:00.000Z".to_owned(),
        };

        assert!(
            busy_text_message(&snapshot, false).contains("settling after an interrupt request")
        );
        assert!(busy_command_message(&snapshot).contains("settling after an interrupt request"));
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

    fn test_mirror_preview_state() -> MirrorPreviewState {
        MirrorPreviewState::new(TurnPreviewController::new(
            Bot::new("test-token"),
            ChatId(1),
            Some(thread_id_from_i32(7)),
            3500,
            800,
            10,
        ))
    }

    #[test]
    fn mirror_preview_state_keeps_same_turn_ownership() {
        let mut preview = test_mirror_preview_state();
        preview.begin_turn(Some("turn-1"));
        preview.set_ownership(Some("turn-1"), true);
        preview.begin_turn(Some("turn-1"));
        assert!(preview.owns_turn(Some("turn-1")));
    }

    #[test]
    fn mirror_preview_state_resets_when_turn_changes() {
        let mut preview = test_mirror_preview_state();
        preview.begin_turn(Some("turn-1"));
        preview.set_ownership(Some("turn-1"), true);
        preview.active_item_id = Some("item-1".to_owned());
        preview.latest_preview_text = "Drafting from the old turn".to_owned();
        preview.begin_turn(Some("turn-2"));
        assert_eq!(preview.active_turn_id.as_deref(), Some("turn-2"));
        assert_eq!(preview.active_item_id, None);
        assert!(!preview.owns_turn(Some("turn-1")));
        assert!(!preview.owns_turn(Some("turn-2")));
        assert!(preview.latest_preview_text.is_empty());
    }

    #[test]
    fn mirror_preview_state_skips_regressive_preview_for_same_turn_and_item() {
        let mut preview = test_mirror_preview_state();
        preview.begin_turn(Some("turn-1"));
        preview.set_ownership(Some("turn-1"), true);
        preview.active_item_id = Some("item-1".to_owned());
        preview.latest_preview_text = "Drafting a longer preview".to_owned();

        assert!(preview.should_skip_regressive_preview(Some("turn-1"), Some("item-1"), "Drafting"));
        assert!(!preview.should_skip_regressive_preview(
            Some("turn-1"),
            Some("item-1"),
            "Drafting a longer preview"
        ));
        assert!(!preview.should_skip_regressive_preview(
            Some("turn-1"),
            Some("item-1"),
            "Drafting a longer preview with more detail"
        ));
        assert!(!preview.should_skip_regressive_preview(
            Some("turn-1"),
            Some("item-2"),
            "Drafting"
        ));
    }

    #[test]
    fn mirror_preview_state_does_not_treat_new_turn_prefix_as_regression() {
        let mut preview = test_mirror_preview_state();
        preview.begin_turn(Some("turn-1"));
        preview.active_item_id = Some("item-1".to_owned());
        preview.latest_preview_text = "Drafting a longer preview".to_owned();
        preview.begin_turn(Some("turn-2"));

        assert!(!preview.should_skip_regressive_preview(
            Some("turn-2"),
            Some("item-1"),
            "Drafting"
        ));
    }

    #[test]
    fn item_id_from_event_payload_reads_preview_item_id() {
        let payload = json!({
            "session_id": "thr_tui",
            "turn_id": "turn-1",
            "item_id": "item-9",
            "text": "draft",
        });
        assert_eq!(
            item_id_from_event_payload(&payload).as_deref(),
            Some("item-9")
        );
    }

    #[test]
    fn turn_id_from_event_payload_reads_preview_turn_id() {
        let payload = json!({
            "session_id": "thr_tui",
            "turn_id": "turn-1",
            "text": "draft",
        });
        assert_eq!(
            turn_id_from_event_payload(&payload).as_deref(),
            Some("turn-1")
        );
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
        let events = vec![
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "user_prompt_submitted".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
                payload: json!({"session_id": "thr_old", "prompt": "old"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "user_prompt_submitted".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:11.000Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "prompt": "new"}),
            },
        ];

        assert_eq!(initial_workspace_event_offset(&events, &local_tui_claim), 1);
    }

    #[test]
    fn preview_event_run_end_collapses_consecutive_same_turn_snapshots() {
        let events = vec![
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "turn_id": "turn-1", "text": "a"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.010Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "turn_id": "turn-1", "text": "ab"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.020Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "turn_id": "turn-1", "text": "abc"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "turn_completed".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:01.000Z".to_owned(),
                payload: json!({"thread-id": "thr_tui", "turn-id": "turn-1", "last-assistant-message": "done"}),
            },
        ];

        assert_eq!(preview_event_run_end(&events, 0), 3);
    }

    #[test]
    fn preview_event_run_end_stops_at_turn_or_session_boundary() {
        let events = vec![
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "turn_id": "turn-1", "text": "a"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.010Z".to_owned(),
                payload: json!({"session_id": "thr_tui", "turn_id": "turn-2", "text": "b"}),
            },
            WorkspaceStatusEventRecord {
                schema_version: 2,
                event: "preview_text".to_owned(),
                source: crate::workspace_status::SessionActivitySource::Tui,
                workspace_cwd: "/tmp/workspace".to_owned(),
                occurred_at: "2026-03-19T00:00:00.020Z".to_owned(),
                payload: json!({"session_id": "thr_other", "turn_id": "turn-2", "text": "c"}),
            },
        ];

        assert_eq!(preview_event_run_end(&events, 0), 1);
        assert_eq!(preview_event_run_end(&events, 1), 2);
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

    #[test]
    fn turn_completed_entry_carries_turn_id() {
        let event = WorkspaceStatusEventRecord {
            schema_version: 2,
            event: "turn_completed".to_owned(),
            source: crate::workspace_status::SessionActivitySource::Tui,
            workspace_cwd: "/tmp/workspace".to_owned(),
            occurred_at: "2026-03-19T00:00:00.000Z".to_owned(),
            payload: json!({
                "thread-id": "thr_cli",
                "turn-id": "turn-1",
                "last-assistant-message": "done"
            }),
        };
        let entry =
            local_mirror_entry_from_event(&event, Some("thr_cli")).expect("assistant final entry");
        assert_eq!(entry.turn_id.as_deref(), Some("turn-1"));
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
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
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
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
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
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
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
        let mut claim =
            default_local_tui_session_claim(&workspace, "thread-key", std::process::id());
        claim.session_id = Some("thr_tui".to_owned());
        write_local_tui_session_claim(&workspace, &claim)
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
