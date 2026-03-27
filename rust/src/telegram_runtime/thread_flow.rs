use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use teloxide::payloads::setters::*;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::{error, info};

use super::final_reply::send_final_assistant_reply;
use super::media::{self, dispatch_workspace_telegram_outbox};
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::restore;
use super::status_sync;
use super::*;
use crate::codex::{COLLABORATION_MODE_UNAVAILABLE_PREFIX, CodexServerRequest};
use crate::collaboration_mode::CollaborationMode;
use crate::delivery_bus::{
    ClaimStatus, DeliveryAttempt, DeliveryChannel, DeliveryClaim, DeliveryKind,
    provisional_key_for_text,
};
use crate::execution_mode::{
    ExecutionMode, workspace_execution_mode, write_workspace_execution_config,
};
use crate::local_control::{TelegramControlBridgeHandle, resolve_workspace_argument};
use crate::process_transcript::process_entry_from_codex_event;
use crate::runtime_control::{
    HcodexLaunchConfigView, WorkspaceExecutionModeView, hcodex_launch_command,
    launch_hcodex_via_terminal, workspace_execution_mode_view_for_record,
    workspace_launch_config_for_record as shared_workspace_launch_config_for_record,
};
use crate::runtime_protocol::{
    WorkingSessionRecordKind, WorkingSessionRecordView, WorkingSessionSummaryView,
    build_working_session_records, build_working_session_summaries,
};
use crate::turn_completion::compose_visible_final_reply;

const TELEGRAM_SESSION_SUMMARY_LIMIT: usize = 5;
const TELEGRAM_SESSION_RECORD_LIMIT: usize = 12;
const STOP_INTERRUPT_GRACE_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchCommandTarget {
    New,
    Current,
    Resume(String),
}

fn is_nonfatal_collaboration_mode_error(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .starts_with(COLLABORATION_MODE_UNAVAILABLE_PREFIX)
}

async fn persist_collaboration_mode_change(
    state: &AppState,
    record: ThreadRecord,
    mode: CollaborationMode,
) -> Result<ThreadRecord> {
    let record = state
        .repository
        .update_session_collaboration_mode(record, mode)
        .await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::System,
            format!("Collaboration mode changed to `{}`.", mode.as_str()),
            None,
        )
        .await?;
    Ok(record)
}

async fn render_thread_info(state: &AppState, record: &ThreadRecord) -> Result<String> {
    let session = state.repository.read_session_binding(record).await?;
    let (resolved_state, blocking_snapshot) =
        resolve_busy_gate_state(state, record, session.as_ref()).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let workspace_execution_mode = match workspace_path.as_deref() {
        Some(path) => workspace_execution_mode(path).await.ok(),
        None => None,
    };
    let current_codex_thread_id = current_bound_session_id(session.as_ref())
        .map(str::to_owned)
        .unwrap_or_else(|| "none".to_owned());
    let current_execution_mode = session
        .as_ref()
        .and_then(|binding| binding.current_execution_mode)
        .map(|mode| mode.as_str().to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let current_collaboration_mode = session
        .as_ref()
        .and_then(|binding| binding.current_collaboration_mode)
        .map(|mode| mode.as_str().to_owned())
        .unwrap_or_else(|| "default".to_owned());
    let current_snapshot = match (
        workspace_path.as_ref(),
        current_bound_session_id(session.as_ref()),
    ) {
        (Some(path), Some(session_id)) => read_session_status(path, session_id).await?,
        _ => None,
    };
    let tui_active_codex_thread_id = session
        .as_ref()
        .and_then(|binding| binding.tui_active_codex_thread_id.as_deref())
        .unwrap_or("none");
    let adoption_state = session
        .as_ref()
        .map(|binding| {
            if binding.tui_session_adoption_pending {
                "pending"
            } else {
                "none"
            }
        })
        .unwrap_or("none");
    let workspace = workspace_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unbound".to_owned());
    let current_phase = current_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.phase))
        .unwrap_or_else(|| "none".to_owned());
    let current_owner = current_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.activity_source))
        .unwrap_or_else(|| "none".to_owned());
    let gate_session_id = blocking_snapshot
        .as_ref()
        .map(|snapshot| snapshot.session_id.clone())
        .unwrap_or_else(|| "none".to_owned());
    let gate_phase = blocking_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.phase))
        .unwrap_or_else(|| "none".to_owned());
    let gate_owner = blocking_snapshot
        .as_ref()
        .map(|snapshot| format!("{:?}", snapshot.activity_source))
        .unwrap_or_else(|| "none".to_owned());

    Ok(format!(
        "thread_key: `{}`\nworkspace: `{}`\nworkspace_execution_mode: `{}`\ncurrent_execution_mode: `{}`\ncurrent_collaboration_mode: `{}`\ncurrent_codex_thread_id: `{}`\ntui_active_codex_thread_id: `{}`\nadoption_state: `{}`\nlifecycle_status: `{}`\nbinding_status: `{}`\nrun_status: `{}`\ntitle_suffix: `{}`\ncurrent_phase: `{}`\ncurrent_owner: `{}`\ngate_session_id: `{}`\ngate_phase: `{}`\ngate_owner: `{}`",
        record.metadata.thread_key,
        workspace,
        workspace_execution_mode
            .map(|mode| mode.as_str().to_owned())
            .unwrap_or_else(|| "none".to_owned()),
        current_execution_mode,
        current_collaboration_mode,
        current_codex_thread_id,
        tui_active_codex_thread_id,
        adoption_state,
        resolved_state.lifecycle_status.as_str(),
        resolved_state.binding_status.as_str(),
        resolved_state.run_status.as_str(),
        status_sync::topic_title_suffix_label(resolved_state.is_broken()),
        current_phase,
        current_owner,
        gate_session_id,
        gate_phase,
        gate_owner,
    ))
}

fn parse_launch_command_target(argument: &str) -> Option<LaunchCommandTarget> {
    let mut parts = argument.split_whitespace();
    match parts.next()? {
        "new" => Some(LaunchCommandTarget::New),
        "current" => Some(LaunchCommandTarget::Current),
        "resume" => Some(LaunchCommandTarget::Resume(parts.next()?.to_owned())),
        _ => None,
    }
}

fn parse_execution_mode_argument(argument: &str) -> Option<ExecutionMode> {
    match argument.trim().to_ascii_lowercase().as_str() {
        "full_auto" | "full-auto" => Some(ExecutionMode::FullAuto),
        "yolo" => Some(ExecutionMode::Yolo),
        _ => None,
    }
}

async fn build_workspace_launch_config(
    state: &AppState,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<HcodexLaunchConfigView> {
    shared_workspace_launch_config_for_record(&state.repository, record, binding).await
}

fn render_workspace_execution_mode_view(view: &WorkspaceExecutionModeView) -> String {
    format!(
        "workspace_execution_mode: `{}`\ncurrent_execution_mode: `{}`\ncurrent_approval_policy: `{}`\ncurrent_sandbox_policy: `{}`\nmode_drift: `{}`\n\nUse `/execution_mode full_auto` or `/execution_mode yolo`.",
        view.workspace_execution_mode.as_str(),
        view.current_execution_mode
            .map(|mode| mode.as_str().to_owned())
            .unwrap_or_else(|| "none".to_owned()),
        view.current_approval_policy
            .clone()
            .unwrap_or_else(|| "none".to_owned()),
        view.current_sandbox_policy
            .clone()
            .unwrap_or_else(|| "none".to_owned()),
        if view.mode_drift { "yes" } else { "no" },
    )
}

fn render_launch_usage(config: &HcodexLaunchConfigView) -> String {
    let recent = if config.recent_codex_sessions.is_empty() {
        "none".to_owned()
    } else {
        config
            .recent_codex_sessions
            .iter()
            .map(|entry| entry.session_id.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Usage: `/launch new`, `/launch current`, or `/launch resume <session_id>`.\ncurrent_codex_thread_id: `{}`\nrecent_sessions: `{}`",
        config
            .current_codex_thread_id
            .clone()
            .unwrap_or_else(|| "none".to_owned()),
        recent,
    )
}

fn render_working_sessions(
    binding: &SessionBinding,
    summaries: &[WorkingSessionSummaryView],
) -> String {
    if summaries.is_empty() {
        return "No working sessions recorded yet.".to_owned();
    }
    let mut lines = vec!["Recent working sessions:".to_owned()];
    for summary in summaries.iter().take(TELEGRAM_SESSION_SUMMARY_LIMIT) {
        let current = if binding.current_codex_thread_id.as_deref() == Some(&summary.session_id) {
            " current"
        } else {
            ""
        };
        let origins = if summary.origins_seen.is_empty() {
            "none".to_owned()
        } else {
            summary
                .origins_seen
                .iter()
                .map(|origin| format!("{origin:?}").to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join(",")
        };
        let run_label = if summary.run_status == "running" && summary.run_phase == "turn_finalizing"
        {
            "running/finalizing".to_owned()
        } else {
            summary.run_status.clone()
        };
        lines.push(format!(
            "- `{}`{} | {} | records={} | tools={} | final={} | origins={}",
            summary.session_id,
            current,
            run_label,
            summary.record_count,
            summary.tool_use_count,
            if summary.has_final_reply { "yes" } else { "no" },
            origins,
        ));
    }
    lines.push("Use `/session_log <session_id>` for detailed records.".to_owned());
    lines.join("\n")
}

fn render_working_session_records(
    session_id: &str,
    records: &[WorkingSessionRecordView],
) -> String {
    if records.is_empty() {
        return format!("No records found for session `{session_id}`.");
    }
    let mut lines = vec![format!("Recent records for session `{session_id}`:")];
    for record in records
        .iter()
        .rev()
        .take(TELEGRAM_SESSION_RECORD_LIMIT)
        .rev()
    {
        let kind = match record.kind {
            WorkingSessionRecordKind::UserPrompt => "user_prompt",
            WorkingSessionRecordKind::AssistantFinal => "assistant_final",
            WorkingSessionRecordKind::ProcessPlan => "process_plan",
            WorkingSessionRecordKind::ProcessTool => "process_tool",
            WorkingSessionRecordKind::Error => "error",
        };
        let summary = record.summary.replace('`', "'");
        lines.push(format!("- {} | {} | {}", record.timestamp, kind, summary));
    }
    lines.join("\n")
}

fn render_stop_started_message(snapshot: &SessionCurrentStatus) -> String {
    if snapshot.phase == crate::workspace_status::WorkspaceStatusPhase::TurnFinalizing {
        return match snapshot.activity_source {
            crate::workspace_status::SessionActivitySource::Tui => format!(
                "Interrupt was already requested for shared TUI session `{}`. Wait for the current turn to settle.",
                snapshot.session_id
            ),
            crate::workspace_status::SessionActivitySource::ManagedRuntime => format!(
                "Interrupt was already requested for Telegram session `{}`. Wait for the current turn to settle.",
                snapshot.session_id
            ),
        };
    }
    match snapshot.activity_source {
        crate::workspace_status::SessionActivitySource::Tui => format!(
            "Interrupt requested for shared TUI session `{}`. Wait for the current turn to settle.",
            snapshot.session_id
        ),
        crate::workspace_status::SessionActivitySource::ManagedRuntime => format!(
            "Interrupt requested for Telegram session `{}`. Wait for the current turn to settle.",
            snapshot.session_id
        ),
    }
}

fn spawn_stop_interrupt_watchdog(
    state: AppState,
    record: ThreadRecord,
    workspace_path: PathBuf,
    session_id: String,
    turn_id: String,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(STOP_INTERRUPT_GRACE_MS)).await;
        match crate::workspace_status::finalize_pending_bot_interrupt_if_still_busy(
            &workspace_path,
            &session_id,
            &turn_id,
        )
        .await
        {
            Ok(true) => {
                let _ = state
                    .repository
                    .append_log(
                        &record,
                        LogDirection::System,
                        format!(
                            "Interrupted session `{}` turn `{}` after `/stop` fallback cleanup.",
                            session_id, turn_id
                        ),
                        None,
                    )
                    .await;
            }
            Ok(false) => {}
            Err(error) => {
                error!(
                    event = "telegram.stop.watchdog.failed",
                    thread_key = %record.metadata.thread_key,
                    session_id,
                    turn_id,
                    error = %error,
                    "failed to reconcile pending `/stop` interrupt"
                );
            }
        }
    });
}

pub(crate) async fn run_command(
    bot: &Bot,
    msg: &Message,
    command: Command,
    state: &AppState,
) -> Result<()> {
    match command {
        Command::Start => {
            let text = if is_control_chat(msg) {
                let record = state.repository.get_main_thread(msg.chat.id.0).await?;
                state
                    .repository
                    .append_log(
                        &record,
                        LogDirection::System,
                        "Control chat initialized from /start.",
                        None,
                    )
                    .await?;
                "Control console.\nUse /add_workspace <absolute-path> for the workspace-first flow."
            } else {
                "Workspace thread.\nUse /new_session, /repair_session, /archive_workspace, /workspace_info, or /rename_workspace here."
            };
            send_scoped_message(bot, msg.chat.id, msg.thread_id, text).await?;
        }
        Command::AddWorkspace => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /add_workspace <absolute-path> from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let Some(argument) = command_argument_text(msg, "add_workspace") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Usage: /add_workspace <absolute-path>",
                )
                .await?;
                return Ok(());
            };
            let workspace_path = resolve_workspace_argument(argument).await?;
            let bridge = TelegramControlBridgeHandle::new(bot.clone(), state.repository.clone());
            match state
                .control
                .workspace_session_service()
                .resolve_workspace_add(&workspace_path)
                .await
            {
                Ok(crate::runtime_control::WorkspaceAddResolution::Existing(record)) => {
                    let binding = state.repository.read_session_binding(&record).await?;
                    if let Some(workspace_cwd) = binding
                        .as_ref()
                        .and_then(|binding| binding.workspace_cwd.as_deref())
                    {
                        let _ = bridge
                            .notify_workspace_bound(
                                &record,
                                Path::new(workspace_cwd),
                                "telegram_bind_workspace",
                            )
                            .await;
                    }
                }
                Ok(crate::runtime_control::WorkspaceAddResolution::Create {
                    canonical_workspace_cwd,
                    suggested_title,
                }) => {
                    let created = bridge
                        .create_workspace_thread(Some(suggested_title), "Telegram /add_workspace")
                        .await?;
                    let record = state
                        .control
                        .repository
                        .create_thread(
                            created.chat_id,
                            created.message_thread_id,
                            created.title.clone(),
                        )
                        .await?;
                    let record = state
                        .control
                        .workspace_session_service()
                        .bind_workspace_record(record, Path::new(&canonical_workspace_cwd))
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Bound Telegram thread to workspace {} from Telegram /add_workspace.",
                                canonical_workspace_cwd
                            ),
                            None,
                        )
                        .await?;
                    bridge
                        .notify_workspace_bound(
                            &record,
                            Path::new(&canonical_workspace_cwd),
                            "telegram_bind_workspace",
                        )
                        .await?;
                }
                Err(error) => {
                    send_scoped_warning_message(
                        bot,
                        msg.chat.id,
                        None,
                        format!("Add workspace failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::NewSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /new_session inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, blocking_snapshot) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet. Archive it and re-add the workspace from the control chat with /add_workspace <absolute-path>.",
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = blocking_snapshot.as_ref() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(busy),
                )
                .await?;
                return Ok(());
            }
            let workspace_path = workspace_path_from_binding(binding)?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .control
                .workspace_session_service()
                .start_fresh_session(record.clone(), workspace_path.clone())
                .await;
            typing.stop().await;

            match result {
                Ok(record) => {
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Started a fresh Codex session for workspace {}.",
                                workspace_path.display()
                            ),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Started a fresh Codex session for this workspace.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        &state.repository,
                        &record,
                        "new",
                    )
                    .await;
                }
                Err(error) => {
                    let _ = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_warning_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("New session failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::RepairSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /repair_session inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, blocking_snapshot) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(_) = usable_bound_session_id(resolved_state, session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint_for_state(resolved_state, session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = blocking_snapshot.as_ref() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(busy),
                )
                .await?;
                return Ok(());
            }
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnect = state
                .control
                .workspace_session_service()
                .repair_session_binding(record, session.as_ref().context("missing binding")?)
                .await;
            typing.stop().await;

            match reconnect {
                Ok(result) if result.verified => {
                    let updated = result.record;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            "Codex session revalidated for the current workspace binding.",
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session continuity verified for this workspace.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        &state.repository,
                        &updated,
                        "reconnect_codex_verified",
                    )
                    .await;
                }
                Ok(result) => {
                    let updated = result.record;
                    send_scoped_warning_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session repair failed. Use /new_session to start fresh or /repair_session to retry.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        &state.repository,
                        &updated,
                        "reconnect_codex_broken",
                    )
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
        Command::WorkspaceInfo => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /workspace_info inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                render_thread_info(state, &record).await?,
            )
            .await?;
        }
        Command::Launch => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /launch inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, _) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet.",
                )
                .await?;
                return Ok(());
            };
            let config = build_workspace_launch_config(state, &record, binding).await?;
            let Some(argument) = command_argument_text(msg, "launch") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_launch_usage(&config),
                )
                .await?;
                return Ok(());
            };
            let Some(target) = parse_launch_command_target(argument) else {
                send_scoped_warning_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_launch_usage(&config),
                )
                .await?;
                return Ok(());
            };
            if !config.hcodex_available {
                send_scoped_warning_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "Managed hcodex is unavailable for this workspace.",
                )
                .await?;
                return Ok(());
            }
            let (command, label) = match target {
                LaunchCommandTarget::New => (config.launch_new_command.clone(), "new"),
                LaunchCommandTarget::Current => (
                    config
                        .launch_current_command
                        .clone()
                        .context("managed workspace is missing a current Telegram session")?,
                    "current",
                ),
                LaunchCommandTarget::Resume(session_id) => (
                    hcodex_launch_command(
                        Path::new(&config.hcodex_path),
                        &record.metadata.thread_key,
                        config.workspace_execution_mode,
                        Some(&session_id),
                    ),
                    "resume",
                ),
            };
            launch_hcodex_via_terminal(&command).await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!("Launched local hcodex via `/launch {label}`."),
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!(
                    "Launched local hcodex via `{label}` in `{}` mode.",
                    config.workspace_execution_mode.as_str()
                ),
            )
            .await?;
        }
        Command::ExecutionMode => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /execution_mode inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, _) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet.",
                )
                .await?;
                return Ok(());
            };
            let view = workspace_execution_mode_view_for_record(&record, binding).await?;
            let Some(argument) = command_argument_text(msg, "execution_mode") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_workspace_execution_mode_view(&view),
                )
                .await?;
                return Ok(());
            };
            let Some(mode) = parse_execution_mode_argument(argument) else {
                send_scoped_warning_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_workspace_execution_mode_view(&view),
                )
                .await?;
                return Ok(());
            };
            write_workspace_execution_config(Path::new(&view.workspace_cwd), mode).await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!("Workspace execution mode changed to `{}`.", mode.as_str()),
                    None,
                )
                .await?;
            let updated_view = workspace_execution_mode_view_for_record(&record, binding).await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!(
                    "Workspace execution mode is now `{}`.\nExisting sessions converge on the next turn or resume.\n\n{}",
                    updated_view.workspace_execution_mode.as_str(),
                    render_workspace_execution_mode_view(&updated_view)
                ),
            )
            .await?;
        }
        Command::Sessions => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /sessions inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, _) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet.",
                )
                .await?;
                return Ok(());
            };
            let summaries =
                build_working_session_summaries(&state.repository, &record, binding).await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                render_working_sessions(binding, &summaries),
            )
            .await?;
        }
        Command::SessionLog => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /session_log inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, _) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet.",
                )
                .await?;
                return Ok(());
            };
            let Some(session_id) = command_argument_text(msg, "session_log") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "Usage: /session_log <session_id>",
                )
                .await?;
                return Ok(());
            };
            let Some(records) =
                build_working_session_records(&state.repository, &record, binding, session_id)
                    .await?
            else {
                send_scoped_warning_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    format!("Session `{session_id}` was not found for this workspace."),
                )
                .await?;
                return Ok(());
            };
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                render_working_session_records(session_id, &records),
            )
            .await?;
        }
        Command::Stop => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /stop inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, blocking_snapshot) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace thread is not bound yet.",
                )
                .await?;
                return Ok(());
            };
            let Some(busy) = blocking_snapshot.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "No active turn is running for this workspace.",
                )
                .await?;
                return Ok(());
            };
            if busy.phase == crate::workspace_status::WorkspaceStatusPhase::TurnFinalizing {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_stop_started_message(busy),
                )
                .await?;
                return Ok(());
            }
            let Some(turn_id) = busy.turn_id.as_deref() else {
                send_scoped_warning_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    format!(
                        "A turn is running for session `{}`, but its turn id is unavailable, so `/stop` cannot interrupt it yet.",
                        busy.session_id
                    ),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = workspace_path_from_binding(binding)?;
            let codex_workspace = state
                .control
                .workspace_runtime_service()
                .shared_codex_workspace(workspace_path.clone())
                .await?;
            state
                .codex
                .interrupt_turn(&codex_workspace, &busy.session_id, turn_id)
                .await?;
            crate::workspace_status::record_bot_interrupt_requested(
                &workspace_path,
                &busy.session_id,
                turn_id,
            )
            .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!(
                        "Requested interrupt for session `{}` turn `{}` via `/stop`.",
                        busy.session_id, turn_id
                    ),
                    None,
                )
                .await?;
            spawn_stop_interrupt_watchdog(
                state.clone(),
                record.clone(),
                workspace_path,
                busy.session_id.clone(),
                turn_id.to_owned(),
            );
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                render_stop_started_message(busy),
            )
            .await?;
        }
        Command::RenameWorkspace => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /rename_workspace inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let (resolved_state, blocking_snapshot) =
                resolve_busy_gate_state(state, &record, session.as_ref()).await?;
            if resolved_state.is_archived() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This workspace is archived.",
                )
                .await?;
                return Ok(());
            }
            let Some(existing_thread_id) =
                usable_bound_session_id(resolved_state, session.as_ref())
            else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint_for_state(resolved_state, session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = blocking_snapshot.as_ref() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(busy),
                )
                .await?;
                return Ok(());
            }
            let workspace_path = state
                .control
                .workspace_runtime_service()
                .ensure_bound_workspace_runtime(session.as_ref().context("missing binding")?)
                .await?;
            let codex_workspace = state
                .control
                .workspace_runtime_service()
                .shared_codex_workspace(workspace_path.clone())
                .await?;
            record_bot_status_event(
                &workspace_path,
                "bot_turn_started",
                Some(existing_thread_id),
                None,
                Some("Generate Telegram topic title from conversation"),
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .generate_thread_title_from_session(&codex_workspace, existing_thread_id)
                .await;
            typing.stop().await;

            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let _ = record_bot_status_event(
                        &workspace_path,
                        "bot_turn_failed",
                        Some(existing_thread_id),
                        None,
                        Some("generate_title failed"),
                    )
                    .await;
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_warning_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /repair_session or /new_session.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(
                        bot,
                        &state.repository,
                        &updated,
                        "generate_title_broken",
                    )
                    .await;
                    return Ok(());
                }
            };

            let mut updated = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let title = result.final_response.trim().to_owned();
            record_bot_status_event(
                &workspace_path,
                "bot_turn_completed",
                Some(existing_thread_id),
                None,
                Some(&title),
            )
            .await?;
            updated.metadata.title = Some(title.clone());
            let updated = state.repository.update_metadata(updated).await?;
            info!(
                event = "telegram.generate_title.completed",
                thread_key = %updated.metadata.thread_key,
                chat_id = updated.metadata.chat_id,
                message_thread_id = updated.metadata.message_thread_id.unwrap_or_default(),
                codex_thread_id = existing_thread_id,
                generated_title = %title,
                "generated Telegram topic title from Codex conversation"
            );
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!("Generated title: {title}"),
                    None,
                )
                .await?;
            let _ = status_sync::refresh_thread_topic_title(
                bot,
                &state.repository,
                &updated,
                "generate_title",
            )
            .await;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Workspace renamed: {title}"),
            )
            .await?;
        }
        Command::ArchiveWorkspace => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /archive_workspace inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let _ = bot.delete_forum_topic(msg.chat.id, thread_id).await;
            let record = state.repository.archive_thread(record).await?;
            state
                .repository
                .append_log(&record, LogDirection::System, "Workspace archived.", None)
                .await?;
        }
        Command::RestoreWorkspace => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /restore_workspace from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let (text, markup) = restore::render_restore_page(state, msg.chat.id.0, 0).await?;
            bot.send_message(msg.chat.id, text)
                .link_preview_options(disabled_link_preview_options())
                .reply_markup(markup)
                .await?;
        }
        Command::PlanMode | Command::DefaultMode => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use this command inside a workspace thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let mode = match command {
                Command::PlanMode => CollaborationMode::Plan,
                Command::DefaultMode => CollaborationMode::Default,
                _ => unreachable!(),
            };
            let record = persist_collaboration_mode_change(state, record, mode).await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Collaboration mode is now `{}`.", mode.as_str()),
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(
                bot,
                &state.repository,
                &record,
                "collaboration_mode",
            )
            .await;
        }
    }
    Ok(())
}

pub(crate) async fn run_text_message(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &AppState,
) -> Result<()> {
    if is_control_chat(msg) {
        send_scoped_message(
            bot,
            msg.chat.id,
            None,
            "Main private chat is the control console. Use /add_workspace <absolute-path> first.",
        )
        .await?;
        return Ok(());
    }

    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if let Some(advance) = state
        .interactive_requests
        .submit_text(msg.chat.id.0, thread_id_to_i32(thread_id), text.to_owned())
        .await?
    {
        apply_interactive_advance(bot, state, msg.chat.id, thread_id, advance).await?;
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let (record, session) = state
        .control
        .session_routing_service()
        .maybe_route_telegram_input_to_tui_session(record, session)
        .await?
        .into_record_session();
    let (resolved_state, blocking_snapshot) =
        resolve_busy_gate_state(state, &record, session.as_ref()).await?;
    if resolved_state.is_archived() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This workspace is archived.",
        )
        .await?;
        return Ok(());
    }
    let Some(existing_thread_id) = usable_bound_session_id(resolved_state, session.as_ref()) else {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint_for_state(resolved_state, session.as_ref()),
        )
        .await?;
        return Ok(());
    };
    let workspace_path = state
        .control
        .workspace_runtime_service()
        .ensure_bound_workspace_runtime(session.as_ref().context("missing binding")?)
        .await?;
    if let Some(busy) = blocking_snapshot.as_ref() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::busy_text_message(busy, false),
        )
        .await?;
        return Ok(());
    }
    info!(
        event = "telegram.thread.message.received",
        thread_key = %record.metadata.thread_key,
        chat_id = record.metadata.chat_id,
        message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
        codex_thread_id = existing_thread_id,
        text = text,
        "received thread text message"
    );

    if let Some(batch) = state.repository.read_pending_image_batch(&record).await? {
        if !batch.images.is_empty() {
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::User,
                    text.to_owned(),
                    msg.from.as_ref().map(|user| user.id.0 as i64),
                )
                .await?;
            media::analyze_pending_image_batch(
                bot,
                state,
                record,
                thread_id,
                &batch.batch_id,
                Some(text),
                None,
            )
            .await?;
            return Ok(());
        }
    }

    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            text.to_owned(),
            msg.from.as_ref().map(|user| user.id.0 as i64),
        )
        .await?;
    let _ = state
        .repository
        .append_transcript_mirror(
            &record,
            &TranscriptMirrorEntry {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                session_id: existing_thread_id.to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: text.to_owned(),
            },
        )
        .await?;
    let prompt_occurred_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let user_echo_key = provisional_key_for_text(
        existing_thread_id,
        DeliveryKind::UserEcho,
        text,
        &prompt_occurred_at,
    );
    let _ = state
        .control
        .delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: record.metadata.thread_key.clone(),
            session_id: existing_thread_id.to_owned(),
            turn_id: None,
            provisional_key: Some(user_echo_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::UserEcho,
            owner: "telegram_thread_flow".to_owned(),
        })
        .await?;
    let _ = state
        .control
        .delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key: record.metadata.thread_key.clone(),
            session_id: existing_thread_id.to_owned(),
            turn_id: None,
            provisional_key: Some(user_echo_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::UserEcho,
            executor: "telegram_inbound".to_owned(),
            transport_ref: Some(format!("message:{}", msg.id.0)),
            report_json: serde_json::json!({
                "targets": [{
                    "type": "telegram_inbound_message",
                    "target_ref": format!(
                        "chat:{}/thread:{}",
                        record.metadata.chat_id,
                        thread_id_to_i32(thread_id)
                    ),
                    "state": "committed",
                    "transport_ref": format!("message:{}", msg.id.0),
                }]
            }),
        })
        .await;
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(text),
    )
    .await?;

    spawn_text_turn(
        bot.clone(),
        state.clone(),
        record,
        msg.chat.id,
        thread_id,
        workspace_path,
        existing_thread_id.to_owned(),
        user_echo_key,
        text.to_owned(),
        collaboration_mode_for_session(session.as_ref()),
    );

    Ok(())
}

fn spawn_text_turn(
    bot: Bot,
    state: AppState,
    record: ThreadRecord,
    chat_id: ChatId,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: String,
    user_echo_key: String,
    text: String,
    collaboration_mode: CollaborationMode,
) {
    tokio::spawn(async move {
        if let Err(error) = execute_text_turn(
            &bot,
            &state,
            record,
            chat_id,
            thread_id,
            workspace_path,
            &existing_thread_id,
            &user_echo_key,
            &text,
            collaboration_mode,
        )
        .await
        {
            error!(
                event = "telegram.thread.message.background_failed",
                chat_id = chat_id.0,
                message_thread_id = thread_id_to_i32(thread_id),
                error = %error,
                "background text turn failed"
            );
            let _ = send_scoped_warning_message(
                &bot,
                chat_id,
                Some(thread_id),
                format!("Request failed: {error}"),
            )
            .await;
        }
    });
}

async fn execute_text_turn(
    bot: &Bot,
    state: &AppState,
    mut record: ThreadRecord,
    chat_id: ChatId,
    thread_id: ThreadId,
    workspace_path: PathBuf,
    existing_thread_id: &str,
    user_echo_key: &str,
    text: &str,
    collaboration_mode: CollaborationMode,
) -> Result<()> {
    let typing = TypingHeartbeat::start(bot.clone(), chat_id, Some(thread_id));
    let codex_workspace = state
        .control
        .workspace_runtime_service()
        .shared_codex_workspace(workspace_path.clone())
        .await?;
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        chat_id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    let mirror_record = record.clone();
    let mirror_repository = state.repository.clone();
    let mirror_session_id = existing_thread_id.to_owned();
    let turn_workspace_path = workspace_path.clone();
    let turn_session_id = existing_thread_id.to_owned();
    let user_turn_key = user_echo_key.to_owned();
    let delivery_bus = state.control.delivery_bus.clone();
    let turn_id_slot = Arc::new(Mutex::new(None::<String>));
    let event_turn_id_slot = turn_id_slot.clone();
    let execution_mode = workspace_execution_mode(&workspace_path).await?;
    let interactive_bot = bot.clone();
    let interactive_state = state.clone();
    let interactive_thread_key = record.metadata.thread_key.clone();

    let result = state
        .codex
        .run_locked_prompt_with_events_mode_and_requests(
            &codex_workspace,
            existing_thread_id,
            Some(execution_mode),
            Some(collaboration_mode),
            text,
            |event| {
                let preview = preview.clone();
                let mirror_record = mirror_record.clone();
                let mirror_repository = mirror_repository.clone();
                let mirror_session_id = mirror_session_id.clone();
                let turn_workspace_path = turn_workspace_path.clone();
                let turn_session_id = turn_session_id.clone();
                let delivery_bus = delivery_bus.clone();
                let user_turn_key = user_turn_key.clone();
                let turn_id_slot = event_turn_id_slot.clone();
                async move {
                    if let CodexThreadEvent::TurnStarted {
                        turn_id: Some(turn_id),
                    } = &event
                    {
                        *turn_id_slot.lock().await = Some(turn_id.clone());
                        let _ = record_bot_status_event(
                            &turn_workspace_path,
                            "bot_turn_started",
                            Some(&turn_session_id),
                            Some(turn_id),
                            None,
                        )
                        .await;
                        let _ = delivery_bus
                            .promote_delivery_turn(
                                &mirror_record.metadata.thread_key,
                                &turn_session_id,
                                &user_turn_key,
                                DeliveryChannel::Telegram,
                                DeliveryKind::UserEcho,
                                turn_id,
                            )
                            .await;
                    }
                    preview.lock().await.consume(&event).await;
                    if let Some(entry) = process_entry_from_codex_event(
                        &event,
                        &mirror_session_id,
                        TranscriptMirrorOrigin::Telegram,
                    ) {
                        preview.lock().await.consume_process_entry(&entry).await;
                        let _ = mirror_repository
                            .append_transcript_mirror(&mirror_record, &entry)
                            .await;
                    }
                }
            },
            move |request| {
                let interactive_bot = interactive_bot.clone();
                let interactive_state = interactive_state.clone();
                let interactive_thread_key = interactive_thread_key.clone();
                async move {
                    match request {
                        CodexServerRequest::RequestUserInput { request_id, params } => {
                            if params.questions.iter().any(|question| question.is_secret) {
                                return Ok(None);
                            }
                            let (tx, rx) = oneshot::channel();
                            let snapshot = interactive_state
                                .interactive_requests
                                .register_direct(
                                    chat_id.0,
                                    thread_id_to_i32(thread_id),
                                    interactive_thread_key,
                                    request_id,
                                    params,
                                    tx,
                                )
                                .await?;
                            upsert_request_user_input_prompt(
                                &interactive_bot,
                                &interactive_state,
                                chat_id,
                                thread_id,
                                &snapshot,
                            )
                            .await?;
                            let response =
                                rx.await.context("request_user_input response dropped")?;
                            Ok(Some(response))
                        }
                    }
                }
            },
        )
        .await;
    preview_heartbeat.stop().await;
    typing.stop().await;

    match result {
        Ok(result) => {
            match result.turn_outcome {
                crate::codex::CodexTurnOutcome::Interrupted => {
                    let interrupted_turn_id = turn_id_slot.lock().await.clone();
                    record_bot_status_event(
                        &workspace_path,
                        "bot_turn_interrupted",
                        Some(existing_thread_id),
                        interrupted_turn_id.as_deref(),
                        None,
                    )
                    .await?;
                    record = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    record = state
                        .repository
                        .update_session_execution_snapshot(record, &result.execution)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            "Interrupted current reply via `/stop`.",
                            None,
                        )
                        .await?;
                }
                crate::codex::CodexTurnOutcome::Completed
                | crate::codex::CodexTurnOutcome::Failed => {
                    let visible_final_text = compose_visible_final_reply(
                        &result.final_response,
                        result.final_plan_text.as_deref(),
                    );
                    record_bot_status_event(
                        &workspace_path,
                        "bot_turn_completed",
                        Some(existing_thread_id),
                        None,
                        visible_final_text.as_deref(),
                    )
                    .await?;
                    record = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    record = state
                        .repository
                        .update_session_execution_snapshot(record, &result.execution)
                        .await?;
                    if let Some(final_text) = visible_final_text.as_deref() {
                        let final_turn_id = turn_id_slot.lock().await.clone();
                        let final_occurred_at =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let final_provisional_key = final_turn_id.as_ref().is_none().then(|| {
                            provisional_key_for_text(
                                existing_thread_id,
                                DeliveryKind::AssistantFinal,
                                final_text,
                                &final_occurred_at,
                            )
                        });
                        state
                            .repository
                            .append_log(&record, LogDirection::Assistant, final_text, None)
                            .await?;
                        let _ = state
                            .repository
                            .append_transcript_mirror(
                                &record,
                                &TranscriptMirrorEntry {
                                    timestamp: chrono::Utc::now()
                                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                                    session_id: existing_thread_id.to_owned(),
                                    origin: TranscriptMirrorOrigin::Telegram,
                                    role: TranscriptMirrorRole::Assistant,
                                    delivery: TranscriptMirrorDelivery::Final,
                                    phase: None,
                                    text: final_text.to_owned(),
                                },
                            )
                            .await?;
                        let final_claim = state
                            .control
                            .delivery_bus
                            .claim_delivery(DeliveryClaim {
                                thread_key: record.metadata.thread_key.clone(),
                                session_id: existing_thread_id.to_owned(),
                                turn_id: final_turn_id.clone(),
                                provisional_key: final_provisional_key.clone(),
                                channel: DeliveryChannel::Telegram,
                                kind: DeliveryKind::AssistantFinal,
                                owner: "telegram_thread_flow".to_owned(),
                            })
                            .await?;
                        let preview_completed = preview.lock().await.complete(final_text).await;
                        if matches!(final_claim, ClaimStatus::Claimed(_)) {
                            if !preview_completed
                                && let Err(error) = send_final_assistant_reply(
                                    bot,
                                    &record,
                                    Some(thread_id),
                                    final_text,
                                )
                                .await
                            {
                                let _ = state
                                    .control
                                    .delivery_bus
                                    .fail_delivery(
                                        DeliveryAttempt {
                                            thread_key: record.metadata.thread_key.clone(),
                                            session_id: existing_thread_id.to_owned(),
                                            turn_id: final_turn_id.clone(),
                                            provisional_key: final_provisional_key.clone(),
                                            channel: DeliveryChannel::Telegram,
                                            kind: DeliveryKind::AssistantFinal,
                                            executor: "telegram_thread_flow".to_owned(),
                                            transport_ref: None,
                                            report_json: serde_json::json!({ "targets": [] }),
                                        },
                                        error.to_string(),
                                    )
                                    .await;
                                return Err(error.into());
                            }
                            let _ = state
                                .control
                                .delivery_bus
                                .commit_delivery(DeliveryAttempt {
                                    thread_key: record.metadata.thread_key.clone(),
                                    session_id: existing_thread_id.to_owned(),
                                    turn_id: final_turn_id.clone(),
                                    provisional_key: final_provisional_key.clone(),
                                    channel: DeliveryChannel::Telegram,
                                    kind: DeliveryKind::AssistantFinal,
                                    executor: "telegram_thread_flow".to_owned(),
                                    transport_ref: None,
                                    report_json: serde_json::json!({
                                        "targets": [{
                                            "type": "telegram_assistant_final",
                                            "target_ref": format!(
                                                "chat:{}/thread:{}",
                                                record.metadata.chat_id,
                                                thread_id_to_i32(thread_id)
                                            ),
                                            "state": "committed",
                                            "preview_completed": preview_completed,
                                        }]
                                    }),
                                })
                                .await;
                        }
                    }
                    if collaboration_mode == CollaborationMode::Plan
                        && result.final_plan_text.is_some()
                    {
                        let plan_prompt_at = chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let plan_key = provisional_key_for_text(
                            existing_thread_id,
                            DeliveryKind::SystemNotice,
                            visible_final_text
                                .as_deref()
                                .unwrap_or("plan_implementation_prompt"),
                            &plan_prompt_at,
                        );
                        let plan_claim = state
                            .control
                            .delivery_bus
                            .claim_delivery(DeliveryClaim {
                                thread_key: record.metadata.thread_key.clone(),
                                session_id: existing_thread_id.to_owned(),
                                turn_id: turn_id_slot.lock().await.clone(),
                                provisional_key: Some(plan_key.clone()),
                                channel: DeliveryChannel::Telegram,
                                kind: DeliveryKind::SystemNotice,
                                owner: "telegram_thread_flow".to_owned(),
                            })
                            .await?;
                        if matches!(plan_claim, ClaimStatus::Claimed(_)) {
                            send_plan_implementation_prompt(bot, chat_id, thread_id).await?;
                            let _ = state
                                .control
                                .delivery_bus
                                .commit_delivery(DeliveryAttempt {
                                    thread_key: record.metadata.thread_key.clone(),
                                    session_id: existing_thread_id.to_owned(),
                                    turn_id: turn_id_slot.lock().await.clone(),
                                    provisional_key: Some(plan_key),
                                    channel: DeliveryChannel::Telegram,
                                    kind: DeliveryKind::SystemNotice,
                                    executor: "telegram_thread_flow".to_owned(),
                                    transport_ref: None,
                                    report_json: serde_json::json!({
                                        "targets": [{
                                            "type": "telegram_plan_prompt",
                                            "target_ref": format!(
                                                "chat:{}/thread:{}",
                                                record.metadata.chat_id,
                                                thread_id_to_i32(thread_id)
                                            ),
                                            "state": "committed",
                                        }]
                                    }),
                                })
                                .await;
                        }
                    }
                    dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
                }
            }
        }
        Err(error) => {
            let _ = record_bot_status_event(
                &workspace_path,
                "bot_turn_failed",
                Some(existing_thread_id),
                None,
                Some("text turn failed"),
            )
            .await;
            error!(
                event = "telegram.thread.message.codex_failed",
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                codex_thread_id = existing_thread_id,
                error = %error,
                "codex turn failed for thread message"
            );
            if is_nonfatal_collaboration_mode_error(&error) {
                send_scoped_warning_message(bot, chat_id, Some(thread_id), error.to_string())
                    .await?;
                return Ok(());
            }
            let record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    format!("Codex turn failed: {error}"),
                    None,
                )
                .await?;
            send_scoped_warning_message(
                bot,
                chat_id,
                Some(thread_id),
                "Codex session is unavailable. Use /repair_session to retry or /new_session to start a fresh one.",
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(
                bot,
                &state.repository,
                &record,
                "thread_message_codex_failed",
            )
            .await;
        }
    }

    Ok(())
}

pub(crate) async fn launch_plan_implementation_turn(
    bot: &Bot,
    state: &AppState,
    message: &Message,
) -> Result<()> {
    let thread_id = message
        .thread_id
        .context("thread message missing thread id")?;
    let record = state
        .repository
        .get_thread(message.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    let session = state.repository.read_session_binding(&record).await?;
    let (record, session) = state
        .control
        .session_routing_service()
        .maybe_route_telegram_input_to_tui_session(record, session)
        .await?
        .into_record_session();
    let (resolved_state, blocking_snapshot) =
        resolve_busy_gate_state(state, &record, session.as_ref()).await?;
    if resolved_state.is_archived() {
        anyhow::bail!("workspace is archived");
    }
    let Some(existing_thread_id) = usable_bound_session_id(resolved_state, session.as_ref()) else {
        anyhow::bail!("workspace is missing a usable session");
    };
    if let Some(busy) = blocking_snapshot.as_ref() {
        anyhow::bail!("{}", status_sync::busy_text_message(busy, false));
    }
    let workspace_path = state
        .control
        .workspace_runtime_service()
        .ensure_bound_workspace_runtime(session.as_ref().context("missing binding")?)
        .await?;
    let record = state
        .repository
        .update_session_collaboration_mode(record, CollaborationMode::Default)
        .await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            PLAN_IMPLEMENTATION_MESSAGE,
            None,
        )
        .await?;
    let _ = state
        .repository
        .append_transcript_mirror(
            &record,
            &TranscriptMirrorEntry {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                session_id: existing_thread_id.to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: PLAN_IMPLEMENTATION_MESSAGE.to_owned(),
            },
        )
        .await?;
    let prompt_occurred_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let user_echo_key = provisional_key_for_text(
        existing_thread_id,
        DeliveryKind::UserEcho,
        PLAN_IMPLEMENTATION_MESSAGE,
        &prompt_occurred_at,
    );
    let _ = state
        .control
        .delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: record.metadata.thread_key.clone(),
            session_id: existing_thread_id.to_owned(),
            turn_id: None,
            provisional_key: Some(user_echo_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::UserEcho,
            owner: "telegram_plan_callback".to_owned(),
        })
        .await?;
    let _ = state
        .control
        .delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key: record.metadata.thread_key.clone(),
            session_id: existing_thread_id.to_owned(),
            turn_id: None,
            provisional_key: Some(user_echo_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::UserEcho,
            executor: "telegram_plan_callback".to_owned(),
            transport_ref: Some("callback:plan_implement".to_owned()),
            report_json: serde_json::json!({
                "targets": [{
                    "type": "telegram_plan_callback",
                    "target_ref": format!(
                        "chat:{}/thread:{}",
                        record.metadata.chat_id,
                        thread_id_to_i32(thread_id)
                    ),
                    "state": "committed",
                }]
            }),
        })
        .await;
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(PLAN_IMPLEMENTATION_MESSAGE),
    )
    .await?;
    spawn_text_turn(
        bot.clone(),
        state.clone(),
        record,
        message.chat.id,
        thread_id,
        workspace_path,
        existing_thread_id.to_owned(),
        user_echo_key,
        PLAN_IMPLEMENTATION_MESSAGE.to_owned(),
        CollaborationMode::Default,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        LaunchCommandTarget, build_workspace_launch_config, parse_execution_mode_argument,
        parse_launch_command_target, persist_collaboration_mode_change,
        render_stop_started_message, render_working_session_records, render_working_sessions,
    };
    use crate::collaboration_mode::CollaborationMode;
    use crate::config::{AppConfig, RuntimeConfig, TelegramConfig};
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::hcodex_ingress::HcodexIngressManager;
    use crate::repository::{
        ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorOrigin, TranscriptMirrorPhase,
        TranscriptMirrorRole,
    };
    use crate::runtime_control::{RuntimeControlContext, RuntimeOwnershipMode};
    use crate::runtime_protocol::{WorkingSessionRecordKind, WorkingSessionRecordView};
    use crate::telegram_runtime::AppState;
    use crate::workspace_status::{
        SessionActivitySource, SessionCurrentStatus, WorkspaceStatusCache, WorkspaceStatusPhase,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-thread-flow-test-{}", Uuid::new_v4()))
    }

    #[test]
    fn thread_flow_module_compiles_without_attach_helpers() {}

    #[test]
    fn launch_command_parser_accepts_new_current_and_resume() {
        assert_eq!(
            parse_launch_command_target("new"),
            Some(LaunchCommandTarget::New)
        );
        assert_eq!(
            parse_launch_command_target("current"),
            Some(LaunchCommandTarget::Current)
        );
        assert_eq!(
            parse_launch_command_target("resume thr_123"),
            Some(LaunchCommandTarget::Resume("thr_123".to_owned()))
        );
        assert_eq!(parse_launch_command_target("resume"), None);
        assert_eq!(parse_launch_command_target("unknown"), None);
    }

    #[test]
    fn execution_mode_argument_parser_accepts_known_aliases() {
        assert_eq!(
            parse_execution_mode_argument("full_auto"),
            Some(ExecutionMode::FullAuto)
        );
        assert_eq!(
            parse_execution_mode_argument("full-auto"),
            Some(ExecutionMode::FullAuto)
        );
        assert_eq!(
            parse_execution_mode_argument("yolo"),
            Some(ExecutionMode::Yolo)
        );
        assert_eq!(parse_execution_mode_argument("plan"), None);
    }

    #[test]
    fn working_session_renderers_show_expected_commands() {
        let binding = crate::repository::SessionBinding::fresh(
            Some("/tmp/workspace".to_owned()),
            Some("thr_current".to_owned()),
            SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
        );
        let summaries = vec![crate::runtime_protocol::WorkingSessionSummaryView {
            session_id: "thr_current".to_owned(),
            thread_key: "thread-1".to_owned(),
            workspace_cwd: "/tmp/workspace".to_owned(),
            started_at: Some("2026-03-25T00:00:00.000Z".to_owned()),
            updated_at: "2026-03-25T00:01:00.000Z".to_owned(),
            run_status: "running".to_owned(),
            run_phase: "turn_finalizing".to_owned(),
            origins_seen: vec![
                TranscriptMirrorOrigin::Telegram,
                TranscriptMirrorOrigin::Tui,
            ],
            record_count: 3,
            tool_use_count: 1,
            has_final_reply: true,
            last_error: None,
        }];
        let rendered_summaries = render_working_sessions(&binding, &summaries);
        assert!(rendered_summaries.contains("/session_log <session_id>"));
        assert!(rendered_summaries.contains("thr_current"));
        assert!(rendered_summaries.contains("current"));
        assert!(rendered_summaries.contains("running/finalizing"));

        let rendered_records = render_working_session_records(
            "thr_current",
            &[WorkingSessionRecordView {
                timestamp: "2026-03-25T00:00:00.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                kind: WorkingSessionRecordKind::ProcessTool,
                origin: Some(TranscriptMirrorOrigin::Telegram),
                role: Some(TranscriptMirrorRole::Assistant),
                summary: "Tool call finished".to_owned(),
                text: "Tool call finished".to_owned(),
                delivery: Some(TranscriptMirrorDelivery::Process),
                phase: Some(TranscriptMirrorPhase::Tool),
                source_ref: "transcript_mirror".to_owned(),
            }],
        );
        assert!(rendered_records.contains("process_tool"));
        assert!(rendered_records.contains("Tool call finished"));
    }

    #[test]
    fn stop_started_message_is_idempotent_for_finalizing_turns() {
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

        assert!(render_stop_started_message(&snapshot).contains("already requested"));
    }

    #[tokio::test]
    async fn collaboration_mode_change_is_persisted_and_logged() {
        let root = temp_path();
        let repository = ThreadRepository::open(&root).await.unwrap();
        let record = repository
            .create_thread(1, 7, "Title".to_owned())
            .await
            .unwrap();
        let record = repository
            .bind_workspace(
                record,
                "/tmp/workspace".to_owned(),
                "thr_current".to_owned(),
                SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            )
            .await
            .unwrap();

        let state = AppState {
            config: AppConfig {
                telegram: TelegramConfig {
                    telegram_token: "test".to_owned(),
                    authorized_user_ids: HashSet::from([7_i64]),
                },
                stream_edit_interval_ms: 10,
                stream_message_max_chars: 1000,
                command_output_tail_chars: 1000,
                workspace_status_poll_interval_ms: 1000,
                runtime: RuntimeConfig {
                    data_root_path: root.clone(),
                    codex_working_directory: root.clone(),
                    codex_model: None,
                    debug_log_path: root.join("debug.jsonl"),
                    management_bind_addr: "127.0.0.1:38420".parse().unwrap(),
                },
            },
            repository: repository.clone(),
            codex: crate::codex::CodexRunner::new(None),
            control: RuntimeControlContext {
                runtime: RuntimeConfig {
                    data_root_path: root.clone(),
                    codex_working_directory: root.clone(),
                    codex_model: None,
                    debug_log_path: root.join("debug.jsonl"),
                    management_bind_addr: "127.0.0.1:38420".parse().unwrap(),
                },
                repository: repository.clone(),
                delivery_bus: crate::delivery_bus::DeliveryBusCoordinator::new(&root)
                    .await
                    .unwrap(),
                codex: crate::codex::CodexRunner::new(None),
                app_server_runtime: crate::app_server_runtime::WorkspaceRuntimeManager::new(),
                hcodex_ingress: Some(HcodexIngressManager::new(repository.clone())),
                seed_template_path: root.join("seed.md"),
                runtime_ownership_mode: RuntimeOwnershipMode::SelfManaged,
            },
            interactive_requests: crate::interactive::InteractiveRequestRegistry::new(),
            workspace_status_cache: WorkspaceStatusCache::new(),
        };

        let record = persist_collaboration_mode_change(&state, record, CollaborationMode::Plan)
            .await
            .unwrap();
        let binding = repository
            .read_session_binding(&record)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.current_collaboration_mode,
            Some(CollaborationMode::Plan)
        );

        let content = tokio::fs::read_to_string(&record.log_path).await.unwrap();
        assert!(content.contains("Collaboration mode changed to `plan`."));
    }

    #[tokio::test]
    async fn workspace_launch_config_uses_current_and_recent_sessions() {
        let root = temp_path();
        let workspace = root.join("workspace");
        tokio::fs::create_dir_all(workspace.join(".threadbridge/bin"))
            .await
            .unwrap();
        tokio::fs::write(workspace.join(".threadbridge/bin/hcodex"), "#!/bin/sh\n")
            .await
            .unwrap();

        let repository = ThreadRepository::open(&root).await.unwrap();
        let record = repository
            .create_thread(1, 7, "Title".to_owned())
            .await
            .unwrap();
        let record = repository
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            )
            .await
            .unwrap();
        let _ = repository
            .set_tui_active_session_for_thread_key(&record.metadata.thread_key, "thr_recent")
            .await
            .unwrap();
        let binding = repository
            .read_session_binding(&record)
            .await
            .unwrap()
            .unwrap();

        let state = AppState {
            config: AppConfig {
                telegram: TelegramConfig {
                    telegram_token: "test".to_owned(),
                    authorized_user_ids: HashSet::from([7_i64]),
                },
                stream_edit_interval_ms: 10,
                stream_message_max_chars: 1000,
                command_output_tail_chars: 1000,
                workspace_status_poll_interval_ms: 1000,
                runtime: RuntimeConfig {
                    data_root_path: root.clone(),
                    codex_working_directory: root.clone(),
                    codex_model: None,
                    debug_log_path: root.join("debug.jsonl"),
                    management_bind_addr: "127.0.0.1:38420".parse().unwrap(),
                },
            },
            repository: repository.clone(),
            codex: crate::codex::CodexRunner::new(None),
            control: RuntimeControlContext {
                runtime: RuntimeConfig {
                    data_root_path: root.clone(),
                    codex_working_directory: root.clone(),
                    codex_model: None,
                    debug_log_path: root.join("debug.jsonl"),
                    management_bind_addr: "127.0.0.1:38420".parse().unwrap(),
                },
                repository: repository.clone(),
                delivery_bus: crate::delivery_bus::DeliveryBusCoordinator::new(&root)
                    .await
                    .unwrap(),
                codex: crate::codex::CodexRunner::new(None),
                app_server_runtime: crate::app_server_runtime::WorkspaceRuntimeManager::new(),
                hcodex_ingress: Some(HcodexIngressManager::new(repository.clone())),
                seed_template_path: root.join("seed.md"),
                runtime_ownership_mode: RuntimeOwnershipMode::SelfManaged,
            },
            interactive_requests: crate::interactive::InteractiveRequestRegistry::new(),
            workspace_status_cache: WorkspaceStatusCache::new(),
        };

        let config = build_workspace_launch_config(&state, &record, &binding)
            .await
            .unwrap();

        assert!(config.hcodex_available);
        assert!(config.launch_new_command.contains("--thread-key"));
        assert!(
            config
                .launch_current_command
                .as_deref()
                .is_some_and(|command| command.contains("resume 'thr_current'"))
        );
        assert!(
            config
                .launch_resume_commands
                .iter()
                .any(|command| command.contains("resume 'thr_recent'"))
        );
    }
}
