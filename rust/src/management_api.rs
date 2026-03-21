use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_stream::stream;
use axum::extract::{Path as AxumPath, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::app_server_runtime::WorkspaceRuntimeState;
use crate::config::{RuntimeConfig, load_optional_telegram_config};
use crate::repository::{RecentCodexSessionEntry, ThreadRepository};
use crate::workspace_status::{read_cli_owner_claim, read_workspace_aggregate_status};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramPollingState {
    Disconnected,
    Active,
}

#[derive(Clone)]
pub struct ManagementApiHandle {
    pub base_url: String,
    state: Arc<ManagementApiState>,
}

impl ManagementApiHandle {
    pub async fn set_telegram_polling_state(&self, state: TelegramPollingState) {
        let mut current = self.state.telegram_polling_state.write().await;
        *current = state;
    }
}

#[derive(Clone)]
struct ManagementApiState {
    runtime: RuntimeConfig,
    repository: ThreadRepository,
    telegram_polling_state: Arc<RwLock<TelegramPollingState>>,
}

#[derive(Debug, Clone, Serialize)]
struct SetupStateView {
    telegram_token_configured: bool,
    authorized_user_ids: Vec<i64>,
    authorized_user_count: usize,
    telegram_polling_state: TelegramPollingState,
    management_base_url: String,
    restart_required_after_setup_save: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeHealthView {
    management_bind_addr: String,
    broken_threads: usize,
    running_workspaces: usize,
    conflicted_workspaces: usize,
    app_server_status: &'static str,
    tui_proxy_status: &'static str,
    handoff_readiness: &'static str,
    managed_codex: ManagedCodexView,
}

#[derive(Debug, Clone, Serialize)]
struct ManagedCodexView {
    source: &'static str,
    binary_path: String,
    binary_ready: bool,
    version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ManagedWorkspaceView {
    workspace_cwd: String,
    title: Option<String>,
    thread_key: Option<String>,
    binding_status: &'static str,
    run_status: &'static str,
    current_codex_thread_id: Option<String>,
    tui_active_codex_thread_id: Option<String>,
    session_broken: bool,
    last_used_at: Option<String>,
    conflict: bool,
    app_server_status: &'static str,
    tui_proxy_status: &'static str,
    handoff_readiness: &'static str,
    hcodex_path: String,
    hcodex_available: bool,
    recent_codex_sessions: Vec<RecentCodexSessionEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct ArchivedThreadView {
    thread_key: String,
    title: Option<String>,
    workspace_cwd: Option<String>,
    archived_at: Option<String>,
    previous_message_thread_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize)]
struct HcodexLaunchConfigView {
    workspace_cwd: String,
    thread_key: String,
    hcodex_path: String,
    hcodex_available: bool,
    current_codex_thread_id: Option<String>,
    recent_codex_sessions: Vec<RecentCodexSessionEntry>,
    launch_new_command: String,
    launch_resume_commands: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ArchiveThreadResponse {
    archived: bool,
    thread_key: String,
}

#[derive(Debug, Deserialize)]
struct LaunchResumeRequest {
    session_id: String,
}

#[derive(Debug, Serialize)]
struct LaunchWorkspaceResponse {
    launched: bool,
    thread_key: String,
    command: String,
}

#[derive(Debug, Deserialize)]
struct UpdateTelegramSetupRequest {
    telegram_token: String,
    authorized_user_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct UpdateTelegramSetupResponse {
    saved: bool,
    restart_required: bool,
}

#[derive(Debug)]
struct WorkspaceAggregateView {
    workspace_cwd: String,
    items: Vec<ManagedWorkspaceView>,
}

pub async fn spawn_management_api(runtime: RuntimeConfig) -> Result<ManagementApiHandle> {
    let repository = ThreadRepository::open(&runtime.data_root_path).await?;
    let state = Arc::new(ManagementApiState {
        runtime: runtime.clone(),
        repository,
        telegram_polling_state: Arc::new(RwLock::new(TelegramPollingState::Disconnected)),
    });
    let bind_addr = runtime.management_bind_addr;
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind local management API at {bind_addr}"))?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let router = Router::new()
        .route("/", get(index))
        .route("/api/setup", get(get_setup))
        .route("/api/setup/telegram", put(put_telegram_setup))
        .route("/api/runtime-health", get(get_runtime_health))
        .route("/api/workspaces", get(get_workspaces))
        .route("/api/archived-threads", get(get_archived_threads))
        .route(
            "/api/workspaces/:thread_key/launch-config",
            get(get_workspace_launch_config),
        )
        .route(
            "/api/workspaces/:thread_key/launch-new",
            post(post_launch_workspace_new),
        )
        .route(
            "/api/workspaces/:thread_key/launch-resume",
            post(post_launch_workspace_resume),
        )
        .route("/api/threads/:thread_key/archive", post(post_archive_thread))
        .route("/api/events", get(get_events))
        .with_state(state.clone());
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            warn!(event = "management_api.serve.failed", error = %error);
        }
    });
    info!(
        event = "management_api.started",
        bind_addr = %bind_addr,
        base_url = %base_url,
        "local management API started"
    );
    Ok(ManagementApiHandle { base_url, state })
}

async fn index(State(state): State<Arc<ManagementApiState>>) -> impl IntoResponse {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>threadBridge Management</title>
  <style>
    body {{ font-family: ui-sans-serif, sans-serif; margin: 2rem; line-height: 1.4; }}
    h1, h2 {{ margin-bottom: 0.4rem; }}
    pre {{ background: #f5f5f5; padding: 1rem; overflow: auto; border-radius: 8px; }}
    .grid {{ display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); }}
    .card {{ border: 1px solid #ddd; border-radius: 8px; padding: 1rem; }}
  </style>
</head>
<body>
  <h1>threadBridge Management</h1>
  <p>Local management API is running at <code>{}</code>.</p>
  <div class="grid">
    <div class="card">
      <h2>Setup</h2>
      <form id="setup-form">
        <p><label>Telegram Bot Token<br /><input id="telegram-token" type="password" style="width:100%" /></label></p>
        <p><label>Authorized User IDs (comma separated)<br /><input id="authorized-user-ids" type="text" style="width:100%" /></label></p>
        <p><button type="submit">Save Setup</button> <span id="setup-status"></span></p>
      </form>
      <pre id="setup">loading...</pre>
    </div>
    <div class="card"><h2>Runtime Health</h2><pre id="health">loading...</pre></div>
  </div>
  <div class="card"><h2>Managed Workspaces</h2><div id="workspaces">loading...</div></div>
  <div class="card"><h2>Archived Threads</h2><div id="archived">loading...</div></div>
  <script>
    async function renderJson(id, path) {{
      const response = await fetch(path);
      const data = await response.json();
      document.getElementById(id).textContent = JSON.stringify(data, null, 2);
      return data;
    }}
    function renderWorkspaceCards(items) {{
      const root = document.getElementById('workspaces');
      if (!items.length) {{
        root.innerHTML = '<p>No managed workspaces.</p>';
        return;
      }}
      root.innerHTML = items.map(item => `
        <div style="border:1px solid #ddd;border-radius:8px;padding:1rem;margin-bottom:1rem;">
          <strong>${{item.title || item.workspace_cwd}}</strong><br />
          <code>${{item.workspace_cwd}}</code><br />
          thread_key: <code>${{item.thread_key || ''}}</code><br />
          binding: <code>${{item.binding_status}}</code> |
          run: <code>${{item.run_status}}</code> |
          handoff: <code>${{item.handoff_readiness}}</code><br />
          current: <code>${{item.current_codex_thread_id || 'none'}}</code><br />
          tui: <code>${{item.tui_active_codex_thread_id || 'none'}}</code><br />
          hcodex: <code>${{item.hcodex_path}}</code><br />
          recent: ${{item.recent_codex_sessions.map(x => `<code>${{x.session_id}}</code>`).join(', ') || 'none'}}
          <div style="margin-top:0.75rem;">
            <button onclick="launchNew('${{item.thread_key}}')">Launch New</button>
            <button onclick="showLaunchConfig('${{item.thread_key}}')">Show Launch Commands</button>
            <button onclick="archiveThread('${{item.thread_key}}')">Archive</button>
          </div>
          <pre id="launch-${{item.thread_key}}" style="display:none;margin-top:0.75rem;"></pre>
        </div>
      `).join('');
    }}
    function renderArchivedThreads(items) {{
      const root = document.getElementById('archived');
      if (!items.length) {{
        root.innerHTML = '<p>No archived threads.</p>';
        return;
      }}
      root.innerHTML = items.map(item => `
        <div style="border:1px solid #ddd;border-radius:8px;padding:1rem;margin-bottom:1rem;">
          <strong>${{item.title || item.thread_key}}</strong><br />
          thread_key: <code>${{item.thread_key}}</code><br />
          workspace: <code>${{item.workspace_cwd || 'unbound'}}</code><br />
          archived_at: <code>${{item.archived_at || 'unknown'}}</code>
        </div>
      `).join('');
    }}
    async function refresh() {{
      const [setup, _, workspaces, archived] = await Promise.all([
        renderJson('setup', '/api/setup'),
        renderJson('health', '/api/runtime-health'),
        fetch('/api/workspaces').then(r => r.json()),
        fetch('/api/archived-threads').then(r => r.json()),
      ]);
      document.getElementById('authorized-user-ids').value = (setup.authorized_user_ids || []).join(',');
      renderWorkspaceCards(workspaces);
      renderArchivedThreads(archived);
    }}
    async function showLaunchConfig(threadKey) {{
      const response = await fetch(`/api/workspaces/${{threadKey}}/launch-config`);
      const data = await response.json();
      const target = document.getElementById(`launch-${{threadKey}}`);
      target.style.display = 'block';
      target.textContent = JSON.stringify(data, null, 2);
    }}
    async function launchNew(threadKey) {{
      const response = await fetch(`/api/workspaces/${{threadKey}}/launch-new`, {{ method: 'POST' }});
      const data = await response.json();
      if (!response.ok) {{
        alert(data.error || 'Launch failed');
        return;
      }}
      const target = document.getElementById(`launch-${{threadKey}}`);
      target.style.display = 'block';
      target.textContent = JSON.stringify(data, null, 2);
      await refresh();
    }}
    async function archiveThread(threadKey) {{
      const response = await fetch(`/api/threads/${{threadKey}}/archive`, {{ method: 'POST' }});
      const data = await response.json();
      if (!response.ok) {{
        alert(data.error || 'Archive failed');
        return;
      }}
      await refresh();
    }}
    document.getElementById('setup-form').addEventListener('submit', async event => {{
      event.preventDefault();
      const status = document.getElementById('setup-status');
      status.textContent = 'Saving...';
      const payload = {{
        telegram_token: document.getElementById('telegram-token').value,
        authorized_user_ids: document.getElementById('authorized-user-ids').value
          .split(',')
          .map(x => x.trim())
          .filter(Boolean)
          .map(x => Number(x)),
      }};
      const response = await fetch('/api/setup/telegram', {{
        method: 'PUT',
        headers: {{ 'Content-Type': 'application/json' }},
        body: JSON.stringify(payload),
      }});
      const data = await response.json();
      if (!response.ok) {{
        status.textContent = data.error || 'Save failed';
        return;
      }}
      document.getElementById('telegram-token').value = '';
      status.textContent = data.restart_required ? 'Saved. Restart required.' : 'Saved.';
      await refresh();
    }});
    refresh();
    const events = new EventSource('/api/events');
    events.onmessage = () => refresh();
  </script>
</body>
</html>"#,
        state.runtime.management_bind_addr
    );
    Html(html)
}

async fn get_setup(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<SetupStateView>, ManagementApiError> {
    Ok(Json(state.setup_state().await?))
}

async fn put_telegram_setup(
    State(state): State<Arc<ManagementApiState>>,
    Json(payload): Json<UpdateTelegramSetupRequest>,
) -> Result<Json<UpdateTelegramSetupResponse>, ManagementApiError> {
    state.write_telegram_setup(payload).await?;
    Ok(Json(UpdateTelegramSetupResponse {
        saved: true,
        restart_required: true,
    }))
}

async fn get_runtime_health(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<RuntimeHealthView>, ManagementApiError> {
    Ok(Json(state.runtime_health().await?))
}

async fn get_workspaces(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ManagedWorkspaceView>>, ManagementApiError> {
    Ok(Json(state.workspace_views().await?))
}

async fn get_archived_threads(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ArchivedThreadView>>, ManagementApiError> {
    Ok(Json(state.archived_thread_views().await?))
}

async fn get_workspace_launch_config(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<HcodexLaunchConfigView>, ManagementApiError> {
    Ok(Json(state.workspace_launch_config(&thread_key).await?))
}

async fn post_launch_workspace_new(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<LaunchWorkspaceResponse>, ManagementApiError> {
    Ok(Json(state.launch_workspace_new(&thread_key).await?))
}

async fn post_launch_workspace_resume(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
    Json(payload): Json<LaunchResumeRequest>,
) -> Result<Json<LaunchWorkspaceResponse>, ManagementApiError> {
    Ok(Json(
        state
            .launch_workspace_resume(&thread_key, &payload.session_id)
            .await?,
    ))
}

async fn post_archive_thread(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ArchiveThreadResponse>, ManagementApiError> {
    Ok(Json(state.archive_thread(&thread_key).await?))
}

async fn get_events(
    State(state): State<Arc<ManagementApiState>>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = stream! {
        loop {
            let setup = state.setup_state().await.ok();
            let runtime = state.runtime_health().await.ok();
            let workspaces = state.workspace_views().await.ok();
            let archived = state.archived_thread_views().await.ok();
            let payload = serde_json::json!({
                "setup": setup,
                "runtime": runtime,
                "workspaces": workspaces,
                "archived_threads": archived,
            });
            yield Ok(Event::default().data(payload.to_string()));
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(5)))
}

impl ManagementApiState {
    async fn setup_state(&self) -> Result<SetupStateView> {
        let telegram = load_optional_telegram_config()?;
        Ok(SetupStateView {
            telegram_token_configured: telegram.is_some(),
            authorized_user_ids: telegram
                .as_ref()
                .map(|config| {
                    let mut ids = config.authorized_user_ids.iter().copied().collect::<Vec<_>>();
                    ids.sort_unstable();
                    ids
                })
                .unwrap_or_default(),
            authorized_user_count: telegram
                .as_ref()
                .map(|config| config.authorized_user_ids.len())
                .unwrap_or_default(),
            telegram_polling_state: *self.telegram_polling_state.read().await,
            management_base_url: format!("http://{}", self.runtime.management_bind_addr),
            restart_required_after_setup_save: true,
        })
    }

    async fn runtime_health(&self) -> Result<RuntimeHealthView> {
        let workspaces = self.workspace_views().await?;
        let app_server_status =
            aggregate_running_status(workspaces.iter().map(|workspace| workspace.app_server_status));
        let tui_proxy_status =
            aggregate_running_status(workspaces.iter().map(|workspace| workspace.tui_proxy_status));
        let handoff_readiness = aggregate_handoff_status(
            workspaces.iter().map(|workspace| workspace.handoff_readiness),
        );
        Ok(RuntimeHealthView {
            management_bind_addr: self.runtime.management_bind_addr.to_string(),
            broken_threads: workspaces
                .iter()
                .filter(|workspace| workspace.session_broken)
                .count(),
            running_workspaces: workspaces
                .iter()
                .filter(|workspace| workspace.run_status == "running")
                .count(),
            conflicted_workspaces: workspaces.iter().filter(|workspace| workspace.conflict).count(),
            app_server_status,
            tui_proxy_status,
            handoff_readiness,
            managed_codex: self.managed_codex_view(),
        })
    }

    fn managed_codex_view(&self) -> ManagedCodexView {
        let managed_codex_path = self
            .runtime
            .codex_working_directory
            .join(".threadbridge")
            .join("codex")
            .join("codex");
        ManagedCodexView {
            source: "threadbridge_managed",
            binary_path: managed_codex_path.display().to_string(),
            binary_ready: managed_codex_path.exists(),
            version: None,
        }
    }

    async fn workspace_views(&self) -> Result<Vec<ManagedWorkspaceView>> {
        let active_threads = self.repository.list_active_threads().await?;
        let mut grouped: BTreeMap<String, WorkspaceAggregateView> = BTreeMap::new();
        for record in active_threads {
            let Some(binding) = self.repository.read_session_binding(&record).await? else {
                continue;
            };
            let Some(workspace_cwd) = binding.workspace_cwd.clone() else {
                continue;
            };
            let aggregate = grouped
                .entry(workspace_cwd.clone())
                .or_insert_with(|| WorkspaceAggregateView {
                    workspace_cwd: workspace_cwd.clone(),
                    items: Vec::new(),
                });
            let workspace_path = Path::new(&workspace_cwd);
            let hcodex_path = workspace_path
                .join(".threadbridge")
                .join("bin")
                .join("hcodex");
            let workspace_status = read_workspace_aggregate_status(workspace_path)
                .await
                .unwrap_or_else(|_| crate::workspace_status::default_workspace_status(workspace_path));
            let has_live_cli = !workspace_status.live_cli_session_ids.is_empty();
            let runtime_status = read_workspace_runtime_health(workspace_path).await;
            let recent_sessions = self
                .repository
                .read_recent_workspace_sessions(&workspace_cwd)
                .await
                .unwrap_or_default();
            let session_broken = binding.session_broken || record.metadata.session_broken;
            aggregate.items.push(ManagedWorkspaceView {
                workspace_cwd: workspace_cwd.clone(),
                title: record.metadata.title.clone(),
                thread_key: Some(record.metadata.thread_key.clone()),
                binding_status: if session_broken { "broken" } else { "healthy" },
                run_status: if has_live_cli { "running" } else { "idle" },
                current_codex_thread_id: binding.current_codex_thread_id.clone(),
                tui_active_codex_thread_id: binding.tui_active_codex_thread_id.clone(),
                session_broken,
                last_used_at: record.metadata.last_codex_turn_at.clone(),
                conflict: false,
                app_server_status: runtime_status.app_server_status,
                tui_proxy_status: runtime_status.tui_proxy_status,
                handoff_readiness: runtime_status.handoff_readiness,
                hcodex_path: hcodex_path.display().to_string(),
                hcodex_available: hcodex_path.exists(),
                recent_codex_sessions: recent_sessions,
            });
        }

        let mut views = Vec::new();
        for aggregate in grouped.into_values() {
            let conflict = aggregate.items.len() > 1;
            if conflict {
                for mut item in aggregate.items {
                    item.binding_status = "conflict";
                    item.conflict = true;
                    views.push(item);
                }
                continue;
            }
            let mut item = aggregate
                .items
                .into_iter()
                .next()
                .expect("workspace group is non-empty");
            let workspace_path = Path::new(&aggregate.workspace_cwd);
            if read_cli_owner_claim(workspace_path).await.ok().flatten().is_some()
                && item.run_status != "running"
            {
                item.run_status = "running";
            }
            views.push(item);
        }
        views.sort_by(|a, b| a.workspace_cwd.cmp(&b.workspace_cwd));
        Ok(views)
    }

    async fn archived_thread_views(&self) -> Result<Vec<ArchivedThreadView>> {
        let archived = self.repository.list_all_archived_threads().await?;
        let mut views = Vec::new();
        for record in archived {
            let binding = self.repository.read_session_binding(&record).await?;
            views.push(ArchivedThreadView {
                thread_key: record.metadata.thread_key,
                title: record.metadata.title,
                workspace_cwd: binding.and_then(|binding| binding.workspace_cwd),
                archived_at: record.metadata.archived_at,
                previous_message_thread_ids: record.metadata.previous_message_thread_ids,
            });
        }
        Ok(views)
    }

    async fn workspace_launch_config(&self, thread_key: &str) -> Result<HcodexLaunchConfigView> {
        let record = self
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active managed workspace")?;
        let binding = self
            .repository
            .read_session_binding(&record)
            .await?
            .context("managed workspace is missing session binding")?;
        let workspace_cwd = binding
            .workspace_cwd
            .clone()
            .context("managed workspace is missing workspace_cwd")?;
        let hcodex_path = Path::new(&workspace_cwd)
            .join(".threadbridge")
            .join("bin")
            .join("hcodex");
        let recent_codex_sessions = self
            .repository
            .read_recent_workspace_sessions(&workspace_cwd)
            .await
            .unwrap_or_default();
        Ok(HcodexLaunchConfigView {
            workspace_cwd: workspace_cwd.clone(),
            thread_key: record.metadata.thread_key,
            hcodex_path: hcodex_path.display().to_string(),
            hcodex_available: hcodex_path.exists(),
            current_codex_thread_id: binding.current_codex_thread_id,
            launch_new_command: format!(
                "{} --thread-key {}",
                shell_quote_path(&hcodex_path),
                shell_quote(thread_key)
            ),
            launch_resume_commands: recent_codex_sessions
                .iter()
                .map(|entry| {
                    format!(
                        "{} --thread-key {} resume {}",
                        shell_quote_path(&hcodex_path),
                        shell_quote(thread_key),
                        shell_quote(&entry.session_id)
                    )
                })
                .collect(),
            recent_codex_sessions,
        })
    }

    async fn archive_thread(&self, thread_key: &str) -> Result<ArchiveThreadResponse> {
        let record = self
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("thread_key is not an active thread"))?;
        let archived = self.repository.archive_thread(record).await?;
        Ok(ArchiveThreadResponse {
            archived: true,
            thread_key: archived.metadata.thread_key,
        })
    }

    async fn launch_workspace_new(&self, thread_key: &str) -> Result<LaunchWorkspaceResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        launch_hcodex_via_terminal(&config.launch_new_command).await?;
        Ok(LaunchWorkspaceResponse {
            launched: true,
            thread_key: thread_key.to_owned(),
            command: config.launch_new_command,
        })
    }

    async fn launch_workspace_resume(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<LaunchWorkspaceResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        let command = format!(
            "{} --thread-key {} resume {}",
            shell_quote_path(Path::new(&config.hcodex_path)),
            shell_quote(thread_key),
            shell_quote(session_id),
        );
        launch_hcodex_via_terminal(&command).await?;
        Ok(LaunchWorkspaceResponse {
            launched: true,
            thread_key: thread_key.to_owned(),
            command,
        })
    }

    async fn write_telegram_setup(&self, payload: UpdateTelegramSetupRequest) -> Result<()> {
        let mut updates = BTreeMap::new();
        updates.insert(
            "TELEGRAM_BOT_TOKEN".to_owned(),
            payload.telegram_token.trim().to_owned(),
        );
        let authorized = payload
            .authorized_user_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        updates.insert("AUTHORIZED_TELEGRAM_USER_IDS".to_owned(), authorized);
        let env_path = self.runtime.codex_working_directory.join(".env.local");
        write_env_file(&env_path, &updates).await
    }
}

async fn write_env_file(path: &Path, updates: &BTreeMap<String, String>) -> Result<()> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("failed to read {}", path.display())),
    };
    let mut lines = Vec::new();
    let mut seen = BTreeMap::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains('=') {
            lines.push(line.to_owned());
            continue;
        }
        let key = trimmed.split_once('=').map(|(key, _)| key.trim()).unwrap_or_default();
        if let Some(value) = updates.get(key) {
            lines.push(format!("{key}={value}"));
            seen.insert(key.to_owned(), true);
        } else {
            lines.push(line.to_owned());
        }
    }
    for (key, value) in updates {
        if !seen.contains_key(key) {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut output = lines.join("\n");
    output.push('\n');
    tokio::fs::write(path, output)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

async fn launch_hcodex_via_terminal(command: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
            apple_script_string(command)
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()
            .await
            .context("failed to launch Terminal via osascript")?;
        if !status.success() {
            return Err(anyhow!("osascript launch failed with status {status}"));
        }
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        Err(anyhow!("workspace launch is only implemented on macOS"))
    }
}

fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceRuntimeHealth {
    app_server_status: &'static str,
    tui_proxy_status: &'static str,
    handoff_readiness: &'static str,
}

async fn read_workspace_runtime_health(workspace_path: &Path) -> WorkspaceRuntimeHealth {
    let state_path = workspace_path
        .join(".threadbridge")
        .join("state")
        .join("app-server")
        .join("current.json");
    let contents = match tokio::fs::read_to_string(&state_path).await {
        Ok(contents) => contents,
        Err(_) => {
            return WorkspaceRuntimeHealth {
                app_server_status: "missing",
                tui_proxy_status: "missing",
                handoff_readiness: "unavailable",
            };
        }
    };
    let state: WorkspaceRuntimeState = match serde_json::from_str(&contents) {
        Ok(state) => state,
        Err(_) => {
            return WorkspaceRuntimeHealth {
                app_server_status: "invalid",
                tui_proxy_status: "invalid",
                handoff_readiness: "unavailable",
            };
        }
    };
    let app_server_running = tcp_endpoint_is_live(&state.daemon_ws_url).await;
    let proxy_running = match state.tui_proxy_base_ws_url.as_deref() {
        Some(url) => tcp_endpoint_is_live(url).await,
        None => false,
    };
    let app_server_status = if app_server_running { "running" } else { "stale" };
    let tui_proxy_status = match state.tui_proxy_base_ws_url.as_deref() {
        Some(_) if proxy_running => "running",
        Some(_) => "stale",
        None => "missing",
    };
    let handoff_readiness = if app_server_running && proxy_running {
        "ready"
    } else if app_server_running {
        "degraded"
    } else {
        "unavailable"
    };
    WorkspaceRuntimeHealth {
        app_server_status,
        tui_proxy_status,
        handoff_readiness,
    }
}

async fn tcp_endpoint_is_live(url: &str) -> bool {
    let Some(socket_addr) = url.strip_prefix("ws://") else {
        return false;
    };
    TcpStream::connect(socket_addr).await.is_ok()
}

fn aggregate_running_status<'a>(statuses: impl Iterator<Item = &'a str>) -> &'static str {
    let mut saw_any = false;
    let mut has_non_running = false;
    for status in statuses {
        saw_any = true;
        match status {
            "running" => {}
            _ => has_non_running = true,
        }
    }
    if !saw_any {
        return "missing";
    }
    if has_non_running {
        return "unavailable";
    }
    "running"
}

fn aggregate_handoff_status<'a>(statuses: impl Iterator<Item = &'a str>) -> &'static str {
    let mut saw_any = false;
    let mut has_unavailable = false;
    let mut has_degraded = false;
    for status in statuses {
        saw_any = true;
        match status {
            "ready" => {}
            "degraded" => has_degraded = true,
            _ => has_unavailable = true,
        }
    }
    if !saw_any {
        return "missing";
    }
    if has_unavailable {
        return "unavailable";
    }
    if has_degraded {
        return "degraded";
    }
    "ready"
}

#[derive(Debug)]
struct ManagementApiError(anyhow::Error);

impl From<anyhow::Error> for ManagementApiError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

impl IntoResponse for ManagementApiError {
    fn into_response(self) -> axum::response::Response {
        let message = serde_json::json!({
            "error": self.0.to_string(),
        });
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response()
    }
}
