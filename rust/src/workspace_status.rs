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

const STATUS_SCHEMA_VERSION: u32 = 1;
const STATUS_DIR: &str = ".threadbridge/state/codex-sync";
const CURRENT_FILE: &str = "current.json";
const EVENTS_FILE: &str = "events.jsonl";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatusSource {
    Cli,
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
    pub fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceCurrentStatus {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub source: Option<WorkspaceStatusSource>,
    pub phase: WorkspaceStatusPhase,
    pub shell_pid: Option<u32>,
    pub client: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub summary: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatusEventRecord {
    pub schema_version: u32,
    pub event: String,
    pub source: WorkspaceStatusSource,
    pub workspace_cwd: String,
    pub occurred_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatusCache {
    inner: Arc<RwLock<HashMap<String, WorkspaceCurrentStatus>>>,
}

#[derive(Debug, Clone)]
pub struct BusyWorkspaceStatus {
    pub workspace_path: PathBuf,
    pub snapshot: WorkspaceCurrentStatus,
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

pub fn current_status_path(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(CURRENT_FILE)
}

pub fn events_path(workspace_path: &Path) -> PathBuf {
    status_dir(workspace_path).join(EVENTS_FILE)
}

pub fn idle_status_for_workspace(workspace_path: &Path) -> WorkspaceCurrentStatus {
    WorkspaceCurrentStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        workspace_cwd: canonical_workspace_string(workspace_path),
        source: None,
        phase: WorkspaceStatusPhase::Idle,
        shell_pid: None,
        client: None,
        session_id: None,
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

pub async fn write_current_status(
    workspace_path: &Path,
    status: &WorkspaceCurrentStatus,
) -> Result<()> {
    atomic_write_json(&current_status_path(workspace_path), status).await
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

pub async fn ensure_workspace_status_surface(workspace_path: &Path) -> Result<()> {
    let dir = status_dir(workspace_path);
    fs::create_dir_all(&dir).await?;

    let current_path = current_status_path(workspace_path);
    if !fs::try_exists(&current_path).await? {
        write_current_status(workspace_path, &idle_status_for_workspace(workspace_path)).await?;
    }

    let events_path = events_path(workspace_path);
    if !fs::try_exists(&events_path).await? {
        fs::write(events_path, "").await?;
    }

    Ok(())
}

pub async fn read_current_status(workspace_path: &Path) -> Result<WorkspaceCurrentStatus> {
    let path = current_status_path(workspace_path);
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(idle_status_for_workspace(workspace_path))
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
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

pub async fn record_bot_status_event(
    workspace_path: &Path,
    event_name: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    summary: Option<&str>,
) -> Result<WorkspaceCurrentStatus> {
    ensure_workspace_status_surface(workspace_path).await?;
    let mut current = read_current_status(workspace_path).await?;
    let payload = json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "summary": summary,
    });

    let next = match event_name {
        "bot_turn_started" => WorkspaceCurrentStatus {
            schema_version: STATUS_SCHEMA_VERSION,
            workspace_cwd: current.workspace_cwd.clone(),
            source: Some(WorkspaceStatusSource::Bot),
            phase: WorkspaceStatusPhase::TurnRunning,
            shell_pid: None,
            client: Some("threadbridge".to_owned()),
            session_id: session_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
            summary: summary.and_then(summarize_prompt),
            updated_at: now_iso(),
        },
        "bot_turn_completed" | "bot_turn_failed" => {
            current.source = None;
            current.phase = WorkspaceStatusPhase::Idle;
            current.shell_pid = None;
            current.client = None;
            current.session_id = None;
            current.turn_id = None;
            current.summary = None;
            current.updated_at = now_iso();
            current
        }
        other => {
            return Err(anyhow!("unsupported bot workspace status event: {other}"));
        }
    };

    let record = WorkspaceStatusEventRecord {
        schema_version: STATUS_SCHEMA_VERSION,
        event: event_name.to_owned(),
        source: WorkspaceStatusSource::Bot,
        workspace_cwd: canonical_workspace_string(workspace_path),
        occurred_at: now_iso(),
        payload,
    };
    append_status_event(workspace_path, &record).await?;
    write_current_status(workspace_path, &next).await?;
    Ok(next)
}

impl WorkspaceStatusCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, workspace_path: &Path) -> Option<WorkspaceCurrentStatus> {
        self.inner
            .read()
            .await
            .get(&canonical_workspace_string(workspace_path))
            .cloned()
    }

    pub async fn insert(&self, status: WorkspaceCurrentStatus) {
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

pub async fn read_status_with_cache(
    cache: &WorkspaceStatusCache,
    workspace_path: &Path,
) -> Result<WorkspaceCurrentStatus> {
    if let Some(status) = cache.get(workspace_path).await {
        return Ok(status);
    }
    let status = read_current_status(workspace_path).await?;
    cache.insert(status.clone()).await;
    Ok(status)
}

pub async fn busy_workspace_status(
    cache: &WorkspaceStatusCache,
    workspace_path: &Path,
) -> Result<Option<BusyWorkspaceStatus>> {
    let snapshot = read_current_status(workspace_path).await?;
    cache.insert(snapshot.clone()).await;
    if snapshot.phase.is_idle() {
        return Ok(None);
    }
    Ok(Some(BusyWorkspaceStatus {
        workspace_path: workspace_path.to_path_buf(),
        snapshot,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspaceStatusCache, WorkspaceStatusPhase, WorkspaceStatusSource, busy_workspace_status,
        current_status_path, ensure_workspace_status_surface, idle_status_for_workspace,
        read_current_status, record_bot_status_event,
    };
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_workspace() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-status-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn ensure_workspace_status_surface_initializes_idle_snapshot() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).await.unwrap();

        ensure_workspace_status_surface(&workspace).await.unwrap();

        let status = read_current_status(&workspace).await.unwrap();
        assert_eq!(status.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(status.source, None);
        assert!(current_status_path(&workspace).exists());
    }

    #[tokio::test]
    async fn bot_events_transition_status_and_cache() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).await.unwrap();
        ensure_workspace_status_surface(&workspace).await.unwrap();

        let started = record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thread-1"),
            Some("turn-1"),
            Some("hello world"),
        )
        .await
        .unwrap();
        assert_eq!(started.source, Some(WorkspaceStatusSource::Bot));
        assert_eq!(started.phase, WorkspaceStatusPhase::TurnRunning);

        let cache = WorkspaceStatusCache::new();
        cache.insert(started.clone()).await;
        let busy = busy_workspace_status(&cache, &workspace)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(busy.snapshot.phase, WorkspaceStatusPhase::TurnRunning);

        let completed = record_bot_status_event(
            &workspace,
            "bot_turn_completed",
            Some("thread-1"),
            Some("turn-1"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(completed.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(completed.source, None);
    }

    #[test]
    fn idle_snapshot_uses_workspace_path() {
        let workspace = PathBuf::from("/tmp/example-workspace");
        let idle = idle_status_for_workspace(&workspace);
        assert_eq!(idle.phase, WorkspaceStatusPhase::Idle);
        assert!(idle.workspace_cwd.ends_with("example-workspace"));
    }
}
