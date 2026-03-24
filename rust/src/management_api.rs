use std::collections::BTreeMap;
use std::convert::Infallible;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_stream::stream;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{RuntimeConfig, load_optional_telegram_config};
use crate::execution_mode::{
    ExecutionMode, workspace_execution_mode, write_workspace_execution_config,
};
use crate::local_control::LocalControlHandle;
use crate::repository::{
    RecentCodexSessionEntry, ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry,
};
use crate::runtime_owner::{DesktopRuntimeOwner, RuntimeOwnerStatus};
pub use crate::runtime_protocol::{
    ArchivedThreadView, ManagedCodexBuildDefaultsView, ManagedCodexBuildInfoView, ManagedCodexView,
    ManagedWorkspaceView, RuntimeHealthView, ThreadStateView, WorkingSessionRecordView,
    WorkingSessionSummaryView,
};
use crate::runtime_protocol::{
    build_archived_thread_views, build_runtime_health, build_thread_views,
    build_working_session_records, build_working_session_summaries, build_workspace_views,
    workspace_mode_drift,
};
use crate::workspace::{ensure_workspace_runtime, validate_seed_template};

const MANAGED_CODEX_SOURCE_FILE: &str = ".threadbridge/codex/source.txt";
const MANAGED_CODEX_CACHE_BINARY: &str = ".threadbridge/codex/codex";
const MANAGED_CODEX_BUILD_INFO_FILE: &str = ".threadbridge/codex/build-info.txt";
const MANAGED_CODEX_BUILD_CONFIG_FILE: &str = ".threadbridge/codex/build-config.json";
const MANAGEMENT_UI_HTML: &str = include_str!("../static/management/index.html");
const MANAGEMENT_UI_CSS: &str = include_str!("../static/management/index.css");
const MANAGEMENT_UI_JS: &str = include_str!("../static/management/index.js");

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

    pub async fn set_local_control(&self, control: Option<LocalControlHandle>) {
        let mut current = self.state.local_control.write().await;
        *current = control;
    }

    pub async fn set_runtime_owner(&self, owner: Option<DesktopRuntimeOwner>) {
        let mut current = self.state.runtime_owner.write().await;
        *current = owner;
    }

    pub async fn set_native_workspace_picker_available(&self, available: bool) {
        let mut current = self.state.native_workspace_picker_available.write().await;
        *current = available;
    }

    pub async fn setup_state(&self) -> Result<SetupStateView> {
        self.state.setup_state().await
    }

    pub async fn runtime_health(&self) -> Result<RuntimeHealthView> {
        self.state.runtime_health().await
    }

    pub async fn workspace_views(&self) -> Result<Vec<ManagedWorkspaceView>> {
        self.state.workspace_views().await
    }

    pub async fn thread_views(&self) -> Result<Vec<ThreadStateView>> {
        self.state.thread_views().await
    }

    pub async fn archived_thread_views(&self) -> Result<Vec<ArchivedThreadView>> {
        self.state.archived_thread_views().await
    }

    pub async fn working_session_summaries(
        &self,
        thread_key: &str,
    ) -> Result<Vec<WorkingSessionSummaryView>> {
        self.state
            .working_session_summaries(thread_key)
            .await
            .map_err(|error| error.error)
    }

    pub async fn working_session_records(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<Vec<WorkingSessionRecordView>> {
        self.state
            .working_session_records(thread_key, session_id)
            .await
            .map_err(|error| error.error)
    }

    pub async fn workspace_execution_mode(
        &self,
        thread_key: &str,
    ) -> Result<WorkspaceExecutionModeView> {
        self.state.workspace_execution_mode_view(thread_key).await
    }

    pub async fn update_workspace_execution_mode(
        &self,
        thread_key: &str,
        execution_mode: ExecutionMode,
    ) -> Result<WorkspaceExecutionModeView> {
        self.state
            .update_workspace_execution_mode(thread_key, execution_mode)
            .await
    }

    pub async fn launch_workspace_new(&self, thread_key: &str) -> Result<LaunchWorkspaceResponse> {
        self.state.launch_workspace_new(thread_key).await
    }

    pub async fn launch_workspace_continue_current(
        &self,
        thread_key: &str,
    ) -> Result<LaunchWorkspaceResponse> {
        self.state
            .launch_workspace_continue_current(thread_key)
            .await
    }

    pub async fn launch_workspace_resume(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<LaunchWorkspaceResponse> {
        self.state
            .launch_workspace_resume(thread_key, session_id)
            .await
    }

    pub async fn add_workspace(&self, workspace_cwd: &str) -> Result<AddWorkspaceResult> {
        self.state.add_workspace(workspace_cwd).await
    }
}

#[derive(Clone)]
struct ManagementApiState {
    runtime: RuntimeConfig,
    repository: ThreadRepository,
    telegram_polling_state: Arc<RwLock<TelegramPollingState>>,
    local_control: Arc<RwLock<Option<LocalControlHandle>>>,
    runtime_owner: Arc<RwLock<Option<DesktopRuntimeOwner>>>,
    native_workspace_picker_available: Arc<RwLock<bool>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStateView {
    pub telegram_token_configured: bool,
    pub authorized_user_ids: Vec<i64>,
    pub authorized_user_count: usize,
    pub telegram_polling_state: TelegramPollingState,
    pub management_base_url: String,
    pub restart_required_after_setup_save: bool,
    pub control_chat_ready: bool,
    pub control_chat_id: Option<i64>,
    pub native_workspace_picker_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HcodexLaunchConfigView {
    pub workspace_cwd: String,
    pub thread_key: String,
    pub hcodex_path: String,
    pub hcodex_available: bool,
    pub workspace_execution_mode: ExecutionMode,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub mode_drift: bool,
    pub current_codex_thread_id: Option<String>,
    pub recent_codex_sessions: Vec<RecentCodexSessionEntry>,
    pub launch_new_command: String,
    pub launch_current_command: Option<String>,
    pub launch_resume_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceExecutionModeView {
    pub thread_key: String,
    pub workspace_cwd: String,
    pub workspace_execution_mode: ExecutionMode,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub mode_drift: bool,
}

#[derive(Debug, Serialize)]
struct ArchiveThreadResponse {
    archived: bool,
    thread_key: String,
}

#[derive(Debug, Serialize)]
struct ThreadMutationResponse {
    ok: bool,
    thread_key: String,
}

#[derive(Debug, Serialize)]
struct OpenWorkspaceResponse {
    opened: bool,
    thread_key: String,
    workspace_cwd: String,
}

#[derive(Debug, Serialize)]
struct PickAndAddWorkspaceResponse {
    ok: bool,
    created: bool,
    cancelled: bool,
    thread_key: Option<String>,
    title: Option<String>,
    workspace_cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LaunchResumeRequest {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct UpdateWorkspaceExecutionModeRequest {
    execution_mode: ExecutionMode,
}

#[derive(Debug, Deserialize)]
struct TranscriptQuery {
    #[serde(default)]
    delivery: Option<TranscriptMirrorDelivery>,
    #[serde(default = "default_transcript_limit")]
    limit: usize,
}

#[derive(Debug, Serialize)]
pub struct LaunchWorkspaceResponse {
    pub launched: bool,
    pub thread_key: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct AddWorkspaceResult {
    pub created: bool,
    pub thread_key: String,
    pub title: Option<String>,
    pub workspace_cwd: Option<String>,
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

#[derive(Debug, Serialize)]
struct ReconcileRuntimeOwnerResponse {
    ok: bool,
    report: crate::runtime_owner::RuntimeOwnerReconcileReport,
    status: RuntimeOwnerStatus,
}

#[derive(Debug, Deserialize)]
struct UpdateManagedCodexPreferenceRequest {
    source: String,
}

#[derive(Debug, Serialize)]
struct UpdateManagedCodexPreferenceResponse {
    updated: bool,
    source: String,
    synced_workspaces: usize,
}

#[derive(Debug, Serialize)]
struct RefreshManagedCodexCacheResponse {
    updated: bool,
    binary_path: String,
    version: Option<String>,
}

#[derive(Debug, Serialize)]
struct BuildManagedCodexSourceResponse {
    built: bool,
    binary_path: String,
    version: Option<String>,
    build_profile: String,
    source_repo: String,
    source_rs_dir: String,
}

#[derive(Debug, Deserialize)]
struct BuildManagedCodexSourceRequest {
    source_repo: Option<String>,
    source_rs_dir: Option<String>,
    build_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateManagedCodexBuildDefaultsRequest {
    source_repo: String,
    source_rs_dir: String,
    build_profile: String,
}

#[derive(Debug, Serialize)]
struct UpdateManagedCodexBuildDefaultsResponse {
    saved: bool,
    build_defaults: ManagedCodexBuildDefaultsView,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManagedCodexBuildConfigFile {
    #[serde(default)]
    source_repo: Option<String>,
    #[serde(default)]
    source_rs_dir: Option<String>,
    #[serde(default)]
    build_profile: Option<String>,
}

fn default_transcript_limit() -> usize {
    40
}

pub async fn spawn_management_api(runtime: RuntimeConfig) -> Result<ManagementApiHandle> {
    let repository = ThreadRepository::open(&runtime.data_root_path).await?;
    let state = Arc::new(ManagementApiState {
        runtime: runtime.clone(),
        repository,
        telegram_polling_state: Arc::new(RwLock::new(TelegramPollingState::Disconnected)),
        local_control: Arc::new(RwLock::new(None)),
        runtime_owner: Arc::new(RwLock::new(None)),
        native_workspace_picker_available: Arc::new(RwLock::new(false)),
    });
    let bind_addr = runtime.management_bind_addr;
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind local management API at {bind_addr}"))?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let router = Router::new()
        .route("/", get(index))
        .route("/assets/management.css", get(management_css))
        .route("/assets/management.js", get(management_js))
        .route("/api/setup", get(get_setup))
        .route("/api/setup/telegram", put(put_telegram_setup))
        .route(
            "/api/managed-codex/preference",
            post(post_update_managed_codex_preference),
        )
        .route(
            "/api/managed-codex/refresh-cache",
            post(post_refresh_managed_codex_cache),
        )
        .route(
            "/api/managed-codex/build-source",
            post(post_build_managed_codex_source),
        )
        .route(
            "/api/managed-codex/build-defaults",
            post(post_update_managed_codex_build_defaults),
        )
        .route("/api/runtime-health", get(get_runtime_health))
        .route(
            "/api/runtime-owner/reconcile",
            post(post_reconcile_runtime_owner),
        )
        .route("/api/threads", get(get_threads))
        .route(
            "/api/threads/:thread_key/transcript",
            get(get_thread_transcript),
        )
        .route("/api/threads/:thread_key/sessions", get(get_working_sessions))
        .route(
            "/api/threads/:thread_key/sessions/:session_id/records",
            get(get_working_session_records),
        )
        .route("/api/workspaces", get(get_workspaces))
        .route(
            "/api/workspaces/pick-and-add",
            post(post_pick_and_add_workspace),
        )
        .route("/api/archived-threads", get(get_archived_threads))
        .route(
            "/api/threads/:thread_key/adopt-tui",
            post(post_adopt_tui_session),
        )
        .route(
            "/api/threads/:thread_key/reject-tui",
            post(post_reject_tui_session),
        )
        .route(
            "/api/workspaces/:thread_key/launch-config",
            get(get_workspace_launch_config),
        )
        .route(
            "/api/workspaces/:thread_key/execution-mode",
            get(get_workspace_execution_mode).put(put_workspace_execution_mode),
        )
        .route(
            "/api/workspaces/:thread_key/reconnect",
            post(post_reconnect_codex),
        )
        .route(
            "/api/workspaces/:thread_key/open",
            post(post_open_workspace),
        )
        .route(
            "/api/workspaces/:thread_key/repair-runtime",
            post(post_repair_workspace_runtime),
        )
        .route(
            "/api/workspaces/:thread_key/launch-new",
            post(post_launch_workspace_new),
        )
        .route(
            "/api/workspaces/:thread_key/launch-current",
            post(post_launch_workspace_continue_current),
        )
        .route(
            "/api/workspaces/:thread_key/launch-resume",
            post(post_launch_workspace_resume),
        )
        .route(
            "/api/threads/:thread_key/archive",
            post(post_archive_thread),
        )
        .route(
            "/api/threads/:thread_key/restore",
            post(post_restore_thread),
        )
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
    let html = MANAGEMENT_UI_HTML.replace(
        "__MANAGEMENT_BIND_ADDR__",
        &state.runtime.management_bind_addr.to_string(),
    );
    Html(html)
}

async fn management_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        MANAGEMENT_UI_CSS,
    )
}

async fn management_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        MANAGEMENT_UI_JS,
    )
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
        restart_required: state.restart_required_after_setup_save().await,
    }))
}

async fn post_reconcile_runtime_owner(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<ReconcileRuntimeOwnerResponse>, ManagementApiError> {
    Ok(Json(state.reconcile_runtime_owner().await?))
}

async fn post_update_managed_codex_preference(
    State(state): State<Arc<ManagementApiState>>,
    Json(payload): Json<UpdateManagedCodexPreferenceRequest>,
) -> Result<Json<UpdateManagedCodexPreferenceResponse>, ManagementApiError> {
    Ok(Json(
        state
            .update_managed_codex_preference(&payload.source)
            .await?,
    ))
}

async fn post_refresh_managed_codex_cache(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<RefreshManagedCodexCacheResponse>, ManagementApiError> {
    Ok(Json(state.refresh_managed_codex_cache().await?))
}

async fn post_build_managed_codex_source(
    State(state): State<Arc<ManagementApiState>>,
    Json(payload): Json<Option<BuildManagedCodexSourceRequest>>,
) -> Result<Json<BuildManagedCodexSourceResponse>, ManagementApiError> {
    Ok(Json(state.build_managed_codex_source(payload).await?))
}

async fn post_update_managed_codex_build_defaults(
    State(state): State<Arc<ManagementApiState>>,
    Json(payload): Json<UpdateManagedCodexBuildDefaultsRequest>,
) -> Result<Json<UpdateManagedCodexBuildDefaultsResponse>, ManagementApiError> {
    Ok(Json(
        state.update_managed_codex_build_defaults(payload).await?,
    ))
}

async fn get_runtime_health(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<RuntimeHealthView>, ManagementApiError> {
    Ok(Json(state.runtime_health().await?))
}

async fn get_threads(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ThreadStateView>>, ManagementApiError> {
    Ok(Json(state.thread_views().await?))
}

async fn get_thread_transcript(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
    Query(query): Query<TranscriptQuery>,
) -> Result<Json<Vec<TranscriptMirrorEntry>>, ManagementApiError> {
    Ok(Json(
        state
            .thread_transcript(&thread_key, query.delivery, query.limit)
            .await?,
    ))
}

async fn get_working_sessions(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<Vec<WorkingSessionSummaryView>>, ManagementApiError> {
    Ok(Json(state.working_session_summaries(&thread_key).await?))
}

async fn get_working_session_records(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath((thread_key, session_id)): AxumPath<(String, String)>,
) -> Result<Json<Vec<WorkingSessionRecordView>>, ManagementApiError> {
    Ok(Json(
        state
            .working_session_records(&thread_key, &session_id)
            .await?,
    ))
}

async fn get_workspaces(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ManagedWorkspaceView>>, ManagementApiError> {
    Ok(Json(state.workspace_views().await?))
}

async fn post_pick_and_add_workspace(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<PickAndAddWorkspaceResponse>, ManagementApiError> {
    Ok(Json(state.pick_and_add_workspace().await?))
}

async fn get_archived_threads(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ArchivedThreadView>>, ManagementApiError> {
    Ok(Json(state.archived_thread_views().await?))
}

async fn post_adopt_tui_session(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ThreadMutationResponse>, ManagementApiError> {
    Ok(Json(state.adopt_tui_session(&thread_key).await?))
}

async fn post_reject_tui_session(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ThreadMutationResponse>, ManagementApiError> {
    Ok(Json(state.reject_tui_session(&thread_key).await?))
}

async fn get_workspace_launch_config(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<HcodexLaunchConfigView>, ManagementApiError> {
    Ok(Json(state.workspace_launch_config(&thread_key).await?))
}

async fn get_workspace_execution_mode(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<WorkspaceExecutionModeView>, ManagementApiError> {
    Ok(Json(
        state.workspace_execution_mode_view(&thread_key).await?,
    ))
}

async fn put_workspace_execution_mode(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
    Json(payload): Json<UpdateWorkspaceExecutionModeRequest>,
) -> Result<Json<WorkspaceExecutionModeView>, ManagementApiError> {
    Ok(Json(
        state
            .update_workspace_execution_mode(&thread_key, payload.execution_mode)
            .await?,
    ))
}

async fn post_reconnect_codex(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ThreadMutationResponse>, ManagementApiError> {
    Ok(Json(state.reconnect_codex(&thread_key).await?))
}

async fn post_open_workspace(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<OpenWorkspaceResponse>, ManagementApiError> {
    Ok(Json(state.open_workspace(&thread_key).await?))
}

async fn post_repair_workspace_runtime(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ThreadMutationResponse>, ManagementApiError> {
    Ok(Json(state.repair_workspace_runtime(&thread_key).await?))
}

async fn post_launch_workspace_new(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<LaunchWorkspaceResponse>, ManagementApiError> {
    Ok(Json(state.launch_workspace_new(&thread_key).await?))
}

async fn post_launch_workspace_continue_current(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<LaunchWorkspaceResponse>, ManagementApiError> {
    Ok(Json(
        state.launch_workspace_continue_current(&thread_key).await?,
    ))
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

async fn post_restore_thread(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
) -> Result<Json<ThreadMutationResponse>, ManagementApiError> {
    Ok(Json(state.restore_thread(&thread_key).await?))
}

async fn get_events(
    State(state): State<Arc<ManagementApiState>>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = stream! {
        loop {
            let setup = state.setup_state().await.ok();
            let runtime = state.runtime_health().await.ok();
            let threads = state.thread_views().await.ok();
            let workspaces = state.workspace_views().await.ok();
            let archived = state.archived_thread_views().await.ok();
            let payload = serde_json::json!({
                "setup": setup,
                "runtime": runtime,
                "threads": threads,
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
    async fn local_control(&self) -> Result<LocalControlHandle> {
        self.local_control
            .read()
            .await
            .clone()
            .context("Telegram bot runtime is not active. Configure credentials and start the desktop runtime first.")
    }

    async fn restart_required_after_setup_save(&self) -> bool {
        self.runtime_owner.read().await.is_none()
    }

    async fn setup_state(&self) -> Result<SetupStateView> {
        let telegram = load_optional_telegram_config()?;
        let main_thread = self.repository.find_main_thread().await?;
        Ok(SetupStateView {
            telegram_token_configured: telegram.is_some(),
            authorized_user_ids: telegram
                .as_ref()
                .map(|config| {
                    let mut ids = config
                        .authorized_user_ids
                        .iter()
                        .copied()
                        .collect::<Vec<_>>();
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
            restart_required_after_setup_save: self.restart_required_after_setup_save().await,
            control_chat_ready: main_thread.is_some(),
            control_chat_id: main_thread.map(|record| record.metadata.chat_id),
            native_workspace_picker_available: *self.native_workspace_picker_available.read().await,
        })
    }

    async fn runtime_health(&self) -> Result<RuntimeHealthView> {
        let workspaces = self.workspace_views().await?;
        let runtime_owner = match self.runtime_owner.read().await.clone() {
            Some(owner) => owner.status().await,
            None => RuntimeOwnerStatus::inactive(),
        };
        Ok(build_runtime_health(
            self.runtime.management_bind_addr.to_string(),
            &workspaces,
            runtime_owner,
            self.managed_codex_view().await?,
        ))
    }

    async fn managed_codex_view(&self) -> Result<ManagedCodexView> {
        let repo_root = &self.runtime.codex_working_directory;
        let source = read_managed_codex_source_preference(repo_root).await?;
        let binary_path = resolve_managed_codex_binary_path(repo_root, source).await?;
        let binary_ready = binary_path.as_ref().is_some_and(|path| path.exists());
        let build_config_path = repo_root.join(MANAGED_CODEX_BUILD_CONFIG_FILE);
        let build_info_path = repo_root.join(MANAGED_CODEX_BUILD_INFO_FILE);
        let build_defaults = resolve_managed_codex_build_defaults(repo_root).await?;
        let version = match binary_path.as_deref() {
            Some(path) if path.exists() => read_codex_version(path).await.ok(),
            _ => None,
        };
        Ok(ManagedCodexView {
            source: source.as_str(),
            source_file_path: repo_root
                .join(MANAGED_CODEX_SOURCE_FILE)
                .display()
                .to_string(),
            build_config_file_path: build_config_path.display().to_string(),
            build_info_file_path: build_info_path.display().to_string(),
            binary_path: binary_path
                .unwrap_or_else(|| repo_root.join(MANAGED_CODEX_CACHE_BINARY))
                .display()
                .to_string(),
            binary_ready,
            version,
            build_defaults: ManagedCodexBuildDefaultsView {
                source_repo: build_defaults.source_repo.display().to_string(),
                source_rs_dir: build_defaults.source_rs_dir.display().to_string(),
                build_profile: build_defaults.build_profile.as_str().to_owned(),
            },
            build_info: read_managed_codex_build_info(&build_info_path).await?,
        })
    }

    async fn update_managed_codex_preference(
        &self,
        source: &str,
    ) -> Result<UpdateManagedCodexPreferenceResponse> {
        let source = ManagedCodexSourcePreference::parse(source)?;
        write_managed_codex_source_preference(&self.runtime.codex_working_directory, source)
            .await?;
        let seed_template_path = validate_seed_template(
            &self
                .runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        let mut synced_workspaces = 0usize;
        let mut seen = BTreeMap::new();
        for record in self.repository.list_active_threads().await? {
            let Some(binding) = self.repository.read_session_binding(&record).await? else {
                continue;
            };
            let Some(workspace_cwd) = binding.workspace_cwd else {
                continue;
            };
            if seen.contains_key(&workspace_cwd) {
                continue;
            }
            ensure_workspace_runtime(
                &self.runtime.codex_working_directory,
                &self.runtime.data_root_path,
                &seed_template_path,
                Path::new(&workspace_cwd),
            )
            .await?;
            seen.insert(workspace_cwd, true);
            synced_workspaces += 1;
        }
        Ok(UpdateManagedCodexPreferenceResponse {
            updated: true,
            source: source.as_str().to_owned(),
            synced_workspaces,
        })
    }

    async fn refresh_managed_codex_cache(&self) -> Result<RefreshManagedCodexCacheResponse> {
        let source_binary = resolve_codex_from_path().await?;
        let dest_path = self
            .runtime
            .codex_working_directory
            .join(MANAGED_CODEX_CACHE_BINARY);
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        tokio::fs::copy(&source_binary, &dest_path)
            .await
            .with_context(|| {
                format!(
                    "failed to copy managed Codex cache from {} to {}",
                    source_binary.display(),
                    dest_path.display()
                )
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = tokio::fs::metadata(&dest_path).await?;
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            tokio::fs::set_permissions(&dest_path, permissions).await?;
        }
        let version = read_codex_version(&dest_path).await.ok();
        Ok(RefreshManagedCodexCacheResponse {
            updated: true,
            binary_path: dest_path.display().to_string(),
            version,
        })
    }

    async fn build_managed_codex_source(
        &self,
        payload: Option<BuildManagedCodexSourceRequest>,
    ) -> Result<BuildManagedCodexSourceResponse> {
        let defaults =
            read_managed_codex_build_config(&self.runtime.codex_working_directory).await?;
        let build = ManagedCodexSourceBuild::from_sources(
            payload
                .as_ref()
                .and_then(|payload| payload.source_repo.as_deref())
                .or(defaults.source_repo.as_deref()),
            payload
                .as_ref()
                .and_then(|payload| payload.source_rs_dir.as_deref())
                .or(defaults.source_rs_dir.as_deref()),
            payload
                .as_ref()
                .and_then(|payload| payload.build_profile.as_deref())
                .or(defaults.build_profile.as_deref()),
        )?;
        if !build.source_rs_dir.is_dir() {
            return Err(anyhow!(
                "missing Codex source workspace: {}",
                build.source_rs_dir.display()
            ));
        }
        let source_manifest = build.source_rs_dir.join("Cargo.toml");
        if !source_manifest.exists() {
            return Err(anyhow!(
                "missing Codex Cargo.toml: {}",
                source_manifest.display()
            ));
        }

        let mut command = Command::new("cargo");
        command.current_dir(&build.source_rs_dir);
        command.env("CARGO_HOME", &build.cargo_home);
        command.env("CARGO_TARGET_DIR", &build.cargo_target_dir);
        command.env("RUSTUP_HOME", &build.rustup_home);
        command.arg("build");
        if build.build_profile == ManagedCodexBuildProfile::Release {
            command.arg("--release");
        }
        command.arg("-p").arg("codex-cli");
        let output = command.output().await.with_context(|| {
            format!("failed to build Codex in {}", build.source_rs_dir.display())
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            return Err(anyhow!(
                "source Codex build failed in {}: {}",
                build.source_rs_dir.display(),
                detail
            ));
        }

        let source_binary = build.built_binary_path();
        if !source_binary.exists() {
            return Err(anyhow!(
                "expected built Codex binary at {}",
                source_binary.display()
            ));
        }

        let managed_dir = self
            .runtime
            .codex_working_directory
            .join(".threadbridge/codex");
        tokio::fs::create_dir_all(&managed_dir)
            .await
            .with_context(|| format!("failed to create {}", managed_dir.display()))?;
        let dest_path = self
            .runtime
            .codex_working_directory
            .join(MANAGED_CODEX_CACHE_BINARY);
        tokio::fs::copy(&source_binary, &dest_path)
            .await
            .with_context(|| {
                format!(
                    "failed to copy source-built Codex from {} to {}",
                    source_binary.display(),
                    dest_path.display()
                )
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = tokio::fs::metadata(&dest_path).await?;
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            tokio::fs::set_permissions(&dest_path, permissions).await?;
        }

        let git_rev = resolve_git_rev(&build.source_repo)
            .await
            .unwrap_or_else(|_| "unknown".to_owned());
        let build_info = format!(
            "source_repo={}\nsource_rs_dir={}\nbuild_profile={}\ngit_rev={}\nbinary={}\n",
            build.source_repo.display(),
            build.source_rs_dir.display(),
            build.build_profile.as_str(),
            git_rev,
            source_binary.display()
        );
        let build_info_path = self
            .runtime
            .codex_working_directory
            .join(MANAGED_CODEX_BUILD_INFO_FILE);
        tokio::fs::write(&build_info_path, build_info)
            .await
            .with_context(|| format!("failed to write {}", build_info_path.display()))?;

        let version = read_codex_version(&dest_path).await.ok();
        Ok(BuildManagedCodexSourceResponse {
            built: true,
            binary_path: dest_path.display().to_string(),
            version,
            build_profile: build.build_profile.as_str().to_owned(),
            source_repo: build.source_repo.display().to_string(),
            source_rs_dir: build.source_rs_dir.display().to_string(),
        })
    }

    async fn update_managed_codex_build_defaults(
        &self,
        payload: UpdateManagedCodexBuildDefaultsRequest,
    ) -> Result<UpdateManagedCodexBuildDefaultsResponse> {
        let build = ManagedCodexSourceBuild::from_sources(
            Some(payload.source_repo.trim()),
            Some(payload.source_rs_dir.trim()),
            Some(payload.build_profile.trim()),
        )?;
        write_managed_codex_build_config(
            &self.runtime.codex_working_directory,
            &ManagedCodexBuildConfigFile {
                source_repo: Some(build.source_repo.display().to_string()),
                source_rs_dir: Some(build.source_rs_dir.display().to_string()),
                build_profile: Some(build.build_profile.as_str().to_owned()),
            },
        )
        .await?;
        Ok(UpdateManagedCodexBuildDefaultsResponse {
            saved: true,
            build_defaults: ManagedCodexBuildDefaultsView {
                source_repo: build.source_repo.display().to_string(),
                source_rs_dir: build.source_rs_dir.display().to_string(),
                build_profile: build.build_profile.as_str().to_owned(),
            },
        })
    }

    async fn workspace_views(&self) -> Result<Vec<ManagedWorkspaceView>> {
        let runtime_owner = self.runtime_owner.read().await.clone();
        build_workspace_views(&self.repository, runtime_owner.as_ref()).await
    }

    async fn thread_views(&self) -> Result<Vec<ThreadStateView>> {
        build_thread_views(&self.repository).await
    }

    async fn thread_transcript(
        &self,
        thread_key: &str,
        delivery: Option<TranscriptMirrorDelivery>,
        limit: usize,
    ) -> Result<Vec<TranscriptMirrorEntry>, ManagementApiError> {
        let record = self
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .ok_or_else(|| {
                ManagementApiError::not_found(anyhow!("active thread `{thread_key}` not found"))
            })?;
        Ok(self
            .repository
            .read_transcript_mirror(&record, delivery, limit.max(1))
            .await?)
    }

    async fn working_session_summaries(
        &self,
        thread_key: &str,
    ) -> Result<Vec<WorkingSessionSummaryView>, ManagementApiError> {
        let record = self
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .ok_or_else(|| {
                ManagementApiError::not_found(anyhow!("active thread `{thread_key}` not found"))
            })?;
        let binding = self
            .repository
            .read_session_binding(&record)
            .await?
            .context("managed workspace is missing session binding")?;
        Ok(build_working_session_summaries(&self.repository, &record, &binding).await?)
    }

    async fn working_session_records(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<Vec<WorkingSessionRecordView>, ManagementApiError> {
        let record = self
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .ok_or_else(|| {
                ManagementApiError::not_found(anyhow!("active thread `{thread_key}` not found"))
            })?;
        let binding = self
            .repository
            .read_session_binding(&record)
            .await?
            .context("managed workspace is missing session binding")?;
        build_working_session_records(&self.repository, &record, &binding, session_id)
            .await?
            .ok_or_else(|| {
                ManagementApiError::not_found(anyhow!(
                    "session `{session_id}` not found for active thread `{thread_key}`"
                ))
            })
    }

    async fn archived_thread_views(&self) -> Result<Vec<ArchivedThreadView>> {
        build_archived_thread_views(&self.repository).await
    }

    async fn add_workspace(&self, workspace_cwd: &str) -> Result<AddWorkspaceResult> {
        let control = self.local_control().await?;
        let outcome = control.add_workspace(workspace_cwd).await?;
        let record = outcome.record();
        let binding = self.repository.read_session_binding(record).await?;
        let bound_workspace_cwd = binding
            .as_ref()
            .and_then(|binding| binding.workspace_cwd.clone())
            .or_else(|| Some(workspace_cwd.trim().to_owned()));
        if let Some(workspace_cwd) = bound_workspace_cwd.as_deref() {
            self.maybe_reconcile_owner_workspace(workspace_cwd).await?;
        }
        Ok(AddWorkspaceResult {
            created: outcome.created(),
            thread_key: record.metadata.thread_key.clone(),
            title: record.metadata.title.clone(),
            workspace_cwd: bound_workspace_cwd,
        })
    }

    async fn pick_and_add_workspace(&self) -> Result<PickAndAddWorkspaceResponse> {
        anyhow::ensure!(
            *self.native_workspace_picker_available.read().await,
            "Native workspace picker is unavailable. Start threadBridge in desktop mode."
        );
        anyhow::ensure!(
            *self.telegram_polling_state.read().await == TelegramPollingState::Active,
            "Telegram bot runtime is not active yet. Wait for desktop runtime to reconnect polling first."
        );
        anyhow::ensure!(
            self.repository.find_main_thread().await?.is_some(),
            "Control chat is not ready yet. Send /start to the bot from the target Telegram chat first."
        );
        let Some(workspace_cwd) = pick_workspace_folder().await? else {
            return Ok(PickAndAddWorkspaceResponse {
                ok: true,
                created: false,
                cancelled: true,
                thread_key: None,
                title: None,
                workspace_cwd: None,
            });
        };
        let result = self.add_workspace(&workspace_cwd).await?;
        Ok(PickAndAddWorkspaceResponse {
            ok: true,
            created: result.created,
            cancelled: false,
            thread_key: Some(result.thread_key),
            title: result.title,
            workspace_cwd: result.workspace_cwd,
        })
    }

    async fn adopt_tui_session(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.local_control().await?;
        let record = control.adopt_tui_session(thread_key).await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: record.metadata.thread_key,
        })
    }

    async fn reject_tui_session(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.local_control().await?;
        let record = control.reject_tui_session(thread_key).await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: record.metadata.thread_key,
        })
    }

    async fn workspace_execution_mode_view(
        &self,
        thread_key: &str,
    ) -> Result<WorkspaceExecutionModeView> {
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
        let workspace_execution_mode = workspace_execution_mode(Path::new(&workspace_cwd)).await?;
        Ok(WorkspaceExecutionModeView {
            thread_key: record.metadata.thread_key,
            workspace_cwd,
            workspace_execution_mode,
            current_execution_mode: binding.current_execution_mode,
            current_approval_policy: binding.current_approval_policy.clone(),
            current_sandbox_policy: binding.current_sandbox_policy.clone(),
            mode_drift: workspace_mode_drift(workspace_execution_mode, &binding),
        })
    }

    async fn update_workspace_execution_mode(
        &self,
        thread_key: &str,
        execution_mode: ExecutionMode,
    ) -> Result<WorkspaceExecutionModeView> {
        let current = self.workspace_execution_mode_view(thread_key).await?;
        write_workspace_execution_config(Path::new(&current.workspace_cwd), execution_mode).await?;
        self.workspace_execution_mode_view(thread_key).await
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
        let workspace_execution_mode = workspace_execution_mode(Path::new(&workspace_cwd)).await?;
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
            workspace_execution_mode,
            current_execution_mode: binding.current_execution_mode,
            current_approval_policy: binding.current_approval_policy.clone(),
            current_sandbox_policy: binding.current_sandbox_policy.clone(),
            mode_drift: workspace_mode_drift(workspace_execution_mode, &binding),
            current_codex_thread_id: binding.current_codex_thread_id.clone(),
            launch_new_command: hcodex_launch_command(
                &hcodex_path,
                thread_key,
                workspace_execution_mode,
                None,
            ),
            launch_current_command: binding.current_codex_thread_id.as_ref().map(|session_id| {
                hcodex_launch_command(
                    &hcodex_path,
                    thread_key,
                    workspace_execution_mode,
                    Some(session_id),
                )
            }),
            launch_resume_commands: recent_codex_sessions
                .iter()
                .map(|entry| {
                    hcodex_launch_command(
                        &hcodex_path,
                        thread_key,
                        workspace_execution_mode,
                        Some(&entry.session_id),
                    )
                })
                .collect(),
            recent_codex_sessions,
        })
    }

    async fn archive_thread(&self, thread_key: &str) -> Result<ArchiveThreadResponse> {
        let archived = match self.local_control.read().await.clone() {
            Some(control) => control.archive_thread(thread_key).await?,
            None => {
                let record = self
                    .repository
                    .find_active_thread_by_key(thread_key)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("thread_key is not an active thread"))?;
                self.repository.archive_thread(record).await?
            }
        };
        Ok(ArchiveThreadResponse {
            archived: true,
            thread_key: archived.metadata.thread_key,
        })
    }

    async fn restore_thread(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.local_control().await?;
        let restored = control.restore_thread(thread_key).await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: restored.metadata.thread_key,
        })
    }

    async fn reconnect_codex(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        self.maybe_reconcile_owner_workspace(&config.workspace_cwd)
            .await?;
        let control = self.local_control().await?;
        let record = control.reconnect_codex(thread_key).await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: record.metadata.thread_key,
        })
    }

    async fn open_workspace(&self, thread_key: &str) -> Result<OpenWorkspaceResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        open_workspace_path(Path::new(&config.workspace_cwd)).await?;
        Ok(OpenWorkspaceResponse {
            opened: true,
            thread_key: thread_key.to_owned(),
            workspace_cwd: config.workspace_cwd,
        })
    }

    async fn repair_workspace_runtime(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let owner = self
            .runtime_owner
            .read()
            .await
            .clone()
            .context("desktop runtime owner is not active")?;
        let config = self.workspace_launch_config(thread_key).await?;
        let _ = owner
            .reconcile_managed_workspaces([config.workspace_cwd.as_str()])
            .await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: thread_key.to_owned(),
        })
    }

    async fn reconcile_runtime_owner(&self) -> Result<ReconcileRuntimeOwnerResponse> {
        let owner = self
            .runtime_owner
            .read()
            .await
            .clone()
            .context("desktop runtime owner is not active")?;
        let targets = self
            .workspace_views()
            .await?
            .into_iter()
            .filter(|workspace| !workspace.conflict)
            .map(|workspace| workspace.workspace_cwd)
            .collect::<Vec<_>>();
        let report = owner.reconcile_managed_workspaces(targets).await?;
        let status = owner.status().await;
        Ok(ReconcileRuntimeOwnerResponse {
            ok: true,
            report,
            status,
        })
    }

    async fn maybe_reconcile_owner_workspace(&self, workspace_cwd: &str) -> Result<()> {
        let owner = self
            .runtime_owner
            .read()
            .await
            .clone()
            .context("desktop runtime owner is not active")?;
        let _ = owner.reconcile_managed_workspaces([workspace_cwd]).await?;
        Ok(())
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

    async fn launch_workspace_continue_current(
        &self,
        thread_key: &str,
    ) -> Result<LaunchWorkspaceResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        let command = config
            .launch_current_command
            .clone()
            .context("managed workspace is missing a current Telegram session")?;
        launch_hcodex_via_terminal(&command).await?;
        Ok(LaunchWorkspaceResponse {
            launched: true,
            thread_key: thread_key.to_owned(),
            command,
        })
    }

    async fn launch_workspace_resume(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<LaunchWorkspaceResponse> {
        let config = self.workspace_launch_config(thread_key).await?;
        let command = hcodex_launch_command(
            Path::new(&config.hcodex_path),
            thread_key,
            config.workspace_execution_mode,
            Some(session_id),
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
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut lines = Vec::new();
    let mut seen = BTreeMap::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains('=') {
            lines.push(line.to_owned());
            continue;
        }
        let key = trimmed
            .split_once('=')
            .map(|(key, _)| key.trim())
            .unwrap_or_default();
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

#[cfg(target_os = "macos")]
async fn pick_workspace_folder() -> Result<Option<String>> {
    let script =
        r#"POSIX path of (choose folder with prompt "Select a workspace to add to threadBridge")"#;
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await?;
    if output.status.success() {
        let chosen = parse_choose_folder_output(&String::from_utf8_lossy(&output.stdout));
        return chosen
            .map(Some)
            .ok_or_else(|| anyhow!("workspace selection returned an empty path"));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if apple_script_user_cancelled(output.status.code(), &stderr) {
        return Ok(None);
    }
    Err(anyhow!(
        "workspace selection failed: {}",
        fallback_if_blank(stderr.trim(), "unknown osascript error")
    ))
}

#[cfg(not(target_os = "macos"))]
async fn pick_workspace_folder() -> Result<Option<String>> {
    Err(anyhow!(
        "Native workspace picker is unavailable on this platform."
    ))
}

fn parse_choose_folder_output(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "/" {
        return Some("/".to_owned());
    }
    Some(trimmed.trim_end_matches('/').to_owned())
}

fn apple_script_user_cancelled(status_code: Option<i32>, stderr: &str) -> bool {
    matches!(status_code, Some(1) | Some(-128))
        && stderr.to_ascii_lowercase().contains("user canceled")
}

fn fallback_if_blank(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

fn hcodex_launch_command(
    hcodex_path: &Path,
    thread_key: &str,
    execution_mode: ExecutionMode,
    session_id: Option<&str>,
) -> String {
    match session_id {
        Some(session_id) => format!(
            "{} --thread-key {} {} resume {}",
            shell_quote_path(hcodex_path),
            shell_quote(thread_key),
            execution_mode.hcodex_flag(),
            shell_quote(session_id)
        ),
        None => format!(
            "{} --thread-key {} {}",
            shell_quote_path(hcodex_path),
            shell_quote(thread_key),
            execution_mode.hcodex_flag()
        ),
    }
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

async fn open_workspace_path(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("/usr/bin/open")
            .arg(path)
            .status()
            .await
            .with_context(|| format!("failed to open workspace {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!(
                "open workspace failed for {} with status {}",
                path.display(),
                status
            ));
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let status = Command::new("xdg-open")
            .arg(path)
            .status()
            .await
            .with_context(|| format!("failed to open workspace {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!(
                "open workspace failed for {} with status {}",
                path.display(),
                status
            ));
        }
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let status = Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(path)
            .status()
            .await
            .with_context(|| format!("failed to open workspace {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!(
                "open workspace failed for {} with status {}",
                path.display(),
                status
            ));
        }
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        Err(anyhow!(
            "open workspace is not implemented on this platform"
        ))
    }
}

fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedCodexSourcePreference {
    Brew,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedCodexBuildProfile {
    Dev,
    Release,
}

impl ManagedCodexBuildProfile {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "dev" => Ok(Self::Dev),
            "release" => Ok(Self::Release),
            other => Err(anyhow!("unsupported CODEX_BUILD_PROFILE: {other}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Clone)]
struct ManagedCodexSourceBuild {
    source_repo: PathBuf,
    source_rs_dir: PathBuf,
    build_profile: ManagedCodexBuildProfile,
    cargo_home: PathBuf,
    cargo_target_dir: PathBuf,
    rustup_home: PathBuf,
}

impl ManagedCodexSourceBuild {
    fn from_sources(
        source_repo_override: Option<&str>,
        source_rs_dir_override: Option<&str>,
        build_profile_override: Option<&str>,
    ) -> Result<Self> {
        Self::from_env_with_overrides(
            source_repo_override,
            source_rs_dir_override,
            build_profile_override,
        )
    }

    fn from_env_with_overrides(
        source_repo_override: Option<&str>,
        source_rs_dir_override: Option<&str>,
        build_profile_override: Option<&str>,
    ) -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let default_build_profile = env::var("BUILD_PROFILE").unwrap_or_else(|_| "dev".to_owned());
        let build_profile_value = build_profile_override
            .map(str::to_owned)
            .unwrap_or_else(|| env::var("CODEX_BUILD_PROFILE").unwrap_or(default_build_profile));
        let build_profile = ManagedCodexBuildProfile::parse(&build_profile_value)?;
        let source_repo = source_repo_override
            .map(PathBuf::from)
            .or_else(|| env::var_os("CODEX_SOURCE_REPO").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/Volumes/Data/Github/codex"));
        let source_rs_dir = source_rs_dir_override
            .map(PathBuf::from)
            .or_else(|| env::var_os("CODEX_SOURCE_RS_DIR").map(PathBuf::from))
            .unwrap_or_else(|| source_repo.join("codex-rs"));
        let cargo_home = env::var_os("CODEX_CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        let cargo_target_dir = env::var_os("CODEX_CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| source_rs_dir.join("target"));
        let rustup_home = env::var_os("CODEX_RUSTUP_HOME")
            .or_else(|| env::var_os("RUSTUP_HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rustup"));
        Ok(Self {
            source_repo,
            source_rs_dir,
            build_profile,
            cargo_home,
            cargo_target_dir,
            rustup_home,
        })
    }

    fn built_binary_path(&self) -> PathBuf {
        match self.build_profile {
            ManagedCodexBuildProfile::Dev => self.cargo_target_dir.join("debug").join("codex"),
            ManagedCodexBuildProfile::Release => {
                self.cargo_target_dir.join("release").join("codex")
            }
        }
    }
}

async fn read_managed_codex_build_config(repo_root: &Path) -> Result<ManagedCodexBuildConfigFile> {
    let path = repo_root.join(MANAGED_CODEX_BUILD_CONFIG_FILE);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManagedCodexBuildConfigFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    serde_json::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

async fn write_managed_codex_build_config(
    repo_root: &Path,
    config: &ManagedCodexBuildConfigFile,
) -> Result<()> {
    let path = repo_root.join(MANAGED_CODEX_BUILD_CONFIG_FILE);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(config)?;
    tokio::fs::write(&path, format!("{contents}\n"))
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

async fn resolve_managed_codex_build_defaults(repo_root: &Path) -> Result<ManagedCodexSourceBuild> {
    let config = read_managed_codex_build_config(repo_root).await?;
    ManagedCodexSourceBuild::from_sources(
        config.source_repo.as_deref(),
        config.source_rs_dir.as_deref(),
        config.build_profile.as_deref(),
    )
}

impl ManagedCodexSourcePreference {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "brew" => Ok(Self::Brew),
            "alpha" | "source" => Ok(Self::Source),
            other => Err(anyhow!(
                "unsupported managed Codex source preference: {other}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::Source => "source",
        }
    }
}

async fn read_managed_codex_source_preference(
    repo_root: &Path,
) -> Result<ManagedCodexSourcePreference> {
    let path = repo_root.join(MANAGED_CODEX_SOURCE_FILE);
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => ManagedCodexSourcePreference::parse(contents.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ManagedCodexSourcePreference::Brew)
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

async fn write_managed_codex_source_preference(
    repo_root: &Path,
    source: ManagedCodexSourcePreference,
) -> Result<()> {
    let path = repo_root.join(MANAGED_CODEX_SOURCE_FILE);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tokio::fs::write(&path, format!("{}\n", source.as_str()))
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

async fn resolve_managed_codex_binary_path(
    repo_root: &Path,
    source: ManagedCodexSourcePreference,
) -> Result<Option<std::path::PathBuf>> {
    match source {
        ManagedCodexSourcePreference::Source => {
            Ok(Some(repo_root.join(MANAGED_CODEX_CACHE_BINARY)))
        }
        ManagedCodexSourcePreference::Brew => Ok(resolve_codex_from_path().await.ok()),
    }
}

async fn read_codex_version(binary_path: &Path) -> Result<String> {
    let output = Command::new(binary_path)
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("failed to run {} --version", binary_path.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let version = if !stdout.is_empty() { stdout } else { stderr };
    if version.is_empty() {
        return Err(anyhow!(
            "{} --version returned empty output",
            binary_path.display()
        ));
    }
    Ok(version)
}

async fn resolve_codex_from_path() -> Result<std::path::PathBuf> {
    let output = Command::new("/bin/sh")
        .arg("-lc")
        .arg("command -v codex 2>/dev/null || true")
        .output()
        .await
        .context("failed to resolve codex from PATH")?;
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if resolved.is_empty() {
        return Err(anyhow!("could not find `codex` on PATH"));
    }
    Ok(Path::new(&resolved).to_path_buf())
}

async fn read_managed_codex_build_info(path: &Path) -> Result<Option<ManagedCodexBuildInfoView>> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(Some(ManagedCodexBuildInfoView {
        source_repo: values.get("source_repo").cloned(),
        source_rs_dir: values.get("source_rs_dir").cloned(),
        build_profile: values.get("build_profile").cloned(),
        git_rev: values.get("git_rev").cloned(),
        binary: values.get("binary").cloned(),
    }))
}

async fn resolve_git_rev(repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()
        .await
        .with_context(|| format!("failed to read git revision from {}", repo_root.display()))?;
    if !output.status.success() {
        return Err(anyhow!("git rev-parse failed for {}", repo_root.display()));
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if rev.is_empty() {
        return Err(anyhow!("empty git revision for {}", repo_root.display()));
    }
    Ok(rev)
}

#[derive(Debug)]
struct ManagementApiError {
    status: StatusCode,
    error: anyhow::Error,
}

impl From<anyhow::Error> for ManagementApiError {
    fn from(value: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: value,
        }
    }
}

impl ManagementApiError {
    fn not_found(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error,
        }
    }
}

impl IntoResponse for ManagementApiError {
    fn into_response(self) -> axum::response::Response {
        let message = serde_json::json!({
            "error": self.error.to_string(),
        });
        (self.status, Json(message)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkingSessionRecordView, WorkingSessionSummaryView, spawn_management_api,
    };
    use crate::config::RuntimeConfig;
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::repository::{
        ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
        TranscriptMirrorRole,
    };
    use crate::runtime_protocol::WorkingSessionRecordKind;
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-management-api-test-{}", Uuid::new_v4()))
    }

    fn runtime_config(root: &PathBuf) -> RuntimeConfig {
        RuntimeConfig {
            data_root_path: root.join("data"),
            debug_log_path: root.join("debug.log"),
            codex_working_directory: root.join("codex"),
            codex_model: None,
            management_bind_addr: "127.0.0.1:0".parse().unwrap(),
        }
    }

    fn full_auto_snapshot() -> SessionExecutionSnapshot {
        SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto)
    }

    #[tokio::test]
    async fn session_routes_return_summaries_and_records() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        let repo: ThreadRepository = handle.state.repository.clone();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let record = repo.create_thread(1, 7, "Workspace".to_owned()).await.unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        for entry in [
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:00.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "hello".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:01.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "done".to_owned(),
            },
        ] {
            repo.append_transcript_mirror(&record, &entry).await.unwrap();
        }

        let client = reqwest::Client::new();
        let summaries = client
            .get(format!(
                "{}/api/threads/{}/sessions",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert!(summaries.status().is_success());
        let summaries: Vec<WorkingSessionSummaryView> = summaries.json().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, "thr_current");

        let records = client
            .get(format!(
                "{}/api/threads/{}/sessions/thr_current/records",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert!(records.status().is_success());
        let records: Vec<WorkingSessionRecordView> = records.json().await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, WorkingSessionRecordKind::UserPrompt);
        assert_eq!(records[1].kind, WorkingSessionRecordKind::AssistantFinal);

        let missing = client
            .get(format!(
                "{}/api/threads/{}/sessions/missing/records",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    }
}
