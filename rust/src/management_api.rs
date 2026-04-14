use std::collections::BTreeMap;
use std::convert::Infallible;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
use serde_json::Value;
#[cfg(not(test))]
use teloxide::Bot;
#[cfg(not(test))]
use teloxide::requests::Requester;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::RwLock;
#[cfg(not(test))]
use tokio::time::{Duration as TokioDuration, timeout};
use tracing::{info, warn};

use crate::approval::{
    PendingApprovalView, PermissionGrantScope, PermissionProfile, SubmitPermissionsSubsetRequest,
};
use crate::config::{RuntimeConfig, load_optional_telegram_config_from_path};
use crate::launch_at_login::{self, LaunchAtLoginView};
use crate::local_control::{
    TelegramControlBridgeHandle, ensure_archived_thread, resolve_workspace_argument,
};
use crate::repository::{ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry};
use crate::runtime_control::{
    HcodexLaunchConfigView, SharedControlHandle, WorkspaceExecutionModeView,
    preflight_workspace_add, reset_workspace_runtime_surface, workspace_thread_title,
};
use crate::runtime_owner::{DesktopRuntimeOwner, RuntimeOwnerStatus};
pub use crate::runtime_protocol::{
    ArchivedThreadView, LaunchLocalSessionTarget, ManagedCodexBuildDefaultsView,
    ManagedCodexBuildInfoView, ManagedCodexView, ManagedWorkspaceView, MirrorPreviewDebugEventView,
    RuntimeControlAction, RuntimeControlActionEnvelope, RuntimeControlActionRequest,
    RuntimeControlActionResult, RuntimeEvent, RuntimeEventKind, RuntimeEventOperation,
    RuntimeHealthView, ThreadStateView, WorkingSessionRecordView, WorkingSessionSummaryView,
};
use crate::runtime_protocol::{
    build_archived_thread_views, build_runtime_health, build_thread_views,
    build_working_session_mirror_debug_events, build_working_session_records,
    build_working_session_summaries, build_workspace_views,
};
use crate::telemetry::{
    RuntimeTelemetryFields, RuntimeTelemetryHandle, RuntimeTelemetryMetrics,
    RuntimeTelemetrySnapshot,
};
use crate::workspace::{
    WorkspaceRuntimeEnsureMode, ensure_workspace_runtime_with_mode_and_telemetry,
    validate_seed_template,
};

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

#[derive(Debug, Clone)]
pub struct ManagementRuntimeOverview {
    pub health: RuntimeHealthView,
    pub workspaces: Vec<ManagedWorkspaceView>,
}

impl ManagementApiHandle {
    pub async fn set_telegram_polling_state(&self, state: TelegramPollingState) {
        let mut current = self.state.telegram_polling_state.write().await;
        *current = state;
    }

    pub async fn set_shared_control(&self, control: Option<SharedControlHandle>) {
        let mut current = self.state.shared_control.write().await;
        *current = control;
    }

    pub async fn set_telegram_bridge(&self, bridge: Option<TelegramControlBridgeHandle>) {
        let mut current = self.state.telegram_bridge.write().await;
        *current = bridge;
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

    pub async fn current_telegram_polling_state(&self) -> TelegramPollingState {
        self.state.current_telegram_polling_state().await
    }

    pub async fn runtime_health(&self) -> Result<RuntimeHealthView> {
        self.state.runtime_health().await
    }

    pub async fn workspace_views(&self) -> Result<Vec<ManagedWorkspaceView>> {
        self.state.workspace_views().await
    }

    pub async fn runtime_overview(&self) -> Result<ManagementRuntimeOverview> {
        self.state.runtime_overview().await
    }

    pub fn runtime_telemetry_handle(&self) -> RuntimeTelemetryHandle {
        self.state.runtime_telemetry.clone()
    }

    pub fn runtime_telemetry_snapshot(&self, limit: usize) -> RuntimeTelemetrySnapshot {
        self.state.runtime_telemetry.snapshot(limit)
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

    pub async fn working_session_mirror_debug_events(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<Vec<MirrorPreviewDebugEventView>> {
        self.state
            .working_session_mirror_debug_events(thread_key, session_id)
            .await
            .map_err(|error| error.error)
    }

    pub async fn workspace_execution_mode(
        &self,
        thread_key: &str,
    ) -> Result<WorkspaceExecutionModeView> {
        self.state.workspace_execution_mode_view(thread_key).await
    }

    pub async fn run_runtime_control_action(
        &self,
        thread_key: &str,
        request: RuntimeControlActionRequest,
    ) -> Result<RuntimeControlActionEnvelope> {
        self.state
            .run_runtime_control_action(thread_key, request)
            .await
    }

    pub async fn add_workspace(&self, workspace_cwd: &str) -> Result<AddWorkspaceResult> {
        self.state.add_workspace(workspace_cwd).await
    }

    pub async fn purge_all_archived_threads(&self) -> Result<usize> {
        self.state.purge_all_archived_threads().await
    }
}

#[derive(Clone)]
struct ManagementApiState {
    runtime: RuntimeConfig,
    repository: ThreadRepository,
    telegram_polling_state: Arc<RwLock<TelegramPollingState>>,
    shared_control: Arc<RwLock<Option<SharedControlHandle>>>,
    telegram_bridge: Arc<RwLock<Option<TelegramControlBridgeHandle>>>,
    runtime_owner: Arc<RwLock<Option<DesktopRuntimeOwner>>>,
    native_workspace_picker_available: Arc<RwLock<bool>>,
    bot_identity: Arc<RwLock<CachedTelegramBotIdentity>>,
    managed_codex_version_cache: Arc<RwLock<Option<ManagedCodexVersionCacheEntry>>>,
    runtime_telemetry: RuntimeTelemetryHandle,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStateView {
    pub first_run: bool,
    pub telegram_token_configured: bool,
    pub authorized_user_ids: Vec<i64>,
    pub authorized_user_count: usize,
    pub telegram_polling_state: TelegramPollingState,
    pub management_base_url: String,
    pub restart_required_after_setup_save: bool,
    pub control_chat_ready: bool,
    pub control_chat_id: Option<i64>,
    pub native_workspace_picker_available: bool,
    pub launch_at_login: LaunchAtLoginView,
    pub bot_username: Option<String>,
    pub bot_url: Option<String>,
    pub bot_identity_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CachedTelegramBotIdentity {
    token: Option<String>,
    username: Option<String>,
    url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct ManagedCodexVersionCacheEntry {
    binary_path: String,
    modified_at: Option<SystemTime>,
    expires_at: Instant,
    version: Option<String>,
}

fn managed_codex_version_cache_hit(
    entry: &ManagedCodexVersionCacheEntry,
    binary_path: &str,
    modified_at: Option<SystemTime>,
    now: Instant,
) -> bool {
    entry.binary_path == binary_path && entry.modified_at == modified_at && entry.expires_at > now
}

#[derive(Debug, Serialize)]
struct ArchiveThreadResponse {
    archived: bool,
    thread_key: String,
}

#[derive(Debug, Serialize)]
struct PurgeArchivedThreadsResponse {
    purged: usize,
}

#[derive(Debug, Serialize)]
pub struct ThreadMutationResponse {
    pub ok: bool,
    pub thread_key: String,
    pub action: RuntimeControlAction,
}

#[derive(Debug, Serialize)]
struct OpenWorkspaceResponse {
    opened: bool,
    thread_key: String,
    workspace_cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "decision_type", rename_all = "snake_case")]
enum SubmitApprovalDecisionRequest {
    Preset { token: String },
    PermissionsSubset {
        permissions: PermissionProfile,
        #[serde(default)]
        scope: PermissionGrantScope,
    },
}

#[derive(Debug, Serialize)]
struct SubmitApprovalDecisionResponse {
    ok: bool,
    approval_key: String,
}

#[derive(Debug, Serialize)]
struct PickAndAddWorkspaceResponse {
    ok: bool,
    created: bool,
    cancelled: bool,
    thread_key: Option<String>,
    title: Option<String>,
    workspace_cwd: Option<String>,
    probe_report: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TranscriptQuery {
    #[serde(default)]
    delivery: Option<TranscriptMirrorDelivery>,
    #[serde(default = "default_transcript_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct RuntimeTelemetryQuery {
    #[serde(default = "default_runtime_telemetry_limit")]
    limit: usize,
}

#[derive(Debug, Clone)]
pub struct AddWorkspaceResult {
    pub created: bool,
    pub thread_key: String,
    pub title: Option<String>,
    pub workspace_cwd: Option<String>,
    pub probe_report: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateTelegramSetupRequest {
    telegram_token: String,
    authorized_user_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct UpdateLaunchAtLoginRequest {
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct UpdateTelegramSetupResponse {
    saved: bool,
    restart_required: bool,
}

#[derive(Debug, Serialize)]
struct UpdateLaunchAtLoginResponse {
    saved: bool,
    launch_at_login: LaunchAtLoginView,
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

fn default_runtime_telemetry_limit() -> usize {
    200
}

fn bool_metric(value: bool) -> u64 {
    u64::from(value)
}

pub async fn spawn_management_api(runtime: RuntimeConfig) -> Result<ManagementApiHandle> {
    let repository = ThreadRepository::open(&runtime.data_root_path).await?;
    let runtime_telemetry = RuntimeTelemetryHandle::new(runtime.runtime_telemetry_path());
    let state = Arc::new(ManagementApiState {
        runtime: runtime.clone(),
        repository,
        telegram_polling_state: Arc::new(RwLock::new(TelegramPollingState::Disconnected)),
        shared_control: Arc::new(RwLock::new(None)),
        telegram_bridge: Arc::new(RwLock::new(None)),
        runtime_owner: Arc::new(RwLock::new(None)),
        native_workspace_picker_available: Arc::new(RwLock::new(false)),
        bot_identity: Arc::new(RwLock::new(CachedTelegramBotIdentity::default())),
        managed_codex_version_cache: Arc::new(RwLock::new(None)),
        runtime_telemetry,
    });
    state.refresh_cached_bot_identity().await?;
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
            "/api/setup/launch-at-login",
            post(post_update_launch_at_login),
        )
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
        .route("/api/runtime-telemetry", get(get_runtime_telemetry))
        .route(
            "/api/runtime-owner/reconcile",
            post(post_reconcile_runtime_owner),
        )
        .route("/api/threads", get(get_threads))
        .route(
            "/api/threads/:thread_key/transcript",
            get(get_thread_transcript),
        )
        .route(
            "/api/threads/:thread_key/sessions",
            get(get_working_sessions),
        )
        .route(
            "/api/threads/:thread_key/sessions/:session_id/records",
            get(get_working_session_records),
        )
        .route(
            "/api/threads/:thread_key/sessions/:session_id/mirror-preview-events",
            get(get_working_session_mirror_debug_events),
        )
        .route("/api/workspaces", get(get_workspaces))
        .route(
            "/api/workspaces/pick-and-add",
            post(post_pick_and_add_workspace),
        )
        .route("/api/archived-threads", get(get_archived_threads))
        .route(
            "/api/archived-threads/purge",
            post(post_purge_archived_threads),
        )
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
            get(get_workspace_execution_mode),
        )
        .route(
            "/api/threads/:thread_key/actions",
            post(post_runtime_control_action),
        )
        .route(
            "/api/workspaces/:thread_key/open",
            post(post_open_workspace),
        )
        .route("/api/pending-approvals", get(get_pending_approvals))
        .route(
            "/api/pending-approvals/:approval_key/respond",
            post(post_pending_approval_response),
        )
        .route(
            "/api/workspaces/:thread_key/repair-runtime",
            post(post_repair_workspace_runtime),
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

async fn post_update_launch_at_login(
    State(state): State<Arc<ManagementApiState>>,
    Json(payload): Json<UpdateLaunchAtLoginRequest>,
) -> Result<Json<UpdateLaunchAtLoginResponse>, ManagementApiError> {
    Ok(Json(UpdateLaunchAtLoginResponse {
        saved: true,
        launch_at_login: state.set_launch_at_login_enabled(payload.enabled).await?,
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

async fn get_runtime_telemetry(
    State(state): State<Arc<ManagementApiState>>,
    Query(query): Query<RuntimeTelemetryQuery>,
) -> Result<Json<RuntimeTelemetrySnapshot>, ManagementApiError> {
    Ok(Json(state.runtime_telemetry.snapshot(query.limit)))
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

async fn get_working_session_mirror_debug_events(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath((thread_key, session_id)): AxumPath<(String, String)>,
) -> Result<Json<Vec<MirrorPreviewDebugEventView>>, ManagementApiError> {
    Ok(Json(
        state
            .working_session_mirror_debug_events(&thread_key, &session_id)
            .await?,
    ))
}

async fn get_workspaces(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<ManagedWorkspaceView>>, ManagementApiError> {
    Ok(Json(state.workspace_views().await?))
}

async fn get_pending_approvals(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<Vec<PendingApprovalView>>, ManagementApiError> {
    Ok(Json(state.pending_approval_views().await?))
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

async fn post_purge_archived_threads(
    State(state): State<Arc<ManagementApiState>>,
) -> Result<Json<PurgeArchivedThreadsResponse>, ManagementApiError> {
    Ok(Json(PurgeArchivedThreadsResponse {
        purged: state.purge_all_archived_threads().await?,
    }))
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

async fn post_runtime_control_action(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(thread_key): AxumPath<String>,
    Json(payload): Json<RuntimeControlActionRequest>,
) -> Result<Json<RuntimeControlActionEnvelope>, ManagementApiError> {
    Ok(Json(
        state
            .run_runtime_control_action(&thread_key, payload)
            .await?,
    ))
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

async fn post_pending_approval_response(
    State(state): State<Arc<ManagementApiState>>,
    AxumPath(approval_key): AxumPath<String>,
    Json(payload): Json<SubmitApprovalDecisionRequest>,
) -> Result<Json<SubmitApprovalDecisionResponse>, ManagementApiError> {
    Ok(Json(
        state
            .submit_pending_approval_response(&approval_key, payload)
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

#[derive(Debug, Clone)]
struct ManagementEventSnapshot {
    setup: Value,
    runtime: Value,
    threads: BTreeMap<String, Value>,
    workspaces: BTreeMap<String, Value>,
    pending_approvals: BTreeMap<String, Value>,
    archived_threads: BTreeMap<String, Value>,
    working_sessions: BTreeMap<String, Value>,
    transcripts: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkingSessionsEventView {
    thread_key: String,
    updated_at: Option<String>,
    session_count: usize,
    run_status: String,
    run_phase: String,
}

#[derive(Debug, Clone, Serialize)]
struct TranscriptEventView {
    thread_key: String,
    updated_at: Option<String>,
    entry_count: usize,
}

async fn build_management_event_snapshot(
    state: &ManagementApiState,
) -> Result<ManagementEventSnapshot, ManagementApiError> {
    let setup = serde_json::to_value(state.setup_state().await?).map_err(anyhow::Error::from)?;
    let runtime_overview = state.runtime_overview().await?;
    let runtime = serde_json::to_value(runtime_overview.health).map_err(anyhow::Error::from)?;
    let threads = keyed_json_map(state.thread_views().await?, |thread| {
        thread.thread_key.clone()
    })
    .map_err(anyhow::Error::from)?;
    let workspaces = keyed_json_map(runtime_overview.workspaces, |workspace| {
        workspace.workspace_cwd.clone()
    })
    .map_err(anyhow::Error::from)?;
    let pending_approvals = keyed_json_map(state.pending_approval_views().await?, |item| {
        item.approval_key.clone()
    })
    .map_err(anyhow::Error::from)?;
    let archived_threads = keyed_json_map(state.archived_thread_views().await?, |thread| {
        thread.thread_key.clone()
    })
    .map_err(anyhow::Error::from)?;
    let working_sessions = keyed_json_map(state.working_sessions_event_views().await?, |item| {
        item.thread_key.clone()
    })
    .map_err(anyhow::Error::from)?;
    let transcripts = keyed_json_map(state.transcript_event_views().await?, |item| {
        item.thread_key.clone()
    })
    .map_err(anyhow::Error::from)?;
    Ok(ManagementEventSnapshot {
        setup,
        runtime,
        threads,
        workspaces,
        pending_approvals,
        archived_threads,
        working_sessions,
        transcripts,
    })
}

fn keyed_json_map<T, F>(
    items: Vec<T>,
    key_fn: F,
) -> Result<BTreeMap<String, Value>, serde_json::Error>
where
    T: Serialize,
    F: Fn(&T) -> String,
{
    let mut map = BTreeMap::new();
    for item in items {
        map.insert(key_fn(&item), serde_json::to_value(item)?);
    }
    Ok(map)
}

fn diff_management_event_snapshots(
    previous: Option<&ManagementEventSnapshot>,
    current: &ManagementEventSnapshot,
) -> Vec<RuntimeEvent> {
    let mut events = Vec::new();

    if previous.is_none_or(|snapshot| snapshot.setup != current.setup) {
        events.push(RuntimeEvent {
            kind: RuntimeEventKind::SetupChanged,
            op: RuntimeEventOperation::Upsert,
            key: None,
            current: Some(current.setup.clone()),
            message: None,
        });
    }

    if previous.is_none_or(|snapshot| snapshot.runtime != current.runtime) {
        events.push(RuntimeEvent {
            kind: RuntimeEventKind::RuntimeHealthChanged,
            op: RuntimeEventOperation::Upsert,
            key: None,
            current: Some(current.runtime.clone()),
            message: None,
        });
    }
    push_managed_codex_runtime_event(&mut events, previous, current);

    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::ThreadStateChanged,
        previous.map(|snapshot| &snapshot.threads),
        &current.threads,
    );
    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::WorkspaceStateChanged,
        previous.map(|snapshot| &snapshot.workspaces),
        &current.workspaces,
    );
    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::PendingApprovalChanged,
        previous.map(|snapshot| &snapshot.pending_approvals),
        &current.pending_approvals,
    );
    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::ArchivedThreadChanged,
        previous.map(|snapshot| &snapshot.archived_threads),
        &current.archived_threads,
    );
    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::WorkingSessionChanged,
        previous.map(|snapshot| &snapshot.working_sessions),
        &current.working_sessions,
    );
    push_keyed_runtime_events(
        &mut events,
        RuntimeEventKind::TranscriptChanged,
        previous.map(|snapshot| &snapshot.transcripts),
        &current.transcripts,
    );

    events
}

fn push_managed_codex_runtime_event(
    target: &mut Vec<RuntimeEvent>,
    previous: Option<&ManagementEventSnapshot>,
    current: &ManagementEventSnapshot,
) {
    let previous_managed_codex =
        previous.and_then(|snapshot| managed_codex_view(&snapshot.runtime));
    let current_managed_codex = managed_codex_view(&current.runtime);
    if previous_managed_codex == current_managed_codex {
        return;
    }
    let (op, current_payload) = match current_managed_codex {
        Some(value) => (RuntimeEventOperation::Upsert, Some(value)),
        None => (RuntimeEventOperation::Remove, None),
    };
    target.push(RuntimeEvent {
        kind: RuntimeEventKind::ManagedCodexChanged,
        op,
        key: None,
        current: current_payload,
        message: None,
    });
}

fn managed_codex_view(runtime: &Value) -> Option<Value> {
    runtime.get("managed_codex").cloned()
}

fn push_keyed_runtime_events(
    target: &mut Vec<RuntimeEvent>,
    kind: RuntimeEventKind,
    previous: Option<&BTreeMap<String, Value>>,
    current: &BTreeMap<String, Value>,
) {
    let empty = BTreeMap::new();
    let previous = previous.unwrap_or(&empty);

    for (key, value) in current {
        if previous.get(key) != Some(value) {
            target.push(RuntimeEvent {
                kind,
                op: RuntimeEventOperation::Upsert,
                key: Some(key.clone()),
                current: Some(value.clone()),
                message: None,
            });
        }
    }

    for key in previous.keys() {
        if !current.contains_key(key) {
            target.push(RuntimeEvent {
                kind,
                op: RuntimeEventOperation::Remove,
                key: Some(key.clone()),
                current: None,
                message: None,
            });
        }
    }
}

async fn get_events(
    State(state): State<Arc<ManagementApiState>>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = stream! {
        let mut previous_snapshot: Option<ManagementEventSnapshot> = None;
        loop {
            match build_management_event_snapshot(&state).await {
                Ok(snapshot) => {
                    let events = diff_management_event_snapshots(previous_snapshot.as_ref(), &snapshot);
                    previous_snapshot = Some(snapshot);
                    for payload in events {
                        if let Ok(data) = serde_json::to_string(&payload) {
                            yield Ok(
                                Event::default()
                                    .event(payload.kind.as_str())
                                    .data(data)
                            );
                        }
                    }
                }
                Err(error) => {
                    let payload = RuntimeEvent {
                        kind: RuntimeEventKind::Error,
                        op: RuntimeEventOperation::Upsert,
                        key: None,
                        current: None,
                        message: Some(error.error.to_string()),
                    };
                    if let Ok(data) = serde_json::to_string(&payload) {
                        yield Ok(
                            Event::default()
                                .event(payload.kind.as_str())
                                .data(data)
                        );
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(5)))
}

impl ManagementApiState {
    async fn current_telegram_polling_state(&self) -> TelegramPollingState {
        *self.telegram_polling_state.read().await
    }

    async fn invalidate_managed_codex_version_cache(&self) {
        let mut cache = self.managed_codex_version_cache.write().await;
        *cache = None;
    }

    async fn cached_managed_codex_version(
        &self,
        binary_path: Option<&Path>,
    ) -> (Option<String>, bool) {
        let Some(binary_path) = binary_path.filter(|path| path.exists()) else {
            return (None, false);
        };
        let metadata = tokio::fs::metadata(binary_path).await.ok();
        let modified_at = metadata.as_ref().and_then(|value| value.modified().ok());
        let binary_path_string = binary_path.display().to_string();
        let now = Instant::now();

        {
            let cache = self.managed_codex_version_cache.read().await;
            if let Some(entry) = cache.as_ref()
                && managed_codex_version_cache_hit(entry, &binary_path_string, modified_at, now)
            {
                return (entry.version.clone(), true);
            }
        }

        let version = read_codex_version(binary_path).await.ok();
        let mut cache = self.managed_codex_version_cache.write().await;
        *cache = Some(ManagedCodexVersionCacheEntry {
            binary_path: binary_path_string,
            modified_at,
            expires_at: Instant::now() + Duration::from_secs(300),
            version: version.clone(),
        });
        (version, false)
    }

    async fn refresh_cached_bot_identity(&self) -> Result<()> {
        let telegram = load_optional_telegram_config_from_path(&self.runtime.config_env_path())?;
        self.refresh_cached_bot_identity_for_token(
            telegram
                .as_ref()
                .map(|config| config.telegram_token.as_str()),
        )
        .await;
        Ok(())
    }

    async fn refresh_cached_bot_identity_for_token(&self, token: Option<&str>) {
        let started_at = Instant::now();
        let next = resolve_cached_bot_identity(token).await;
        {
            let mut current = self.bot_identity.write().await;
            *current = next.clone();
        }
        let mut fields = RuntimeTelemetryFields::new();
        fields.insert(
            "token_present".to_owned(),
            token
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
                .to_string(),
        );
        let mut metrics = RuntimeTelemetryMetrics::new();
        metrics.insert(
            "username_present".to_owned(),
            bool_metric(next.username.is_some()),
        );
        metrics.insert("url_present".to_owned(), bool_metric(next.url.is_some()));
        metrics.insert(
            "error_present".to_owned(),
            bool_metric(next.error.is_some()),
        );
        self.runtime_telemetry.record_duration(
            "management.bot_identity_refresh",
            started_at,
            if next.error.is_some() { "error" } else { "ok" },
            fields,
            metrics,
            next.error.clone(),
        );
    }

    async fn ensure_cached_bot_identity(&self, token: Option<&str>) -> CachedTelegramBotIdentity {
        let started_at = Instant::now();
        let needs_refresh = {
            let current = self.bot_identity.read().await;
            current.token.as_deref() != token
        };
        if needs_refresh {
            self.refresh_cached_bot_identity_for_token(token).await;
        }
        let current = self.bot_identity.read().await.clone();
        let mut fields = RuntimeTelemetryFields::new();
        fields.insert(
            "token_present".to_owned(),
            token
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
                .to_string(),
        );
        fields.insert("refreshed".to_owned(), needs_refresh.to_string());
        let mut metrics = RuntimeTelemetryMetrics::new();
        metrics.insert(
            "username_present".to_owned(),
            bool_metric(current.username.is_some()),
        );
        metrics.insert("url_present".to_owned(), bool_metric(current.url.is_some()));
        metrics.insert(
            "error_present".to_owned(),
            bool_metric(current.error.is_some()),
        );
        self.runtime_telemetry.record_duration(
            "management.bot_identity_lookup",
            started_at,
            if current.error.is_some() {
                "error"
            } else {
                "ok"
            },
            fields,
            metrics,
            current.error.clone(),
        );
        current
    }

    async fn shared_control(&self) -> Result<SharedControlHandle> {
        self.shared_control.read().await.clone().context(
            "Desktop runtime owner control is not active. Start threadBridge in desktop mode.",
        )
    }

    async fn shared_control_optional(&self) -> Option<SharedControlHandle> {
        self.shared_control.read().await.clone()
    }

    async fn telegram_bridge(&self) -> Result<TelegramControlBridgeHandle> {
        self.telegram_bridge.read().await.clone().context(
            "Telegram bot runtime is unavailable. Reconnect Telegram polling before running this action.",
        )
    }

    async fn restart_required_after_setup_save(&self) -> bool {
        self.runtime_owner.read().await.is_none()
    }

    async fn pending_approval_views(&self) -> Result<Vec<PendingApprovalView>> {
        let Some(control) = self.shared_control_optional().await else {
            return Ok(Vec::new());
        };
        Ok(control.list_pending_approvals().await)
    }

    async fn submit_pending_approval_response(
        &self,
        approval_key: &str,
        request: SubmitApprovalDecisionRequest,
    ) -> Result<SubmitApprovalDecisionResponse> {
        let control = self.shared_control().await?;
        let resolution = match request {
            SubmitApprovalDecisionRequest::Preset { token } => {
                control
                    .resolve_pending_approval_preset(approval_key, &token)
                    .await?
            }
            SubmitApprovalDecisionRequest::PermissionsSubset { permissions, scope } => {
                control
                    .resolve_pending_permission_subset(
                        approval_key,
                        SubmitPermissionsSubsetRequest { permissions, scope },
                    )
                    .await?
            }
        }
        .context("approval is no longer pending")?;
        control.forward_approval_resolution(&resolution).await?;
        Ok(SubmitApprovalDecisionResponse {
            ok: true,
            approval_key: approval_key.to_owned(),
        })
    }

    async fn setup_state(&self) -> Result<SetupStateView> {
        let setup_started_at = Instant::now();
        let config_env_path = self.runtime.config_env_path();

        let telegram_started_at = Instant::now();
        let telegram = match load_optional_telegram_config_from_path(&config_env_path) {
            Ok(telegram) => {
                let mut metrics = RuntimeTelemetryMetrics::new();
                metrics.insert(
                    "telegram_token_configured".to_owned(),
                    bool_metric(telegram.is_some()),
                );
                metrics.insert(
                    "authorized_user_count".to_owned(),
                    telegram
                        .as_ref()
                        .map(|config| config.authorized_user_ids.len() as u64)
                        .unwrap_or_default(),
                );
                self.runtime_telemetry.record_duration(
                    "management.setup_state.telegram_config",
                    telegram_started_at,
                    "ok",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
                telegram
            }
            Err(error) => {
                self.runtime_telemetry.record_duration(
                    "management.setup_state.telegram_config",
                    telegram_started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    RuntimeTelemetryMetrics::new(),
                    Some(error.to_string()),
                );
                self.runtime_telemetry.record_duration(
                    "management.setup_state",
                    setup_started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    RuntimeTelemetryMetrics::new(),
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };

        let main_thread_started_at = Instant::now();
        let main_thread = match self.repository.find_main_thread().await {
            Ok(main_thread) => {
                let mut metrics = RuntimeTelemetryMetrics::new();
                metrics.insert(
                    "control_chat_ready".to_owned(),
                    bool_metric(main_thread.is_some()),
                );
                self.runtime_telemetry.record_duration(
                    "management.setup_state.main_thread",
                    main_thread_started_at,
                    "ok",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
                main_thread
            }
            Err(error) => {
                self.runtime_telemetry.record_duration(
                    "management.setup_state.main_thread",
                    main_thread_started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    RuntimeTelemetryMetrics::new(),
                    Some(error.to_string()),
                );
                self.runtime_telemetry.record_duration(
                    "management.setup_state",
                    setup_started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    RuntimeTelemetryMetrics::new(),
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };

        let bot_identity_started_at = Instant::now();
        let bot_identity = self
            .ensure_cached_bot_identity(
                telegram
                    .as_ref()
                    .map(|config| config.telegram_token.as_str()),
            )
            .await;
        let mut bot_identity_metrics = RuntimeTelemetryMetrics::new();
        bot_identity_metrics.insert(
            "username_present".to_owned(),
            bool_metric(bot_identity.username.is_some()),
        );
        bot_identity_metrics.insert(
            "url_present".to_owned(),
            bool_metric(bot_identity.url.is_some()),
        );
        bot_identity_metrics.insert(
            "error_present".to_owned(),
            bool_metric(bot_identity.error.is_some()),
        );
        self.runtime_telemetry.record_duration(
            "management.setup_state.bot_identity",
            bot_identity_started_at,
            if bot_identity.error.is_some() {
                "error"
            } else {
                "ok"
            },
            RuntimeTelemetryFields::new(),
            bot_identity_metrics,
            bot_identity.error.clone(),
        );

        let launch_at_login_started_at = Instant::now();
        let launch_at_login = launch_at_login::current_view(&self.runtime);
        let mut launch_metrics = RuntimeTelemetryMetrics::new();
        launch_metrics.insert(
            "supported".to_owned(),
            bool_metric(launch_at_login.supported),
        );
        launch_metrics.insert("enabled".to_owned(), bool_metric(launch_at_login.enabled));
        self.runtime_telemetry.record_duration(
            "management.setup_state.launch_at_login",
            launch_at_login_started_at,
            "ok",
            RuntimeTelemetryFields::new(),
            launch_metrics,
            None,
        );

        let setup = SetupStateView {
            first_run: !config_env_path.exists(),
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
            launch_at_login,
            bot_username: bot_identity.username,
            bot_url: bot_identity.url,
            bot_identity_error: bot_identity.error,
        };

        let mut setup_metrics = RuntimeTelemetryMetrics::new();
        setup_metrics.insert(
            "telegram_token_configured".to_owned(),
            bool_metric(setup.telegram_token_configured),
        );
        setup_metrics.insert(
            "authorized_user_count".to_owned(),
            setup.authorized_user_count as u64,
        );
        setup_metrics.insert(
            "control_chat_ready".to_owned(),
            bool_metric(setup.control_chat_ready),
        );
        setup_metrics.insert(
            "native_workspace_picker_available".to_owned(),
            bool_metric(setup.native_workspace_picker_available),
        );
        self.runtime_telemetry.record_duration(
            "management.setup_state",
            setup_started_at,
            "ok",
            RuntimeTelemetryFields::new(),
            setup_metrics,
            None,
        );

        Ok(setup)
    }

    async fn set_launch_at_login_enabled(&self, enabled: bool) -> Result<LaunchAtLoginView> {
        launch_at_login::set_enabled(&self.runtime, enabled)
    }

    async fn runtime_health(&self) -> Result<RuntimeHealthView> {
        Ok(self.runtime_overview().await?.health)
    }

    async fn runtime_overview(&self) -> Result<ManagementRuntimeOverview> {
        let started_at = Instant::now();
        let result = async {
            let workspaces = self.workspace_views().await?;
            let runtime_owner = match self.runtime_owner.read().await.clone() {
                Some(owner) => owner.status().await,
                None => RuntimeOwnerStatus::inactive(),
            };
            let health = build_runtime_health(
                self.runtime.management_bind_addr.to_string(),
                &workspaces,
                runtime_owner,
                self.managed_codex_view().await?,
            );
            Ok::<ManagementRuntimeOverview, anyhow::Error>(ManagementRuntimeOverview {
                health,
                workspaces,
            })
        }
        .await;

        let mut metrics = RuntimeTelemetryMetrics::new();
        match &result {
            Ok(overview) => {
                metrics.insert(
                    "workspace_count".to_owned(),
                    overview.workspaces.len() as u64,
                );
                metrics.insert(
                    "running_workspaces".to_owned(),
                    overview.health.running_workspaces as u64,
                );
                metrics.insert(
                    "ready_workspaces".to_owned(),
                    overview.health.ready_workspaces as u64,
                );
                metrics.insert(
                    "broken_threads".to_owned(),
                    overview.health.broken_threads as u64,
                );
                self.runtime_telemetry.record_duration(
                    "management.runtime_overview",
                    started_at,
                    "ok",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
            }
            Err(error) => {
                self.runtime_telemetry.record_duration(
                    "management.runtime_overview",
                    started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    Some(error.to_string()),
                );
            }
        }

        result
    }

    async fn managed_codex_view(&self) -> Result<ManagedCodexView> {
        let started_at = Instant::now();
        let result = async {
            let data_root = &self.runtime.data_root_path;
            let source = read_managed_codex_source_preference(data_root).await?;
            let binary_path = resolve_managed_codex_binary_path(data_root, source).await?;
            let binary_ready = binary_path.as_ref().is_some_and(|path| path.exists());
            let build_config_path = data_root.join(MANAGED_CODEX_BUILD_CONFIG_FILE);
            let build_info_path = data_root.join(MANAGED_CODEX_BUILD_INFO_FILE);
            let build_defaults = resolve_managed_codex_build_defaults(data_root).await?;
            let (version, cache_hit) = self
                .cached_managed_codex_version(binary_path.as_deref())
                .await;
            Ok::<(ManagedCodexView, bool), anyhow::Error>((
                ManagedCodexView {
                    source: source.as_str(),
                    source_file_path: data_root
                        .join(MANAGED_CODEX_SOURCE_FILE)
                        .display()
                        .to_string(),
                    build_config_file_path: build_config_path.display().to_string(),
                    build_info_file_path: build_info_path.display().to_string(),
                    binary_path: binary_path
                        .unwrap_or_else(|| data_root.join(MANAGED_CODEX_CACHE_BINARY))
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
                },
                cache_hit,
            ))
        }
        .await;

        let mut fields = RuntimeTelemetryFields::new();
        let mut metrics = RuntimeTelemetryMetrics::new();
        match &result {
            Ok((view, cache_hit)) => {
                fields.insert("source".to_owned(), view.source.to_owned());
                fields.insert("cache_hit".to_owned(), cache_hit.to_string());
                metrics.insert("binary_ready".to_owned(), u64::from(view.binary_ready));
                metrics.insert(
                    "version_present".to_owned(),
                    u64::from(view.version.is_some()),
                );
                self.runtime_telemetry.record_duration(
                    "management.managed_codex_view",
                    started_at,
                    "ok",
                    fields,
                    metrics,
                    None,
                );
            }
            Err(error) => {
                self.runtime_telemetry.record_duration(
                    "management.managed_codex_view",
                    started_at,
                    "error",
                    fields,
                    metrics,
                    Some(error.to_string()),
                );
            }
        }

        result.map(|(view, _cache_hit)| view)
    }

    async fn update_managed_codex_preference(
        &self,
        source: &str,
    ) -> Result<UpdateManagedCodexPreferenceResponse> {
        let source = ManagedCodexSourcePreference::parse(source)?;
        write_managed_codex_source_preference(&self.runtime.data_root_path, source).await?;
        self.invalidate_managed_codex_version_cache().await;
        let seed_template_path = validate_seed_template(&self.runtime.runtime_template_path())?;
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
            ensure_workspace_runtime_with_mode_and_telemetry(
                &self.runtime.runtime_support_root_path,
                &self.runtime.data_root_path,
                &seed_template_path,
                Path::new(&workspace_cwd),
                WorkspaceRuntimeEnsureMode::ExplicitSync,
                Some(&self.runtime_telemetry),
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
        let dest_path = self.runtime.managed_codex_binary_path();
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
        self.invalidate_managed_codex_version_cache().await;
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
        let defaults = read_managed_codex_build_config(&self.runtime.data_root_path).await?;
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

        let managed_dir = self.runtime.managed_codex_root_path();
        tokio::fs::create_dir_all(&managed_dir)
            .await
            .with_context(|| format!("failed to create {}", managed_dir.display()))?;
        let dest_path = self.runtime.managed_codex_binary_path();
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
            .data_root_path
            .join(MANAGED_CODEX_BUILD_INFO_FILE);
        tokio::fs::write(&build_info_path, build_info)
            .await
            .with_context(|| format!("failed to write {}", build_info_path.display()))?;

        let version = read_codex_version(&dest_path).await.ok();
        self.invalidate_managed_codex_version_cache().await;
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
            &self.runtime.data_root_path,
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
        let started_at = Instant::now();
        let runtime_owner = self.runtime_owner.read().await.clone();
        let result = build_workspace_views(&self.repository, runtime_owner.as_ref()).await;

        let mut metrics = RuntimeTelemetryMetrics::new();
        match &result {
            Ok(workspaces) => {
                metrics.insert("workspace_count".to_owned(), workspaces.len() as u64);
                metrics.insert(
                    "conflicted_workspaces".to_owned(),
                    workspaces
                        .iter()
                        .filter(|workspace| workspace.conflict)
                        .count() as u64,
                );
                metrics.insert(
                    "running_workspaces".to_owned(),
                    workspaces
                        .iter()
                        .filter(|workspace| workspace.run_status == "running")
                        .count() as u64,
                );
                metrics.insert(
                    "degraded_workspaces".to_owned(),
                    workspaces
                        .iter()
                        .filter(|workspace| {
                            matches!(
                                workspace.runtime_readiness,
                                "degraded" | "pending_adoption" | "unavailable"
                            )
                        })
                        .count() as u64,
                );
                self.runtime_telemetry.record_duration(
                    "management.workspace_views",
                    started_at,
                    "ok",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
            }
            Err(error) => {
                self.runtime_telemetry.record_duration(
                    "management.workspace_views",
                    started_at,
                    "error",
                    RuntimeTelemetryFields::new(),
                    metrics,
                    Some(error.to_string()),
                );
            }
        }

        result
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

    async fn working_session_mirror_debug_events(
        &self,
        thread_key: &str,
        session_id: &str,
    ) -> Result<Vec<MirrorPreviewDebugEventView>, ManagementApiError> {
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
        build_working_session_mirror_debug_events(&self.repository, &record, &binding, session_id)
            .await?
            .ok_or_else(|| {
                ManagementApiError::not_found(anyhow!(
                    "session `{session_id}` not found for active thread `{thread_key}`"
                ))
            })
    }

    async fn working_sessions_event_views(&self) -> Result<Vec<WorkingSessionsEventView>> {
        let active_threads = self.repository.list_active_threads().await?;
        let mut views = Vec::new();
        for record in active_threads {
            let thread_key = record.metadata.thread_key.clone();
            let Some(binding) = self.repository.read_session_binding(&record).await? else {
                views.push(WorkingSessionsEventView {
                    thread_key,
                    updated_at: None,
                    session_count: 0,
                    run_status: "idle".to_owned(),
                    run_phase: "idle".to_owned(),
                });
                continue;
            };
            let summaries = if binding.workspace_cwd.is_some() {
                build_working_session_summaries(&self.repository, &record, &binding).await?
            } else {
                Vec::new()
            };
            let updated_at = summaries
                .iter()
                .map(|summary| &summary.updated_at)
                .max()
                .cloned();
            let latest_summary = summaries.iter().max_by(|a, b| {
                a.updated_at
                    .cmp(&b.updated_at)
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
            views.push(WorkingSessionsEventView {
                thread_key,
                updated_at,
                session_count: summaries.len(),
                run_status: latest_summary
                    .map(|summary| summary.run_status.clone())
                    .unwrap_or_else(|| "idle".to_owned()),
                run_phase: latest_summary
                    .map(|summary| summary.run_phase.clone())
                    .unwrap_or_else(|| "idle".to_owned()),
            });
        }
        views.sort_by(|a, b| a.thread_key.cmp(&b.thread_key));
        Ok(views)
    }

    async fn transcript_event_views(&self) -> Result<Vec<TranscriptEventView>> {
        let active_threads = self.repository.list_active_threads().await?;
        let mut views = Vec::new();
        for record in active_threads {
            let entries = self.repository.read_full_transcript_mirror(&record).await?;
            views.push(TranscriptEventView {
                thread_key: record.metadata.thread_key,
                updated_at: entries.last().map(|entry| entry.timestamp.clone()),
                entry_count: entries.len(),
            });
        }
        views.sort_by(|a, b| a.thread_key.cmp(&b.thread_key));
        Ok(views)
    }

    async fn archived_thread_views(&self) -> Result<Vec<ArchivedThreadView>> {
        build_archived_thread_views(&self.repository).await
    }

    async fn add_workspace(&self, workspace_cwd: &str) -> Result<AddWorkspaceResult> {
        let workspace_path = resolve_workspace_argument(workspace_cwd).await?;
        let preflight = preflight_workspace_add(&self.repository, &workspace_path).await?;
        if let Some(reason) = preflight.blocking_reason() {
            anyhow::bail!("{}\n\n{}", reason, preflight.render_text());
        }
        let control = self.shared_control().await?;
        let bridge = self.telegram_bridge().await?;
        let created = bridge
            .create_workspace_thread(
                Some(workspace_thread_title(&workspace_path)),
                "local management UI",
            )
            .await?;
        let record = control
            .create_thread(
                created.chat_id,
                created.message_thread_id,
                created.title.clone(),
            )
            .await?;
        let reset_performed = match reset_workspace_runtime_surface(&workspace_path).await {
            Ok(value) => value,
            Err(error) => {
                self.rollback_failed_workspace_add(&bridge, record, &workspace_path, &error)
                    .await;
                return Err(error);
            }
        };
        let bound = match control
            .bind_workspace_record(record.clone(), &workspace_path, "local management UI")
            .await
        {
            Ok(record) => record,
            Err(error) => {
                self.rollback_failed_workspace_add(&bridge, record, &workspace_path, &error)
                    .await;
                return Err(error);
            }
        };
        if let Err(error) = bridge
            .notify_workspace_bound(&bound, &workspace_path, "bind")
            .await
        {
            warn!(
                event = "management_api.add_workspace.notify_bound_failed",
                thread_key = %bound.metadata.thread_key,
                workspace = %workspace_path.display(),
                error = %error,
                "workspace was bound but Telegram notification failed"
            );
        }
        let mut report_lines = vec![preflight.render_text()];
        report_lines.push(format!(
            "reset_performed: {}",
            if reset_performed { "yes" } else { "no" }
        ));
        report_lines.push(format!("thread_key: {}", bound.metadata.thread_key));
        Ok(AddWorkspaceResult {
            created: true,
            thread_key: bound.metadata.thread_key.clone(),
            title: bound.metadata.title.clone(),
            workspace_cwd: Some(workspace_path.display().to_string()),
            probe_report: Some(report_lines.join("\n")),
        })
    }

    async fn purge_all_archived_threads(&self) -> Result<usize> {
        self.repository.purge_all_archived_threads().await
    }

    #[allow(dead_code)]
    async fn rollback_failed_workspace_add(
        &self,
        bridge: &TelegramControlBridgeHandle,
        record: crate::repository::ThreadRecord,
        workspace_path: &Path,
        error: &anyhow::Error,
    ) {
        warn!(
            event = "management_api.add_workspace.rollback_started",
            thread_key = %record.metadata.thread_key,
            workspace = %workspace_path.display(),
            error = %error,
            "rolling back failed workspace add after thread creation"
        );
        if let Err(delete_error) = bridge.delete_thread_topic(&record).await {
            warn!(
                event = "management_api.add_workspace.rollback_topic_delete_failed",
                thread_key = %record.metadata.thread_key,
                workspace = %workspace_path.display(),
                error = %delete_error,
                "failed to delete Telegram topic during workspace-add rollback"
            );
        }
        if let Err(archive_error) = self.repository.archive_thread(record.clone()).await {
            warn!(
                event = "management_api.add_workspace.rollback_archive_failed",
                thread_key = %record.metadata.thread_key,
                workspace = %workspace_path.display(),
                error = %archive_error,
                "failed to archive local thread during workspace-add rollback"
            );
        }
    }

    async fn pick_and_add_workspace(&self) -> Result<PickAndAddWorkspaceResponse> {
        anyhow::ensure!(
            *self.native_workspace_picker_available.read().await,
            "Native workspace picker is unavailable. Start threadBridge in desktop mode."
        );
        let Some(workspace_cwd) = pick_workspace_folder().await? else {
            return Ok(PickAndAddWorkspaceResponse {
                ok: true,
                created: false,
                cancelled: true,
                thread_key: None,
                title: None,
                workspace_cwd: None,
                probe_report: None,
            });
        };
        let result = self.add_workspace(&workspace_cwd).await?;
        Ok(PickAndAddWorkspaceResponse {
            ok: true,
            created: result.created,
            cancelled: false,
            thread_key: (!result.thread_key.is_empty()).then_some(result.thread_key),
            title: result.title,
            workspace_cwd: result.workspace_cwd,
            probe_report: result.probe_report,
        })
    }

    async fn adopt_tui_session(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.shared_control().await?;
        let record = control
            .adopt_tui_session(thread_key, "local management UI")
            .await?;
        if let Some(bridge) = self.telegram_bridge.read().await.clone() {
            let _ = bridge
                .refresh_thread_title(&record, "local_tui_adopt_accept")
                .await;
        }
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: record.metadata.thread_key,
            action: RuntimeControlAction::AdoptTuiSession,
        })
    }

    async fn reject_tui_session(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.shared_control().await?;
        let record = control
            .reject_tui_session(thread_key, "local management UI")
            .await?;
        if let Some(bridge) = self.telegram_bridge.read().await.clone() {
            let _ = bridge
                .refresh_thread_title(&record, "local_tui_adopt_reject")
                .await;
        }
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: record.metadata.thread_key,
            action: RuntimeControlAction::RejectTuiSession,
        })
    }

    async fn workspace_execution_mode_view(
        &self,
        thread_key: &str,
    ) -> Result<WorkspaceExecutionModeView> {
        self.shared_control()
            .await?
            .workspace_execution_mode_view(thread_key)
            .await
    }

    async fn workspace_launch_config(&self, thread_key: &str) -> Result<HcodexLaunchConfigView> {
        self.shared_control()
            .await?
            .workspace_launch_config(thread_key)
            .await
    }

    async fn run_runtime_control_action(
        &self,
        thread_key: &str,
        request: RuntimeControlActionRequest,
    ) -> Result<RuntimeControlActionEnvelope> {
        let control = self.shared_control().await?;
        let result = control
            .execute_runtime_control_action(thread_key, request, "local management UI")
            .await?;
        if let Some(bridge) = self.telegram_bridge.read().await.clone() {
            match &result.result {
                RuntimeControlActionResult::StartFreshSession { .. } => {
                    if let Ok(Some(record)) =
                        self.repository.find_active_thread_by_key(thread_key).await
                    {
                        let _ = bridge.refresh_thread_title(&record, "new").await;
                    }
                }
                RuntimeControlActionResult::RepairSessionBinding { verified, .. } => {
                    if let Ok(Some(record)) =
                        self.repository.find_active_thread_by_key(thread_key).await
                    {
                        let source = if *verified {
                            "reconnect_codex_verified"
                        } else {
                            "reconnect_codex_broken"
                        };
                        let _ = bridge.refresh_thread_title(&record, source).await;
                    }
                }
                RuntimeControlActionResult::SetThreadCollaborationMode { .. } => {
                    if let Ok(Some(record)) =
                        self.repository.find_active_thread_by_key(thread_key).await
                    {
                        let _ = bridge
                            .refresh_thread_title(&record, "collaboration_mode")
                            .await;
                    }
                }
                RuntimeControlActionResult::SetWorkspaceExecutionMode { .. }
                | RuntimeControlActionResult::LaunchLocalSession { .. }
                | RuntimeControlActionResult::InterruptRunningTurn { .. } => {}
            }
        }
        Ok(result)
    }

    async fn archive_thread(&self, thread_key: &str) -> Result<ArchiveThreadResponse> {
        let control = self.shared_control().await?;
        let archived = control
            .archive_thread(thread_key, "local management UI")
            .await?;
        if let Some(bridge) = self.telegram_bridge.read().await.clone() {
            let _ = bridge.delete_thread_topic(&archived).await;
        }
        Ok(ArchiveThreadResponse {
            archived: true,
            thread_key: archived.metadata.thread_key,
        })
    }

    async fn restore_thread(&self, thread_key: &str) -> Result<ThreadMutationResponse> {
        let control = self.shared_control().await?;
        let bridge = self.telegram_bridge().await?;
        let archived = ensure_archived_thread(
            control
                .archived_thread(bridge.control_chat_id().await?, thread_key)
                .await?,
        )?;
        let created = bridge.create_restored_thread(&archived).await?;
        let restored = control
            .restore_thread(
                archived,
                created.message_thread_id,
                created.title.clone(),
                "local management UI",
            )
            .await?;
        bridge.notify_restored(&restored, &created.title).await?;
        Ok(ThreadMutationResponse {
            ok: true,
            thread_key: restored.metadata.thread_key,
            action: RuntimeControlAction::RestoreThread,
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
            action: RuntimeControlAction::RepairWorkspaceRuntime,
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

    async fn write_telegram_setup(&self, payload: UpdateTelegramSetupRequest) -> Result<()> {
        let mut updates = BTreeMap::new();
        let telegram_token = payload.telegram_token.trim().to_owned();
        updates.insert("TELEGRAM_BOT_TOKEN".to_owned(), telegram_token.clone());
        let authorized = payload
            .authorized_user_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        updates.insert("AUTHORIZED_TELEGRAM_USER_IDS".to_owned(), authorized);
        let env_path = self.runtime.config_env_path();
        write_env_file(&env_path, &updates).await?;
        self.refresh_cached_bot_identity_for_token(Some(telegram_token.as_str()))
            .await;
        Ok(())
    }
}

async fn resolve_cached_bot_identity(token: Option<&str>) -> CachedTelegramBotIdentity {
    let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) else {
        return CachedTelegramBotIdentity::default();
    };
    match resolve_telegram_bot_identity(token).await {
        Ok((username, url)) => CachedTelegramBotIdentity {
            token: Some(token.to_owned()),
            username,
            url,
            error: None,
        },
        Err(error) => CachedTelegramBotIdentity {
            token: Some(token.to_owned()),
            username: None,
            url: None,
            error: Some(error.to_string()),
        },
    }
}

async fn resolve_telegram_bot_identity(token: &str) -> Result<(Option<String>, Option<String>)> {
    let username = fetch_telegram_bot_username(token).await?;
    let url = username
        .as_deref()
        .map(|value| format!("https://t.me/{value}"));
    Ok((username, url))
}

#[cfg(not(test))]
async fn fetch_telegram_bot_username(token: &str) -> Result<Option<String>> {
    let me = timeout(
        TokioDuration::from_secs(5),
        Bot::new(token.to_owned()).get_me(),
    )
    .await
    .context("Telegram getMe timed out")??;
    Ok(me.user.username.clone())
}

#[cfg(test)]
async fn fetch_telegram_bot_username(_token: &str) -> Result<Option<String>> {
    Ok(None)
}

async fn write_env_file(path: &Path, updates: &BTreeMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
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
        MANAGEMENT_UI_JS, ManagedCodexVersionCacheEntry, ManagementApiHandle,
        ManagementEventSnapshot, MirrorPreviewDebugEventView, UpdateTelegramSetupRequest,
        WorkingSessionRecordView, WorkingSessionSummaryView, diff_management_event_snapshots,
        managed_codex_version_cache_hit, spawn_management_api, write_env_file,
    };
    use crate::app_server_runtime::WorkspaceRuntimeManager;
    use crate::collaboration_mode::CollaborationMode;
    use crate::config::RuntimeConfig;
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::repository::{
        ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
        TranscriptMirrorRole,
    };
    use crate::runtime_control::{
        RuntimeControlContext, RuntimeOwnershipMode, SharedControlHandle,
    };
    use crate::runtime_protocol::{
        RuntimeEventKind, RuntimeEventOperation, WorkingSessionRecordKind,
    };
    use crate::workspace_status::record_tui_mirror_preview_sync;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime};
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "threadbridge-management-api-test-{}",
            Uuid::new_v4()
        ))
    }

    fn runtime_config(root: &PathBuf) -> RuntimeConfig {
        RuntimeConfig {
            data_root_path: root.join("data"),
            debug_log_path: root.join("debug.log"),
            runtime_support_root_path: root.join("runtime_support"),
            runtime_support_seed_root_path: root.join("runtime_support"),
            codex_model: None,
            management_bind_addr: "127.0.0.1:0".parse().unwrap(),
        }
    }

    fn full_auto_snapshot() -> SessionExecutionSnapshot {
        SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto)
    }

    async fn install_shared_control(handle: &ManagementApiHandle, root: &PathBuf) {
        let template_dir = root.join("runtime_support").join("templates");
        fs::create_dir_all(&template_dir).await.unwrap();
        fs::write(template_dir.join("AGENTS.md"), "test template")
            .await
            .unwrap();
        let control = RuntimeControlContext::new(
            runtime_config(root),
            WorkspaceRuntimeManager::new(),
            None,
            RuntimeOwnershipMode::DesktopOwner,
        )
        .await
        .unwrap();
        handle
            .set_shared_control(Some(SharedControlHandle::new(control)))
            .await;
    }

    #[test]
    fn management_ui_uses_hcodex_ingress_status_field() {
        assert!(MANAGEMENT_UI_JS.contains("hcodex_ingress_status"));
        assert!(!MANAGEMENT_UI_JS.contains("tui_proxy_status"));
    }

    #[tokio::test]
    async fn setup_state_marks_first_run_until_setup_file_exists() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();

        let initial = handle.setup_state().await.unwrap();
        assert!(initial.first_run);
        assert!(!initial.telegram_token_configured);
        assert!(!initial.launch_at_login.supported);
        assert_eq!(initial.bot_url, None);

        handle
            .state
            .write_telegram_setup(UpdateTelegramSetupRequest {
                telegram_token: "123:abc".to_owned(),
                authorized_user_ids: vec![7, 8],
            })
            .await
            .unwrap();

        let updated = handle.setup_state().await.unwrap();
        assert!(!updated.first_run);
        assert!(updated.telegram_token_configured);
        assert!(!updated.launch_at_login.supported);
        assert_eq!(updated.authorized_user_ids, vec![7, 8]);
    }

    #[tokio::test]
    async fn setup_state_uses_runtime_scoped_config_file() {
        let root_a = temp_path();
        let root_b = temp_path();
        fs::create_dir_all(root_a.join("data")).await.unwrap();
        fs::create_dir_all(root_b.join("data")).await.unwrap();

        write_env_file(
            &root_a.join("data").join("config.env.local"),
            &BTreeMap::from([
                ("TELEGRAM_BOT_TOKEN".to_owned(), "111:aaa".to_owned()),
                ("AUTHORIZED_TELEGRAM_USER_IDS".to_owned(), "7".to_owned()),
            ]),
        )
        .await
        .unwrap();
        write_env_file(
            &root_b.join("data").join("config.env.local"),
            &BTreeMap::from([
                ("TELEGRAM_BOT_TOKEN".to_owned(), "222:bbb".to_owned()),
                (
                    "AUTHORIZED_TELEGRAM_USER_IDS".to_owned(),
                    "41,99".to_owned(),
                ),
            ]),
        )
        .await
        .unwrap();

        let handle_a = spawn_management_api(runtime_config(&root_a)).await.unwrap();
        let handle_b = spawn_management_api(runtime_config(&root_b)).await.unwrap();

        let setup_a = handle_a.setup_state().await.unwrap();
        let setup_b = handle_b.setup_state().await.unwrap();
        assert_eq!(setup_a.authorized_user_ids, vec![7]);
        assert_eq!(setup_b.authorized_user_ids, vec![41, 99]);
    }

    #[tokio::test]
    async fn session_routes_return_summaries_and_records() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        let repo: ThreadRepository = handle.state.repository.clone();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
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
                turn_id: Some("turn-1".to_owned()),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "hello".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:01.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                turn_id: Some("turn-1".to_owned()),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "done".to_owned(),
            },
        ] {
            repo.append_transcript_mirror(&record, &entry)
                .await
                .unwrap();
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

        record_tui_mirror_preview_sync(
            &workspace,
            "thr_current",
            Some("turn-1"),
            Some("item-1"),
            "2026-04-07T10:00:00.000Z",
            "skipped_regressive",
            Some("already_owned"),
            Some("turn-1"),
            Some("turn-1"),
            Some("item-0"),
            Some("item-1"),
            false,
            true,
            true,
            "Drafting",
            "Drafting a longer preview",
            11,
        )
        .await
        .unwrap();

        let mirror_debug = client
            .get(format!(
                "{}/api/threads/{}/sessions/thr_current/mirror-preview-events",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert!(mirror_debug.status().is_success());
        let mirror_debug: Vec<MirrorPreviewDebugEventView> = mirror_debug.json().await.unwrap();
        assert_eq!(mirror_debug.len(), 1);
        assert_eq!(mirror_debug[0].decision, "skipped_regressive");
        assert_eq!(
            mirror_debug[0].claim_status.as_deref(),
            Some("already_owned")
        );
        assert_eq!(mirror_debug[0].draft_id, Some(11));

        let missing = client
            .get(format!(
                "{}/api/threads/{}/sessions/missing/records",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

        let missing_debug = client
            .get(format!(
                "{}/api/threads/{}/sessions/missing/mirror-preview-events",
                handle.base_url, record.metadata.thread_key
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(missing_debug.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn runtime_control_action_route_exists_and_legacy_routes_are_gone() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        let client = reqwest::Client::new();

        let actions = client
            .post(format!("{}/api/threads/thread-1/actions", handle.base_url))
            .json(&serde_json::json!({ "action": "repair_session_binding" }))
            .send()
            .await
            .unwrap();
        assert_ne!(actions.status(), reqwest::StatusCode::NOT_FOUND);

        let legacy_repair = client
            .post(format!(
                "{}/api/threads/thread-1/repair-session-binding",
                handle.base_url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(legacy_repair.status(), reqwest::StatusCode::NOT_FOUND);

        let legacy_launch_new = client
            .post(format!(
                "{}/api/workspaces/thread-1/launch-hcodex-new",
                handle.base_url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(legacy_launch_new.status(), reqwest::StatusCode::NOT_FOUND);

        let legacy_launch_current = client
            .post(format!(
                "{}/api/workspaces/thread-1/launch-hcodex-continue-current",
                handle.base_url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            legacy_launch_current.status(),
            reqwest::StatusCode::NOT_FOUND
        );

        let legacy_launch_resume = client
            .post(format!(
                "{}/api/workspaces/thread-1/launch-hcodex-resume",
                handle.base_url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            legacy_launch_resume.status(),
            reqwest::StatusCode::NOT_FOUND
        );

        let legacy_set_mode = client
            .put(format!(
                "{}/api/workspaces/thread-1/execution-mode",
                handle.base_url
            ))
            .json(&serde_json::json!({ "execution_mode": "full_auto" }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            legacy_set_mode.status(),
            reqwest::StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn runtime_control_action_route_updates_collaboration_mode() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        install_shared_control(&handle, &root).await;
        let repo: ThreadRepository = handle.state.repository.clone();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let response = reqwest::Client::new()
            .post(format!(
                "{}/api/threads/{}/actions",
                handle.base_url, record.metadata.thread_key
            ))
            .json(&serde_json::json!({
                "action": "set_thread_collaboration_mode",
                "mode": "plan"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["action"], "set_thread_collaboration_mode");
        assert_eq!(value["result"]["mode"], "plan");

        let binding = repo.read_session_binding(&record).await.unwrap().unwrap();
        assert_eq!(
            binding.current_collaboration_mode,
            Some(CollaborationMode::Plan)
        );
    }

    #[tokio::test]
    async fn existing_workspace_add_blocks_existing_active_binding_without_telegram_bridge() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        install_shared_control(&handle, &root).await;
        let repo: ThreadRepository = handle.state.repository.clone();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let error = handle
            .add_workspace(&workspace.display().to_string())
            .await
            .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("blocked_by_active_binding"));
        assert!(rendered.contains(&record.metadata.thread_key));
    }

    #[tokio::test]
    async fn archive_thread_still_works_without_telegram_bridge() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        install_shared_control(&handle, &root).await;
        let repo: ThreadRepository = handle.state.repository.clone();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();

        let response = handle
            .state
            .archive_thread(&record.metadata.thread_key)
            .await
            .unwrap();
        assert!(response.archived);
        let archived = repo
            .get_thread_by_key(1, &record.metadata.thread_key)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            archived.metadata.status,
            crate::repository::ThreadStatus::Archived
        ));
    }

    #[tokio::test]
    async fn restore_thread_requires_telegram_bridge() {
        let root = temp_path();
        fs::create_dir_all(&root).await.unwrap();
        let handle = spawn_management_api(runtime_config(&root)).await.unwrap();
        install_shared_control(&handle, &root).await;
        let repo: ThreadRepository = handle.state.repository.clone();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let archived = repo.archive_thread(record).await.unwrap();

        let error = handle
            .state
            .restore_thread(&archived.metadata.thread_key)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Telegram bot runtime is unavailable")
        );
    }

    #[test]
    fn diff_management_event_snapshots_emits_typed_changes() {
        let previous = ManagementEventSnapshot {
            setup: json!({"telegram_polling_state": "active"}),
            runtime: json!({"runtime_readiness": "ready"}),
            threads: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "run_status": "idle", "run_phase": "idle"}),
            ))
            .collect(),
            workspaces: std::iter::once((
                "/tmp/workspace".to_owned(),
                json!({"workspace_cwd": "/tmp/workspace", "run_status": "idle", "run_phase": "idle"}),
            ))
            .collect(),
            pending_approvals: BTreeMap::new(),
            archived_threads: BTreeMap::new(),
            working_sessions: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "session_count": 1, "updated_at": "2026-03-24T10:00:00.000Z", "run_status": "running", "run_phase": "turn_running"}),
            ))
            .collect(),
            transcripts: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "entry_count": 1, "updated_at": "2026-03-24T10:00:00.000Z"}),
            ))
            .collect(),
        };
        let current = ManagementEventSnapshot {
            setup: json!({"telegram_polling_state": "active"}),
            runtime: json!({"runtime_readiness": "degraded"}),
            threads: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "run_status": "running", "run_phase": "turn_finalizing"}),
            ))
            .collect(),
            workspaces: std::iter::once((
                "/tmp/workspace".to_owned(),
                json!({"workspace_cwd": "/tmp/workspace", "run_status": "running", "run_phase": "turn_finalizing"}),
            ))
            .collect(),
            pending_approvals: BTreeMap::new(),
            archived_threads: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "archived_at": "2026-03-24T00:00:00.000Z"}),
            ))
            .collect(),
            working_sessions: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "session_count": 2, "updated_at": "2026-03-24T10:05:00.000Z", "run_status": "running", "run_phase": "turn_finalizing"}),
            ))
            .collect(),
            transcripts: std::iter::once((
                "thread-1".to_owned(),
                json!({"thread_key": "thread-1", "entry_count": 2, "updated_at": "2026-03-24T10:05:00.000Z"}),
            ))
            .collect(),
        };

        let events = diff_management_event_snapshots(Some(&previous), &current);
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::RuntimeHealthChanged
                && event.op == RuntimeEventOperation::Upsert
        }));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::ThreadStateChanged
                && event.op == RuntimeEventOperation::Upsert
                && event.key.as_deref() == Some("thread-1")
                && event
                    .current
                    .as_ref()
                    .and_then(|value| value.get("run_phase"))
                    .and_then(|value| value.as_str())
                    == Some("turn_finalizing")
        }));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::WorkspaceStateChanged
                && event.op == RuntimeEventOperation::Upsert
                && event.key.as_deref() == Some("/tmp/workspace")
                && event
                    .current
                    .as_ref()
                    .and_then(|value| value.get("run_phase"))
                    .and_then(|value| value.as_str())
                    == Some("turn_finalizing")
        }));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::ArchivedThreadChanged
                && event.op == RuntimeEventOperation::Upsert
                && event.key.as_deref() == Some("thread-1")
        }));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::WorkingSessionChanged
                && event.op == RuntimeEventOperation::Upsert
                && event.key.as_deref() == Some("thread-1")
                && event
                    .current
                    .as_ref()
                    .and_then(|value| value.get("run_phase"))
                    .and_then(|value| value.as_str())
                    == Some("turn_finalizing")
        }));
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::TranscriptChanged
                && event.op == RuntimeEventOperation::Upsert
                && event.key.as_deref() == Some("thread-1")
        }));
    }

    #[test]
    fn diff_management_event_snapshots_emits_managed_codex_changed() {
        let previous = ManagementEventSnapshot {
            setup: json!({}),
            runtime: json!({
                "runtime_readiness": "ready",
                "managed_codex": {"version": "1.0.0"}
            }),
            threads: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            pending_approvals: BTreeMap::new(),
            archived_threads: BTreeMap::new(),
            working_sessions: BTreeMap::new(),
            transcripts: BTreeMap::new(),
        };
        let current = ManagementEventSnapshot {
            setup: json!({}),
            runtime: json!({
                "runtime_readiness": "ready",
                "managed_codex": {"version": "1.0.1"}
            }),
            threads: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            pending_approvals: BTreeMap::new(),
            archived_threads: BTreeMap::new(),
            working_sessions: BTreeMap::new(),
            transcripts: BTreeMap::new(),
        };

        let events = diff_management_event_snapshots(Some(&previous), &current);
        assert!(events.iter().any(|event| {
            event.kind == RuntimeEventKind::ManagedCodexChanged
                && event.op == RuntimeEventOperation::Upsert
                && event
                    .current
                    .as_ref()
                    .and_then(|value| value.get("version"))
                    == Some(&json!("1.0.1"))
        }));
    }

    #[test]
    fn managed_codex_version_cache_hit_requires_matching_key_and_ttl() {
        let modified_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
        let now = Instant::now();
        let entry = ManagedCodexVersionCacheEntry {
            binary_path: "/tmp/codex".to_owned(),
            modified_at,
            expires_at: now + Duration::from_secs(300),
            version: Some("1.0.0".to_owned()),
        };

        assert!(managed_codex_version_cache_hit(
            &entry,
            "/tmp/codex",
            modified_at,
            now
        ));
        assert!(!managed_codex_version_cache_hit(
            &entry,
            "/tmp/other-codex",
            modified_at,
            now
        ));
        assert!(!managed_codex_version_cache_hit(
            &entry,
            "/tmp/codex",
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(20)),
            now
        ));
        assert!(!managed_codex_version_cache_hit(
            &entry,
            "/tmp/codex",
            modified_at,
            now + Duration::from_secs(301)
        ));
    }

    #[test]
    fn runtime_event_remove_serialization_omits_current_payload() {
        let event = crate::runtime_protocol::RuntimeEvent {
            kind: RuntimeEventKind::WorkspaceStateChanged,
            op: RuntimeEventOperation::Remove,
            key: Some("/tmp/workspace".to_owned()),
            current: None,
            message: None,
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["kind"], "workspace_state_changed");
        assert_eq!(value["op"], "remove");
        assert_eq!(value["key"], "/tmp/workspace");
        assert!(value.get("current").is_none());
        assert!(value.get("message").is_none());
    }
}
