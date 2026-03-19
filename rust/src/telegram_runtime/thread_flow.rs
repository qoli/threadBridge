use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use teloxide::payloads::setters::*;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant, sleep};
use tracing::{error, info};

use crate::workspace_status::WorkspaceAggregateStatus;

use super::final_reply::send_final_assistant_reply;
use super::media::{self, dispatch_workspace_telegram_outbox};
use super::preview::{PreviewHeartbeat, TurnPreviewController, TypingHeartbeat};
use super::restore;
use super::status_sync;
use super::*;

fn workspace_for_codex(path: PathBuf) -> CodexWorkspace {
    CodexWorkspace {
        working_directory: path,
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

async fn start_fresh_binding(
    state: &AppState,
    record: ThreadRecord,
    workspace_path: PathBuf,
) -> Result<ThreadRecord> {
    ensure_workspace_runtime(
        &state.config.runtime.codex_working_directory,
        &state.config.runtime.data_root_path,
        &state.seed_template_path,
        &workspace_path,
    )
    .await?;
    let binding = state
        .codex
        .start_thread(&workspace_for_codex(workspace_path))
        .await?;
    state
        .repository
        .bind_workspace(record, binding.cwd, binding.thread_id)
        .await
}

async fn busy_snapshot_for_binding(
    state: &AppState,
    binding: &SessionBinding,
) -> Result<Option<crate::workspace_status::BusySelectedSessionStatus>> {
    let workspace_path = workspace_path_from_binding(binding)?;
    let Some(session_id) = usable_bound_session_id(Some(binding)) else {
        return Ok(None);
    };
    busy_selected_session_status(&state.workspace_status_cache, &workspace_path, session_id).await
}

pub(crate) async fn current_thread_cli_owner_claim(
    _state: &AppState,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<Option<CliOwnerClaim>> {
    if binding.attachment_state == SessionAttachmentState::CliHandoff {
        return Ok(None);
    }
    let workspace_path = workspace_path_from_binding(binding)?;
    let Some(claim) = read_cli_owner_claim(&workspace_path).await? else {
        return Ok(None);
    };
    if claim.thread_key != record.metadata.thread_key {
        return Ok(None);
    }
    Ok(Some(claim))
}

fn render_live_cli_session_choices(
    sessions: &[crate::workspace_status::SessionCurrentStatus],
    current_session_id: Option<&str>,
) -> String {
    let mut lines = vec![
        "Multiple live CLI sessions are available in this workspace.".to_owned(),
        "Run /attach_cli_session <session-id> with one of these ids:".to_owned(),
        String::new(),
    ];
    for session in sessions {
        let current = if current_session_id == Some(session.session_id.as_str()) {
            " (current)"
        } else {
            ""
        };
        let summary = session
            .summary
            .as_deref()
            .map(|value| format!(" - {value}"))
            .unwrap_or_default();
        lines.push(format!(
            "- `{}`{} [{}]{}",
            session.session_id,
            current,
            match session.phase {
                crate::workspace_status::WorkspaceStatusPhase::ShellActive => "shell_active",
                crate::workspace_status::WorkspaceStatusPhase::TurnRunning => "turn_running",
                crate::workspace_status::WorkspaceStatusPhase::TurnFinalizing => "turn_finalizing",
                crate::workspace_status::WorkspaceStatusPhase::Idle => "idle",
            },
            summary
        ));
    }
    lines.join("\n")
}

async fn render_thread_info(state: &AppState, record: &ThreadRecord) -> Result<String> {
    let session = state.repository.read_session_binding(record).await?;
    let workspace_path = session
        .as_ref()
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(PathBuf::from);
    let aggregate = if let Some(path) = workspace_path.as_ref() {
        Some(crate::workspace_status::read_workspace_aggregate_status(path).await?)
    } else {
        None
    };
    let owner_claim = if let Some(path) = workspace_path.as_ref() {
        read_cli_owner_claim(path).await?
    } else {
        None
    };
    let marker = status_sync::cli_topic_marker_for_record(
        record,
        session.as_ref(),
        aggregate.as_ref(),
        owner_claim.as_ref(),
    );
    let selected_session_id = usable_bound_session_id(session.as_ref())
        .map(str::to_owned)
        .unwrap_or_else(|| "none".to_owned());
    let attachment_state = session
        .as_ref()
        .map(|binding| match binding.attachment_state {
            SessionAttachmentState::None => "none",
            SessionAttachmentState::CliHandoff => "cli_handoff",
        })
        .unwrap_or("none");
    let workspace = workspace_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unbound".to_owned());
    let owner_thread_key = owner_claim
        .as_ref()
        .map(|claim| claim.thread_key.as_str())
        .unwrap_or("none");
    let owner_session_id = owner_claim
        .as_ref()
        .and_then(|claim| claim.session_id.as_deref())
        .unwrap_or("none");
    let owner_flag = owner_claim
        .as_ref()
        .is_some_and(|claim| claim.thread_key == record.metadata.thread_key);

    Ok(format!(
        "thread_key: `{}`\nworkspace: `{}`\nselected_session_id: `{}`\nattachment_state: `{}`\nmarker: `{}`\nowner_thread: `{}`\nowner_session_id: `{}`\nis_owner_thread: `{}`",
        record.metadata.thread_key,
        workspace,
        selected_session_id,
        attachment_state,
        status_sync::cli_marker_label(marker),
        owner_thread_key,
        owner_session_id,
        if owner_flag { "yes" } else { "no" },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessRow {
    pid: u32,
    ppid: u32,
    pgid: u32,
    command: String,
}

fn parse_process_rows(ps_output: &str) -> Vec<ProcessRow> {
    ps_output
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(4, char::is_whitespace);
            let pid = parts.next()?.trim().parse().ok()?;
            let ppid = parts.next()?.trim().parse().ok()?;
            let pgid = parts.next()?.trim().parse().ok()?;
            let command = parts.next()?.trim().to_owned();
            if command.is_empty() {
                return None;
            }
            Some(ProcessRow {
                pid,
                ppid,
                pgid,
                command,
            })
        })
        .collect()
}

fn command_binary_name(command: &str) -> Option<&str> {
    let executable = command.split_whitespace().next()?;
    Path::new(executable).file_name()?.to_str()
}

fn resolve_codex_process(rows: &[ProcessRow], shell_pid: u32) -> Option<ProcessRow> {
    rows.iter()
        .filter(|row| row.ppid == shell_pid && command_binary_name(&row.command) == Some("codex"))
        .max_by_key(|row| row.pid)
        .cloned()
}

async fn list_process_rows() -> Result<Vec<ProcessRow>> {
    let output = TokioCommand::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,command="])
        .output()
        .await
        .context("failed to run ps for Codex CLI handoff")?;
    if !output.status.success() {
        bail!(
            "failed to inspect local process table for Codex CLI handoff: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_process_rows(&String::from_utf8_lossy(&output.stdout)))
}

async fn signal_process_group(pgid: u32, signal: &str) -> Result<()> {
    let status = TokioCommand::new("kill")
        .args([format!("-{signal}"), "--".to_owned(), format!("-{pgid}")])
        .status()
        .await
        .with_context(|| format!("failed to send {signal} to process group {pgid}"))?;
    if !status.success() {
        bail!("kill {signal} failed for process group {pgid}");
    }
    Ok(())
}

async fn wait_for_cli_session_to_stop(workspace_path: &Path, session_id: &str) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let Some(snapshot) = read_session_status(workspace_path, session_id).await? else {
            return Ok(true);
        };
        if !snapshot.live {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn terminate_cli_session_tui(
    workspace_path: &Path,
    target: &crate::workspace_status::SessionCurrentStatus,
) -> Result<()> {
    let shell_pid = target
        .shell_pid
        .context("live CLI session is missing shell_pid")?;
    let process =
        resolve_codex_process(&list_process_rows().await?, shell_pid).with_context(|| {
            format!("failed to locate Codex CLI process under shell pid {shell_pid}")
        })?;

    info!(
        event = "telegram.attach.kill_cli_session",
        workspace = %workspace_path.display(),
        session_id = %target.session_id,
        shell_pid,
        codex_pid = process.pid,
        process_group = process.pgid,
        signal = "TERM",
        "starting CLI handoff by terminating local Codex TUI"
    );
    signal_process_group(process.pgid, "TERM").await?;
    if wait_for_cli_session_to_stop(workspace_path, &target.session_id).await? {
        info!(
            event = "telegram.attach.kill_cli_session",
            workspace = %workspace_path.display(),
            session_id = %target.session_id,
            shell_pid,
            codex_pid = process.pid,
            process_group = process.pgid,
            signal = "TERM",
            result = "stopped",
            "local Codex TUI stopped after TERM"
        );
        return Ok(());
    }

    info!(
        event = "telegram.attach.kill_cli_session",
        workspace = %workspace_path.display(),
        session_id = %target.session_id,
        shell_pid,
        codex_pid = process.pid,
        process_group = process.pgid,
        signal = "KILL",
        "CLI handoff still active after TERM; escalating to KILL"
    );
    signal_process_group(process.pgid, "KILL").await?;
    if wait_for_cli_session_to_stop(workspace_path, &target.session_id).await? {
        info!(
            event = "telegram.attach.kill_cli_session",
            workspace = %workspace_path.display(),
            session_id = %target.session_id,
            shell_pid,
            codex_pid = process.pid,
            process_group = process.pgid,
            signal = "KILL",
            result = "stopped",
            "local Codex TUI stopped after KILL"
        );
        return Ok(());
    }

    bail!(
        "CLI session {} did not shut down cleanly after TERM/KILL",
        target.session_id
    );
}

pub(crate) async fn selected_live_cli_owned_session(
    state: &AppState,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<Option<CliOwnerClaim>> {
    if binding.attachment_state == SessionAttachmentState::CliHandoff {
        return Ok(None);
    }
    let Some(owner_claim) = current_thread_cli_owner_claim(state, record, binding).await? else {
        return Ok(None);
    };
    let workspace_path = workspace_path_from_binding(binding)?;
    let aggregate =
        read_workspace_status_with_cache(&state.workspace_status_cache, &workspace_path).await?;
    if cli_owner_claim_is_live(&aggregate, &owner_claim) {
        return Ok(Some(owner_claim));
    }
    Ok(None)
}

fn cli_owner_claim_is_live(
    aggregate: &WorkspaceAggregateStatus,
    owner_claim: &CliOwnerClaim,
) -> bool {
    aggregate
        .active_shell_pids
        .iter()
        .any(|shell_pid| *shell_pid == owner_claim.shell_pid)
        || owner_claim
            .session_id
            .as_deref()
            .is_some_and(|owner_session_id| {
                aggregate
                    .live_cli_session_ids
                    .iter()
                    .any(|session_id| session_id == owner_session_id)
            })
}

pub(crate) fn log_cli_owned_rejection(
    record: &ThreadRecord,
    binding: &SessionBinding,
    owner_claim: &CliOwnerClaim,
    surface: &str,
) {
    info!(
        event = "telegram.cli_owned.rejected",
        surface = surface,
        thread_key = %record.metadata.thread_key,
        workspace = binding.workspace_cwd.as_deref().unwrap_or("unbound"),
        selected_session_id = binding.selected_session_id.as_deref().unwrap_or("none"),
        owner_thread_key = %owner_claim.thread_key,
        owner_session_id = owner_claim.session_id.as_deref().unwrap_or("none"),
        "telegram request rejected because local Codex CLI owns the selected session"
    );
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
                "Control console.\nUse /new_thread to create a Telegram thread."
            } else {
                "Thread workspace.\nUse /bind_workspace <absolute-path> to attach a project."
            };
            send_scoped_message(bot, msg.chat.id, msg.thread_id, text).await?;
        }
        Command::NewThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /new_thread from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let title = format!("Thread {}", chrono::Local::now().format("%m-%d %H:%M"));
            let topic = bot.create_forum_topic(msg.chat.id, title.clone()).await?;
            let record = state
                .repository
                .create_thread(
                    msg.chat.id.0,
                    thread_id_to_i32(topic.thread_id),
                    title.clone(),
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                None,
                format!("Created thread \"{}\".", topic.name),
            )
            .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    "Telegram thread created. Awaiting workspace binding.",
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(topic.thread_id),
                "Thread created.\n\nUse /bind_workspace <absolute-path> in this thread.",
            )
            .await?;
        }
        Command::BindWorkspace => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /bind_workspace <absolute-path> inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let Some(argument) = command_argument_text(msg, "bind_workspace") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Usage: /bind_workspace <absolute-path>",
                )
                .await?;
                return Ok(());
            };
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ThreadStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let existing_binding = state.repository.read_session_binding(&record).await?;
            if let Some(binding) = existing_binding.as_ref()
                && binding.workspace_cwd.is_some()
                && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            if let Some(binding) = existing_binding.as_ref()
                && binding.workspace_cwd.is_some()
                && let Some(owner_claim) =
                    selected_live_cli_owned_session(state, &record, binding).await?
            {
                log_cli_owned_rejection(&record, binding, &owner_claim, "thread_command_bind");
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::cli_owned_command_message(),
                )
                .await?;
                return Ok(());
            }

            let workspace_path = resolve_workspace_argument(argument).await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = start_fresh_binding(state, record.clone(), workspace_path.clone()).await;
            typing.stop().await;

            match result {
                Ok(record) => {
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Bound Telegram thread to workspace {} and started a fresh Codex thread.",
                                workspace_path.display()
                            ),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!(
                            "Bound workspace: `{}`\n\nTo sync local Bash Codex sessions in this workspace, run:\n`source {}/.threadbridge/shell/codex-sync.bash`\nThen start managed CLI with:\n`hcodex`",
                            workspace_path.display(),
                            workspace_path.display()
                        ),
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(bot, state, &record).await;
                }
                Err(error) => {
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("Workspace bind failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::New => {
            if is_control_chat(msg) {
                send_scoped_message(bot, msg.chat.id, None, "Use /new inside a thread.").await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is not bound yet. Use /bind_workspace <absolute-path>.",
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = busy_snapshot_for_binding(state, binding).await? {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            if let Some(owner_claim) =
                selected_live_cli_owned_session(state, &record, binding).await?
            {
                log_cli_owned_rejection(&record, binding, &owner_claim, "thread_command_new");
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::cli_owned_command_message(),
                )
                .await?;
                return Ok(());
            }
            let workspace_path = workspace_path_from_binding(binding)?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = start_fresh_binding(state, record.clone(), workspace_path.clone()).await;
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
                    let _ = status_sync::refresh_thread_topic_title(bot, state, &record).await;
                }
                Err(error) => {
                    let _ = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("New session failed: {error}"),
                    )
                    .await?;
                }
            }
        }
        Command::ReconnectCodex => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /reconnect_codex inside a thread.",
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
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(binding) = session.as_ref()
                && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            if let Some(binding) = session.as_ref()
                && let Some(owner_claim) =
                    selected_live_cli_owned_session(state, &record, binding).await?
            {
                log_cli_owned_rejection(
                    &record,
                    binding,
                    &owner_claim,
                    "thread_command_reconnect_codex",
                );
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::cli_owned_command_message(),
                )
                .await?;
                return Ok(());
            }
            let workspace_path =
                ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?)
                    .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnect = state
                .codex
                .reconnect_session(&workspace_for_codex(workspace_path), existing_thread_id)
                .await;
            typing.stop().await;

            match reconnect {
                Ok(()) => {
                    let updated = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
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
                        "Codex session reconnected for this thread.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(bot, state, &updated).await;
                }
                Err(error) => {
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session revalidation failed. Use /new to start a fresh one or /reconnect_codex to retry.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(bot, state, &updated).await;
                }
            }
        }
        Command::AttachCliSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /attach_cli_session inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ThreadStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let session = state.repository.read_session_binding(&record).await?;
            let Some(binding) = session.as_ref() else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(None),
                )
                .await?;
                return Ok(());
            };
            if let Some(busy) = busy_snapshot_for_binding(state, binding).await? {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }

            let workspace_path = ensure_bound_workspace_runtime(state, binding).await?;
            let live_sessions = list_live_cli_sessions(&workspace_path).await?;
            if live_sessions.is_empty() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "No live CLI sessions are available in this workspace.",
                )
                .await?;
                return Ok(());
            }

            let requested_session_id = command_argument_text(msg, "attach_cli_session");
            let selected_session_id = usable_bound_session_id(session.as_ref());
            let target = if let Some(requested_session_id) = requested_session_id {
                let Some(found) = live_sessions
                    .iter()
                    .find(|item| item.session_id == requested_session_id)
                else {
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        render_live_cli_session_choices(&live_sessions, selected_session_id),
                    )
                    .await?;
                    return Ok(());
                };
                found.clone()
            } else if live_sessions.len() == 1 {
                live_sessions[0].clone()
            } else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    render_live_cli_session_choices(&live_sessions, selected_session_id),
                )
                .await?;
                return Ok(());
            };

            if target.phase.is_turn_busy() {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "That CLI session is still running a turn. Wait for it to finish before attaching it to Telegram.",
                )
                .await?;
                return Ok(());
            }

            if let Some(owner) = state
                .repository
                .find_active_cli_handoff_owner(&target.workspace_cwd, &target.session_id)
                .await?
                && owner.conversation_key != record.conversation_key
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "Another Telegram thread already owns that attached CLI session.",
                )
                .await?;
                return Ok(());
            }

            let intent = default_attach_intent(
                &workspace_path,
                record.metadata.thread_key.clone(),
                target.session_id.clone(),
                target
                    .shell_pid
                    .context("live CLI session is missing shell_pid")?,
            );
            write_attach_intent(&workspace_path, &intent).await?;
            if let Err(error) = terminate_cli_session_tui(&workspace_path, &target).await {
                let _ = remove_attach_intent(&workspace_path).await;
                return Err(error);
            }
            let updated = state
                .repository
                .attach_cli_session_binding_session(record, target.session_id.clone())
                .await?;
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!(
                        "Attached this thread to live CLI session {} and handed ownership to Telegram.",
                        target.session_id
                    ),
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!(
                    "Attached this thread to live CLI session `{}` and switched control to Telegram.\n\nTo return to local CLI later, run:\n`hcodex resume {} --thread-key {}`",
                    target.session_id, target.session_id, updated.metadata.thread_key
                ),
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(bot, state, &updated).await;
        }
        Command::ThreadInfo => {
            if is_control_chat(msg) {
                send_scoped_message(bot, msg.chat.id, None, "Use /thread_info inside a thread.")
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
        Command::GenerateTitle => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /generate_title inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ThreadStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            if let Some(binding) = session.as_ref()
                && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
            {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::busy_command_message(&busy.snapshot),
                )
                .await?;
                return Ok(());
            }
            if let Some(binding) = session.as_ref()
                && let Some(owner_claim) =
                    selected_live_cli_owned_session(state, &record, binding).await?
            {
                log_cli_owned_rejection(
                    &record,
                    binding,
                    &owner_claim,
                    "thread_command_generate_title",
                );
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    status_sync::cli_owned_command_message(),
                )
                .await?;
                return Ok(());
            }
            let workspace_path =
                ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?)
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
                .generate_thread_title_from_session(
                    &workspace_for_codex(workspace_path.clone()),
                    existing_thread_id,
                )
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
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /new.",
                    )
                    .await?;
                    let _ = status_sync::refresh_thread_topic_title(bot, state, &updated).await;
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
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!("Generated title: {title}"),
                    None,
                )
                .await?;
            let _ = status_sync::refresh_thread_topic_title(bot, state, &updated).await;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Title updated: {title}"),
            )
            .await?;
        }
        Command::ArchiveThread => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /archive_thread inside a thread.",
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
                .append_log(&record, LogDirection::System, "Thread archived.", None)
                .await?;
        }
        Command::RestoreThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /restore_thread from the main private chat.",
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
            "Main private chat is the control console. Use /new_thread first.",
        )
        .await?;
        return Ok(());
    }

    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let mut record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if matches!(record.metadata.status, ThreadStatus::Archived) {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This thread is archived.",
        )
        .await?;
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint(session.as_ref()),
        )
        .await?;
        return Ok(());
    };
    let workspace_path =
        ensure_bound_workspace_runtime(state, session.as_ref().context("missing binding")?).await?;
    if let Some(binding) = session.as_ref()
        && let Some(busy) = busy_snapshot_for_binding(state, binding).await?
    {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::busy_text_message(&busy.snapshot, false),
        )
        .await?;
        return Ok(());
    }
    if let Some(binding) = session.as_ref()
        && let Some(owner_claim) = selected_live_cli_owned_session(state, &record, binding).await?
    {
        state
            .repository
            .append_log(
                &record,
                LogDirection::User,
                text.to_owned(),
                msg.from.as_ref().map(|user| user.id.0 as i64),
            )
            .await?;
        state
            .repository
            .append_log(
                &record,
                LogDirection::System,
                format!(
                    "Rejected while local Codex CLI owned the selected session. Owner thread: {}.",
                    owner_claim.thread_key
                ),
                None,
            )
            .await?;
        log_cli_owned_rejection(&record, binding, &owner_claim, "thread_text");
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            status_sync::cli_owned_text_message(false),
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
    state
        .repository
        .append_transcript_mirror(
            &record,
            &TranscriptMirrorEntry {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                session_id: existing_thread_id.to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                text: text.to_owned(),
            },
        )
        .await?;

    let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
    let preview = Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        msg.chat.id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    record_bot_status_event(
        &workspace_path,
        "bot_turn_started",
        Some(existing_thread_id),
        None,
        Some(text),
    )
    .await?;

    let result = state
        .codex
        .run_locked_prompt_with_events(
            &workspace_for_codex(workspace_path.clone()),
            existing_thread_id,
            text,
            |event| {
                let preview = preview.clone();
                async move {
                    preview.lock().await.consume(&event).await;
                }
            },
        )
        .await;
    preview_heartbeat.stop().await;
    typing.stop().await;

    match result {
        Ok(result) => {
            record_bot_status_event(
                &workspace_path,
                "bot_turn_completed",
                Some(existing_thread_id),
                None,
                Some(&result.final_response),
            )
            .await?;
            record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::Assistant,
                    result.final_response.clone(),
                    None,
                )
                .await?;
            state
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
                        text: result.final_response.clone(),
                    },
                )
                .await?;
            if !preview.lock().await.complete(&result.final_response).await {
                let final_text = if result.final_response.trim().is_empty() {
                    preview
                        .lock()
                        .await
                        .fallback_final_response()
                        .trim()
                        .to_owned()
                } else {
                    result.final_response
                };
                if !final_text.trim().is_empty() {
                    send_final_assistant_reply(bot, &record, Some(thread_id), &final_text).await?;
                }
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
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
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                "Codex session is unavailable. Use /reconnect_codex to retry or /new to start a fresh one.",
            )
            .await?;
            let _ = status_sync::refresh_thread_topic_title(bot, state, &record).await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        cli_owner_claim_is_live, command_binary_name, parse_process_rows, resolve_codex_process,
    };
    use crate::workspace_status::{CliOwnerClaim, WorkspaceAggregateStatus};

    #[test]
    fn parse_process_rows_keeps_full_command() {
        let rows =
            parse_process_rows("21345 21344 21345 -zsh\n33298 21345 33298 codex resume 019d032d\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].pid, 33298);
        assert_eq!(rows[1].command, "codex resume 019d032d");
    }

    #[test]
    fn resolve_codex_process_prefers_shell_child_named_codex() {
        let rows = parse_process_rows(
            "21345 21344 21345 -zsh\n33298 21345 33298 codex resume abc\n33299 21345 33299 rg codex\n",
        );
        let process = resolve_codex_process(&rows, 21345).expect("codex child");
        assert_eq!(process.pid, 33298);
        assert_eq!(command_binary_name(&process.command), Some("codex"));
    }

    #[test]
    fn cli_owner_claim_is_live_when_shell_is_active_before_session_start() {
        let aggregate = WorkspaceAggregateStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".into(),
            live_cli_session_ids: Vec::new(),
            active_shell_pids: vec![53249],
            updated_at: "2026-03-19T00:00:00.000Z".into(),
        };
        let owner_claim = CliOwnerClaim {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".into(),
            thread_key: "thread-1".into(),
            shell_pid: 53249,
            session_id: None,
            started_at: "2026-03-19T00:00:00.000Z".into(),
            updated_at: "2026-03-19T00:00:00.000Z".into(),
        };
        assert!(cli_owner_claim_is_live(&aggregate, &owner_claim));
    }

    #[test]
    fn cli_owner_claim_is_not_live_when_shell_and_session_are_both_absent() {
        let aggregate = WorkspaceAggregateStatus {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".into(),
            live_cli_session_ids: vec!["session-b".into()],
            active_shell_pids: vec![12345],
            updated_at: "2026-03-19T00:00:00.000Z".into(),
        };
        let owner_claim = CliOwnerClaim {
            schema_version: 2,
            workspace_cwd: "/tmp/workspace".into(),
            thread_key: "thread-1".into(),
            shell_pid: 53249,
            session_id: Some("session-a".into()),
            started_at: "2026-03-19T00:00:00.000Z".into(),
            updated_at: "2026-03-19T00:00:00.000Z".into(),
        };
        assert!(!cli_owner_claim_is_live(&aggregate, &owner_claim));
    }
}
