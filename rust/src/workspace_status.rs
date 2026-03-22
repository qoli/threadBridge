use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use uuid::Uuid;

const STATUS_SCHEMA_VERSION: u32 = 2;
const STATUS_DIR: &str = ".threadbridge/state/shared-runtime";
const CURRENT_FILE: &str = "current.json";
const EVENTS_FILE: &str = "events.jsonl";
const SESSIONS_DIR: &str = "sessions";
const LOCAL_SESSION_FILE: &str = "local-session.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusOwner {
    Local,
    Bot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatusPhase {
    Idle,
    ShellActive,
    TurnRunning,
    TurnFinalizing,
}

impl WorkspaceStatusPhase {
    pub fn is_turn_busy(self) -> bool {
        matches!(self, Self::TurnRunning | Self::TurnFinalizing)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCurrentStatus {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub session_id: String,
    pub owner: SessionStatusOwner,
    pub live: bool,
    pub phase: WorkspaceStatusPhase,
    pub shell_pid: Option<u32>,
    #[serde(default)]
    pub child_pid: Option<u32>,
    #[serde(default)]
    pub child_pgid: Option<u32>,
    #[serde(default)]
    pub child_command: Option<String>,
    pub client: Option<String>,
    pub turn_id: Option<String>,
    pub summary: Option<String>,
    pub updated_at: String,
}

impl SessionCurrentStatus {
    pub fn is_live_local_session(&self) -> bool {
        self.owner == SessionStatusOwner::Local && self.live
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceAggregateStatus {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub live_local_session_ids: Vec<String>,
    #[serde(default)]
    pub active_shell_pids: Vec<u32>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatusEventRecord {
    pub schema_version: u32,
    pub event: String,
    pub source: SessionStatusOwner,
    pub workspace_cwd: String,
    pub occurred_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalSessionClaim {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub thread_key: String,
    pub shell_pid: u32,
    pub session_id: Option<String>,
    #[serde(default)]
    pub child_pid: Option<u32>,
    #[serde(default)]
    pub child_pgid: Option<u32>,
    #[serde(default)]
    pub child_command: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatusCache {
    inner: Arc<RwLock<HashMap<String, WorkspaceAggregateStatus>>>,
}

#[derive(Debug, Clone)]
pub struct BusySelectedSessionStatus {
    pub workspace_path: PathBuf,
    pub snapshot: SessionCurrentStatus,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn canonical_workspace_string(workspace_path: &Path) -> String {
    workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string()
}

fn status_dir(workspace_path: &Path) -> PathBuf {
    workspace_path.join(STATUS_DIR)
}

fn sessions_dir(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(SESSIONS_DIR)
}

pub fn local_session_claim_path(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(LOCAL_SESSION_FILE)
}

pub fn current_status_path(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(CURRENT_FILE)
}

pub fn events_path(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(EVENTS_FILE)
}

fn session_file_name(session_id: &str) -> String {
    let mut name = String::with_capacity(session_id.len() + 5);
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    name.push_str(".json");
    name
}

pub fn session_status_path(workspace_path: &Path, session_id: &str) -> PathBuf {
    sessions_dir(workspace_path).join(session_file_name(session_id))
}

pub fn default_local_session_claim(
    workspace_path: &Path,
    thread_key: impl Into<String>,
    shell_pid: u32,
) -> LocalSessionClaim {
    let now = now_iso();
    LocalSessionClaim {
        schema_version: STATUS_SCHEMA_VERSION,
        workspace_cwd: canonical_workspace_string(workspace_path),
        thread_key: thread_key.into(),
        shell_pid,
        session_id: None,
        child_pid: None,
        child_pgid: None,
        child_command: None,
        started_at: now.clone(),
        updated_at: now,
    }
}

pub fn default_workspace_status(workspace_path: &Path) -> WorkspaceAggregateStatus {
    WorkspaceAggregateStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        workspace_cwd: canonical_workspace_string(workspace_path),
        live_local_session_ids: Vec::new(),
        active_shell_pids: Vec::new(),
        updated_at: now_iso(),
    }
}

pub fn default_session_status(
    workspace_path: &Path,
    session_id: &str,
    owner: SessionStatusOwner,
) -> SessionCurrentStatus {
    SessionCurrentStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        workspace_cwd: canonical_workspace_string(workspace_path),
        session_id: session_id.to_owned(),
        owner,
        live: matches!(owner, SessionStatusOwner::Local),
        phase: WorkspaceStatusPhase::Idle,
        shell_pid: None,
        child_pid: None,
        child_pgid: None,
        child_command: None,
        client: None,
        turn_id: None,
        summary: None,
        updated_at: now_iso(),
    }
}

async fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let serialized = format!("{}\n", serde_json::to_string_pretty(value)?);
    let parent = path
        .parent()
        .context("workspace status path is missing parent directory")?;
    fs::create_dir_all(parent).await?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("status"),
        Uuid::new_v4()
    ));
    fs::write(&temp_path, serialized).await?;
    fs::rename(&temp_path, path).await?;
    Ok(())
}

async fn write_workspace_status(
    workspace_path: &Path,
    status: &WorkspaceAggregateStatus,
) -> Result<()> {
    atomic_write_json(&current_status_path(workspace_path), status).await
}

async fn write_session_status(workspace_path: &Path, status: &SessionCurrentStatus) -> Result<()> {
    atomic_write_json(
        &session_status_path(workspace_path, &status.session_id),
        status,
    )
    .await
}

pub async fn write_local_session_claim(
    workspace_path: &Path,
    claim: &LocalSessionClaim,
) -> Result<()> {
    atomic_write_json(&local_session_claim_path(workspace_path), claim).await
}

pub async fn remove_local_session_claim(workspace_path: &Path) -> Result<()> {
    let path = local_session_claim_path(workspace_path);
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

pub async fn append_status_event(
    workspace_path: &Path,
    event: &WorkspaceStatusEventRecord,
) -> Result<()> {
    let path = events_path(workspace_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    file.write_all(serde_json::to_string(event)?.as_bytes())
        .await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

async fn last_status_event(workspace_path: &Path) -> Result<Option<WorkspaceStatusEventRecord>> {
    let path = events_path(workspace_path);
    let contents = match fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let Some(line) = contents.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(line).with_context(|| {
        format!("failed to parse trailing event from {}", path.display())
    })?))
}

async fn should_skip_duplicate_turn_completed_event(
    workspace_path: &Path,
    session_id: &str,
    turn_id: Option<&str>,
) -> Result<bool> {
    let Some(turn_id) = turn_id else {
        return Ok(false);
    };
    let Some(previous) = last_status_event(workspace_path).await? else {
        return Ok(false);
    };
    if previous.event != "turn_completed" || previous.source != SessionStatusOwner::Local {
        return Ok(false);
    }
    Ok(
        previous.payload.get("thread-id").and_then(Value::as_str) == Some(session_id)
            && previous.payload.get("turn-id").and_then(Value::as_str) == Some(turn_id),
    )
}

pub async fn ensure_workspace_status_surface(workspace_path: &Path) -> Result<()> {
    let dir = status_dir(workspace_path);
    fs::create_dir_all(&dir).await?;
    fs::create_dir_all(sessions_dir(workspace_path)).await?;

    let current_path = current_status_path(workspace_path);
    if !fs::try_exists(&current_path).await? {
        write_workspace_status(workspace_path, &default_workspace_status(workspace_path)).await?;
    }

    let events_path = events_path(workspace_path);
    if !fs::try_exists(&events_path).await? {
        fs::write(events_path, "").await?;
    }

    Ok(())
}

pub async fn read_workspace_aggregate_status(
    workspace_path: &Path,
) -> Result<WorkspaceAggregateStatus> {
    let path = current_status_path(workspace_path);
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(default_workspace_status(workspace_path))
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub async fn read_session_status(
    workspace_path: &Path,
    session_id: &str,
) -> Result<Option<SessionCurrentStatus>> {
    let path = session_status_path(workspace_path, session_id);
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub async fn read_local_session_claim(workspace_path: &Path) -> Result<Option<LocalSessionClaim>> {
    let path = local_session_claim_path(workspace_path);
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

async fn list_all_session_statuses(workspace_path: &Path) -> Result<Vec<SessionCurrentStatus>> {
    let dir_path = sessions_dir(workspace_path);
    if !fs::try_exists(&dir_path).await? {
        return Ok(Vec::new());
    }
    let mut dir = fs::read_dir(dir_path).await?;
    let mut sessions: Vec<SessionCurrentStatus> = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let content = fs::read_to_string(entry.path()).await?;
        sessions.push(serde_json::from_str(&content)?);
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

async fn refresh_workspace_aggregate_status(
    workspace_path: &Path,
    mut aggregate: WorkspaceAggregateStatus,
) -> Result<WorkspaceAggregateStatus> {
    let sessions = list_all_session_statuses(workspace_path).await?;
    let mut live_local_session_ids = sessions
        .iter()
        .filter(|session| session.is_live_local_session())
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    live_local_session_ids.sort();
    live_local_session_ids.dedup();
    aggregate.schema_version = STATUS_SCHEMA_VERSION;
    aggregate.workspace_cwd = canonical_workspace_string(workspace_path);
    aggregate.live_local_session_ids = live_local_session_ids;
    aggregate.updated_at = now_iso();
    write_workspace_status(workspace_path, &aggregate).await?;
    Ok(aggregate)
}

fn summarize_prompt(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut summary = String::new();
    for ch in trimmed.chars().take(96) {
        summary.push(ch);
    }
    if trimmed.chars().count() > 96 {
        summary.push_str("...");
    }
    Some(summary)
}

pub async fn list_live_local_sessions(workspace_path: &Path) -> Result<Vec<SessionCurrentStatus>> {
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let mut sessions = Vec::new();
    for session_id in aggregate.live_local_session_ids {
        if let Some(session) = read_session_status(workspace_path, &session_id).await? {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

pub async fn record_bot_status_event(
    workspace_path: &Path,
    event_name: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    summary: Option<&str>,
) -> Result<SessionCurrentStatus> {
    ensure_workspace_status_surface(workspace_path).await?;
    let session_id = session_id.context("bot workspace status event requires a session_id")?;
    let mut current = read_session_status(workspace_path, session_id)
        .await?
        .unwrap_or_else(|| {
            default_session_status(workspace_path, session_id, SessionStatusOwner::Bot)
        });
    let payload = json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "summary": summary,
    });

    current.schema_version = STATUS_SCHEMA_VERSION;
    current.workspace_cwd = canonical_workspace_string(workspace_path);
    current.owner = SessionStatusOwner::Bot;
    current.live = false;
    current.shell_pid = None;
    current.child_pid = None;
    current.child_pgid = None;
    current.child_command = None;
    current.turn_id = turn_id.map(str::to_owned);
    current.updated_at = now_iso();

    let next = match event_name {
        "bot_turn_started" => {
            current.phase = WorkspaceStatusPhase::TurnRunning;
            current.client = Some("threadbridge".to_owned());
            current.summary = summary.and_then(summarize_prompt);
            current
        }
        "bot_turn_completed" | "bot_turn_failed" | "bot_turn_recovered" => {
            current.phase = WorkspaceStatusPhase::Idle;
            current.client = Some("threadbridge".to_owned());
            current.turn_id = None;
            current.summary = summary.and_then(summarize_prompt).or(current.summary);
            current
        }
        other => {
            return Err(anyhow!("unsupported bot workspace status event: {other}"));
        }
    };

    let record = WorkspaceStatusEventRecord {
        schema_version: STATUS_SCHEMA_VERSION,
        event: event_name.to_owned(),
        source: SessionStatusOwner::Bot,
        workspace_cwd: canonical_workspace_string(workspace_path),
        occurred_at: now_iso(),
        payload,
    };
    append_status_event(workspace_path, &record).await?;
    write_session_status(workspace_path, &next).await?;
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(next)
}

pub async fn record_tui_proxy_connected(
    workspace_path: &Path,
    thread_key: &str,
    session_id: &str,
) -> Result<SessionCurrentStatus> {
    ensure_workspace_status_surface(workspace_path).await?;
    let mut owner_claim = read_local_session_claim(workspace_path)
        .await?
        .filter(|claim| claim.thread_key == thread_key)
        .unwrap_or_else(|| default_local_session_claim(workspace_path, thread_key.to_owned(), 0));
    owner_claim.thread_key = thread_key.to_owned();
    owner_claim.session_id = Some(session_id.to_owned());
    owner_claim.updated_at = now_iso();
    write_local_session_claim(workspace_path, &owner_claim).await?;

    deactivate_other_tui_proxy_sessions(workspace_path, session_id).await?;
    let mut current = read_session_status(workspace_path, session_id)
        .await?
        .unwrap_or_else(|| {
            default_session_status(workspace_path, session_id, SessionStatusOwner::Local)
        });
    current.schema_version = STATUS_SCHEMA_VERSION;
    current.workspace_cwd = canonical_workspace_string(workspace_path);
    current.owner = SessionStatusOwner::Local;
    current.live = true;
    current.phase = WorkspaceStatusPhase::ShellActive;
    current.shell_pid = Some(owner_claim.shell_pid);
    current.child_pid = owner_claim.child_pid;
    current.child_pgid = owner_claim.child_pgid;
    current.child_command = owner_claim.child_command.clone();
    current.client = Some("threadbridge-tui-proxy".to_owned());
    current.turn_id = None;
    current.updated_at = now_iso();
    write_session_status(workspace_path, &current).await?;
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(current)
}

pub async fn record_tui_proxy_prompt(
    workspace_path: &Path,
    session_id: &str,
    prompt: &str,
) -> Result<SessionCurrentStatus> {
    ensure_workspace_status_surface(workspace_path).await?;
    let mut current = read_session_status(workspace_path, session_id)
        .await?
        .unwrap_or_else(|| {
            default_session_status(workspace_path, session_id, SessionStatusOwner::Local)
        });
    current.schema_version = STATUS_SCHEMA_VERSION;
    current.workspace_cwd = canonical_workspace_string(workspace_path);
    current.owner = SessionStatusOwner::Local;
    current.live = true;
    current.phase = WorkspaceStatusPhase::TurnRunning;
    current.shell_pid = Some(0);
    current.client = Some("threadbridge-tui-proxy".to_owned());
    current.summary = summarize_prompt(prompt);
    current.updated_at = now_iso();
    write_session_status(workspace_path, &current).await?;
    let record = WorkspaceStatusEventRecord {
        schema_version: STATUS_SCHEMA_VERSION,
        event: "user_prompt_submitted".to_owned(),
        source: SessionStatusOwner::Local,
        workspace_cwd: canonical_workspace_string(workspace_path),
        occurred_at: now_iso(),
        payload: json!({
            "session_id": session_id,
            "prompt": prompt,
            "client": "threadbridge-tui-proxy",
        }),
    };
    append_status_event(workspace_path, &record).await?;
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(current)
}

pub async fn record_tui_proxy_process_event(
    workspace_path: &Path,
    session_id: &str,
    phase: &str,
    text: &str,
) -> Result<()> {
    ensure_workspace_status_surface(workspace_path).await?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let record = WorkspaceStatusEventRecord {
        schema_version: STATUS_SCHEMA_VERSION,
        event: "process_transcript".to_owned(),
        source: SessionStatusOwner::Local,
        workspace_cwd: canonical_workspace_string(workspace_path),
        occurred_at: now_iso(),
        payload: json!({
            "session_id": session_id,
            "phase": phase,
            "text": trimmed,
            "client": "threadbridge-tui-proxy",
        }),
    };
    append_status_event(workspace_path, &record).await?;
    Ok(())
}

pub async fn record_tui_proxy_preview_text(
    workspace_path: &Path,
    session_id: &str,
    text: &str,
) -> Result<()> {
    ensure_workspace_status_surface(workspace_path).await?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let record = WorkspaceStatusEventRecord {
        schema_version: STATUS_SCHEMA_VERSION,
        event: "preview_text".to_owned(),
        source: SessionStatusOwner::Local,
        workspace_cwd: canonical_workspace_string(workspace_path),
        occurred_at: now_iso(),
        payload: json!({
            "session_id": session_id,
            "text": trimmed,
            "client": "threadbridge-tui-proxy",
        }),
    };
    append_status_event(workspace_path, &record).await?;
    Ok(())
}

pub async fn record_tui_proxy_completed(
    workspace_path: &Path,
    session_id: &str,
    turn_id: Option<&str>,
    last_assistant_message: Option<&str>,
) -> Result<SessionCurrentStatus> {
    ensure_workspace_status_surface(workspace_path).await?;
    let mut current = read_session_status(workspace_path, session_id)
        .await?
        .unwrap_or_else(|| {
            default_session_status(workspace_path, session_id, SessionStatusOwner::Local)
        });
    current.schema_version = STATUS_SCHEMA_VERSION;
    current.workspace_cwd = canonical_workspace_string(workspace_path);
    current.owner = SessionStatusOwner::Local;
    current.live = true;
    current.phase = WorkspaceStatusPhase::Idle;
    current.shell_pid = Some(0);
    current.client = Some("threadbridge-tui-proxy".to_owned());
    current.turn_id = None;
    current.summary = last_assistant_message
        .and_then(summarize_prompt)
        .or(current.summary);
    current.updated_at = now_iso();
    write_session_status(workspace_path, &current).await?;
    if !should_skip_duplicate_turn_completed_event(workspace_path, session_id, turn_id).await? {
        let record = WorkspaceStatusEventRecord {
            schema_version: STATUS_SCHEMA_VERSION,
            event: "turn_completed".to_owned(),
            source: SessionStatusOwner::Local,
            workspace_cwd: canonical_workspace_string(workspace_path),
            occurred_at: now_iso(),
            payload: json!({
                "thread-id": session_id,
                "turn-id": turn_id,
                "last-assistant-message": last_assistant_message,
                "client": "threadbridge-tui-proxy",
            }),
        };
        append_status_event(workspace_path, &record).await?;
    }
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(current)
}

pub async fn record_tui_proxy_disconnected(
    workspace_path: &Path,
    _thread_key: &str,
    session_id: Option<&str>,
) -> Result<()> {
    ensure_workspace_status_surface(workspace_path).await?;
    if let Some(session_id) = session_id
        && let Some(mut current) = read_session_status(workspace_path, session_id).await?
    {
        current.updated_at = now_iso();
        write_session_status(workspace_path, &current).await?;
    }
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(())
}

pub async fn record_hcodex_launcher_started(
    workspace_path: &Path,
    thread_key: &str,
    shell_pid: u32,
    child_pid: u32,
    child_command: &str,
) -> Result<()> {
    ensure_workspace_status_surface(workspace_path).await?;
    let mut owner_claim =
        default_local_session_claim(workspace_path, thread_key.to_owned(), shell_pid);
    owner_claim.child_pid = Some(child_pid);
    owner_claim.child_command = Some(child_command.to_owned());
    owner_claim.updated_at = now_iso();
    write_local_session_claim(workspace_path, &owner_claim).await?;
    Ok(())
}

pub async fn record_hcodex_launcher_ended(
    workspace_path: &Path,
    thread_key: &str,
    shell_pid: u32,
    child_pid: u32,
) -> Result<()> {
    ensure_workspace_status_surface(workspace_path).await?;
    let Some(owner_claim) = read_local_session_claim(workspace_path).await? else {
        return Ok(());
    };
    if owner_claim.thread_key != thread_key
        || owner_claim.shell_pid != shell_pid
        || owner_claim.child_pid != Some(child_pid)
    {
        return Ok(());
    }

    remove_local_session_claim(workspace_path).await?;
    if let Some(session_id) = owner_claim.session_id.as_deref()
        && let Some(mut current) = read_session_status(workspace_path, session_id).await?
    {
        current.live = false;
        current.phase = WorkspaceStatusPhase::Idle;
        current.turn_id = None;
        current.updated_at = now_iso();
        write_session_status(workspace_path, &current).await?;
    }
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    let _ = refresh_workspace_aggregate_status(workspace_path, aggregate).await?;
    Ok(())
}

async fn deactivate_other_tui_proxy_sessions(
    workspace_path: &Path,
    active_session_id: &str,
) -> Result<()> {
    for mut session in list_all_session_statuses(workspace_path).await? {
        if session.session_id == active_session_id
            || session.client.as_deref() != Some("threadbridge-tui-proxy")
            || !session.live
        {
            continue;
        }
        session.live = false;
        session.phase = WorkspaceStatusPhase::Idle;
        session.turn_id = None;
        session.updated_at = now_iso();
        write_session_status(workspace_path, &session).await?;
    }
    Ok(())
}

impl WorkspaceStatusCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, workspace_path: &Path) -> Option<WorkspaceAggregateStatus> {
        self.inner
            .read()
            .await
            .get(&canonical_workspace_string(workspace_path))
            .cloned()
    }

    pub async fn insert(&self, status: WorkspaceAggregateStatus) {
        self.inner
            .write()
            .await
            .insert(status.workspace_cwd.clone(), status);
    }

    pub async fn remove_missing_workspaces(&self, keep: &[String]) {
        let mut guard = self.inner.write().await;
        guard.retain(|workspace, _| keep.iter().any(|item| item == workspace));
    }
}

pub async fn read_workspace_status_with_cache(
    cache: &WorkspaceStatusCache,
    workspace_path: &Path,
) -> Result<WorkspaceAggregateStatus> {
    if let Some(status) = cache.get(workspace_path).await {
        return Ok(status);
    }
    let status = read_workspace_aggregate_status(workspace_path).await?;
    cache.insert(status.clone()).await;
    Ok(status)
}

pub async fn busy_selected_session_status(
    cache: &WorkspaceStatusCache,
    workspace_path: &Path,
    session_id: &str,
) -> Result<Option<BusySelectedSessionStatus>> {
    let aggregate = read_workspace_aggregate_status(workspace_path).await?;
    cache.insert(aggregate).await;
    let Some(snapshot) = read_session_status(workspace_path, session_id).await? else {
        return Ok(None);
    };
    if !snapshot.phase.is_turn_busy() {
        return Ok(None);
    }
    Ok(Some(BusySelectedSessionStatus {
        workspace_path: workspace_path.to_path_buf(),
        snapshot,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        SessionCurrentStatus, SessionStatusOwner, WorkspaceStatusCache, WorkspaceStatusPhase,
        busy_selected_session_status, current_status_path, ensure_workspace_status_surface,
        events_path, list_live_local_sessions, read_local_session_claim, read_session_status,
        read_workspace_aggregate_status, record_bot_status_event, record_hcodex_launcher_ended,
        record_hcodex_launcher_started, record_tui_proxy_completed, record_tui_proxy_connected,
        session_status_path,
    };
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "threadbridge-workspace-status-test-{}",
            Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn ensure_surface_creates_aggregate_and_sessions_directory() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();

        assert!(
            fs::try_exists(current_status_path(&workspace))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/state/shared-runtime/sessions"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn bot_events_write_session_snapshot() {
        let workspace = temp_path();
        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_bot"),
            Some("turn-1"),
            Some("hello"),
        )
        .await
        .unwrap();

        let session = read_session_status(&workspace, "thr_bot")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.owner, SessionStatusOwner::Bot);
        assert_eq!(session.phase, WorkspaceStatusPhase::TurnRunning);
        assert!(!session.live);
    }

    #[tokio::test]
    async fn bot_events_take_over_existing_local_session_snapshot() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let local_session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_cli".to_owned(),
            owner: SessionStatusOwner::Local,
            live: false,
            phase: WorkspaceStatusPhase::Idle,
            shell_pid: Some(99),
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("codex-cli".to_owned()),
            turn_id: None,
            summary: Some("startup".to_owned()),
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(&workspace, "thr_cli"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&local_session).unwrap()
            ),
        )
        .await
        .unwrap();

        let session = record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_cli"),
            Some("turn-1"),
            Some("handoff"),
        )
        .await
        .unwrap();
        assert_eq!(session.owner, SessionStatusOwner::Bot);
        assert_eq!(session.phase, WorkspaceStatusPhase::TurnRunning);
        assert!(!session.live);
        assert_eq!(session.shell_pid, None);
    }

    #[tokio::test]
    async fn busy_selected_session_only_blocks_running_turns() {
        let workspace = temp_path();
        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_bot"),
            Some("turn-1"),
            Some("hello"),
        )
        .await
        .unwrap();

        let busy =
            busy_selected_session_status(&WorkspaceStatusCache::new(), &workspace, "thr_bot")
                .await
                .unwrap();
        assert!(busy.is_some());

        record_bot_status_event(
            &workspace,
            "bot_turn_completed",
            Some("thr_bot"),
            Some("turn-1"),
            Some("done"),
        )
        .await
        .unwrap();

        let busy =
            busy_selected_session_status(&WorkspaceStatusCache::new(), &workspace, "thr_bot")
                .await
                .unwrap();
        assert!(busy.is_none());
    }

    #[tokio::test]
    async fn bot_turn_recovered_clears_busy_without_overwriting_summary() {
        let workspace = temp_path();
        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_bot"),
            Some("turn-1"),
            Some("prompt summary"),
        )
        .await
        .unwrap();

        record_bot_status_event(
            &workspace,
            "bot_turn_recovered",
            Some("thr_bot"),
            Some("turn-1"),
            None,
        )
        .await
        .unwrap();

        let session = read_session_status(&workspace, "thr_bot")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.owner, SessionStatusOwner::Bot);
        assert_eq!(session.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(session.turn_id, None);
        assert_eq!(session.summary.as_deref(), Some("prompt summary"));

        let busy =
            busy_selected_session_status(&WorkspaceStatusCache::new(), &workspace, "thr_bot")
                .await
                .unwrap();
        assert!(busy.is_none());
    }

    #[tokio::test]
    async fn live_local_session_listing_reads_per_session_registry() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let local_session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace.display().to_string(),
            session_id: "thr_cli".to_owned(),
            owner: SessionStatusOwner::Local,
            live: true,
            phase: WorkspaceStatusPhase::ShellActive,
            shell_pid: Some(12),
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("codex-cli".to_owned()),
            turn_id: None,
            summary: Some("startup".to_owned()),
            updated_at: "2026-03-19T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(&workspace, "thr_cli"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&local_session).unwrap()
            ),
        )
        .await
        .unwrap();

        let aggregate = read_workspace_aggregate_status(&workspace).await.unwrap();
        super::refresh_workspace_aggregate_status(&workspace, aggregate)
            .await
            .unwrap();
        let sessions = list_live_local_sessions(&workspace).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "thr_cli");
    }

    #[tokio::test]
    async fn tui_proxy_connected_deactivates_previous_tui_session() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        record_hcodex_launcher_started(&workspace, "thread-key", 42, 77, "codex --remote")
            .await
            .unwrap();

        record_tui_proxy_connected(&workspace, "thread-key", "thr_old")
            .await
            .unwrap();
        record_tui_proxy_connected(&workspace, "thread-key", "thr_new")
            .await
            .unwrap();

        let old_session = read_session_status(&workspace, "thr_old")
            .await
            .unwrap()
            .unwrap();
        let new_session = read_session_status(&workspace, "thr_new")
            .await
            .unwrap()
            .unwrap();
        let owner_claim = read_local_session_claim(&workspace).await.unwrap().unwrap();
        let aggregate = read_workspace_aggregate_status(&workspace).await.unwrap();

        assert!(!old_session.live);
        assert!(new_session.live);
        assert_eq!(new_session.shell_pid, Some(42));
        assert_eq!(new_session.child_pid, Some(77));
        assert_eq!(owner_claim.session_id.as_deref(), Some("thr_new"));
        assert_eq!(aggregate.live_local_session_ids, vec!["thr_new".to_owned()]);
    }

    #[tokio::test]
    async fn launcher_end_clears_owner_and_marks_session_not_live() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        record_hcodex_launcher_started(&workspace, "thread-key", 42, 77, "codex --remote")
            .await
            .unwrap();
        record_tui_proxy_connected(&workspace, "thread-key", "thr_new")
            .await
            .unwrap();

        record_hcodex_launcher_ended(&workspace, "thread-key", 42, 77)
            .await
            .unwrap();

        let owner_claim = read_local_session_claim(&workspace).await.unwrap();
        let session = read_session_status(&workspace, "thr_new")
            .await
            .unwrap()
            .unwrap();

        assert!(owner_claim.is_none());
        assert!(!session.live);
        assert_eq!(session.phase, WorkspaceStatusPhase::Idle);
    }

    #[tokio::test]
    async fn tui_proxy_completed_deduplicates_same_turn_id() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();

        record_tui_proxy_completed(&workspace, "thr_same", Some("turn-1"), Some("hello"))
            .await
            .unwrap();
        record_tui_proxy_completed(&workspace, "thr_same", Some("turn-1"), Some("hello"))
            .await
            .unwrap();

        let lines = fs::read_to_string(events_path(&workspace)).await.unwrap();
        let turn_completed_count = lines
            .lines()
            .filter(|line| line.contains("\"event\":\"turn_completed\""))
            .count();
        assert_eq!(turn_completed_count, 1);
    }
}
