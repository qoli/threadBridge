#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("threadbridge_desktop is currently implemented for macOS only")
}

#[cfg(target_os = "macos")]
mod macos_app {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Result, anyhow};
    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use tokio::time::{Instant, sleep_until};
    use tracing::{info, warn};
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use threadbridge_rust::bot_runner::spawn_bot_runtime_from_env_with_runtimes;
    use threadbridge_rust::config::{RuntimeConfig, load_runtime_config};
    use threadbridge_rust::hcodex_runtime;
    use threadbridge_rust::hcodex_ws_bridge;
    use threadbridge_rust::logging::init_runtime_json_logs;
    use threadbridge_rust::management_api::{
        LaunchLocalSessionTarget, ManagedWorkspaceView, ManagementApiHandle,
        RuntimeControlActionRequest, RuntimeHealthView, TelegramPollingState, spawn_management_api,
    };
    use threadbridge_rust::runtime_control::{
        RuntimeControlContext, RuntimeOwnershipMode, SharedControlHandle,
    };
    use threadbridge_rust::runtime_owner::DesktopRuntimeOwner;
    use threadbridge_rust::runtime_support::{ensure_runtime_support, rebuild_runtime_support};

    const TRAY_ICON_SIZE: u32 = 32;
    const TOKIO_WORKER_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;
    const TRAY_ICON_RGBA: &[u8] =
        include_bytes!("../../static/tray/point_3_filled_connected_trianglepath_dotted.rgba");

    #[derive(Debug, Clone)]
    enum UserEvent {
        Menu(MenuEvent),
        Snapshot(Box<DesktopSnapshot>),
        Refresh,
        ShowSettings,
        Quit,
    }

    #[derive(Debug, Clone)]
    struct DesktopSnapshot {
        setup: DesktopSetupSnapshot,
        health: RuntimeHealthView,
        workspaces: Vec<ManagedWorkspaceView>,
    }

    #[derive(Debug, Clone)]
    struct DesktopSetupSnapshot {
        telegram_polling_state: TelegramPollingState,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DesktopPollMode {
        Fast,
        Slow,
    }

    impl DesktopPollMode {
        fn interval(self) -> Duration {
            match self {
                Self::Fast => Duration::from_secs(3),
                Self::Slow => Duration::from_secs(15),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TraySnapshotSignature {
        tooltip: String,
        workspaces: Vec<TrayWorkspaceSignature>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TrayWorkspaceSignature {
        thread_key: String,
        label: String,
        can_launch_new: bool,
        can_continue_current: bool,
    }

    #[derive(Debug, Clone)]
    enum TrayAction {
        OpenSettings,
        OpenAddWorkspace,
        PurgeArchivedThreads,
        ResetThreadTitles,
        RebuildRuntimeSupport,
        Quit,
        LaunchNew { thread_key: String },
        ContinueCurrent { thread_key: String },
    }

    struct MenuModel {
        menu: Menu,
        actions: HashMap<MenuId, TrayAction>,
    }

    struct DesktopApp {
        runtime: Arc<Runtime>,
        runtime_config: RuntimeConfig,
        management_api: ManagementApiHandle,
        owner: Arc<DesktopRuntimeOwner>,
        tray_icon: Option<TrayIcon>,
        tray_actions: HashMap<MenuId, TrayAction>,
        latest_snapshot: Option<DesktopSnapshot>,
        latest_tray_signature: Option<TraySnapshotSignature>,
    }

    pub fn run() -> Result<()> {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        let runtime = Arc::new(
            RuntimeBuilder::new_multi_thread()
                .enable_all()
                .thread_stack_size(TOKIO_WORKER_STACK_SIZE_BYTES)
                .build()
                .expect("tokio runtime"),
        );
        if runtime.block_on(hcodex_ws_bridge::maybe_run_from_args(args.clone()))? {
            return Ok(());
        }
        if runtime.block_on(hcodex_runtime::maybe_run_from_args(args.clone()))? {
            return Ok(());
        }

        let runtime_config = load_runtime_config()?;
        runtime.block_on(ensure_runtime_support(&runtime_config))?;
        let _guard = init_runtime_json_logs(&runtime_config.debug_log_path)?;
        let management_api = runtime.block_on(spawn_management_api(runtime_config.clone()))?;
        runtime.block_on(management_api.set_native_workspace_picker_available(true));
        let owner = Arc::new(runtime.block_on(DesktopRuntimeOwner::new(
            runtime_config.clone(),
            management_api.runtime_telemetry_handle(),
        ))?);
        runtime.block_on(management_api.set_runtime_owner(Some((*owner).clone())));
        let shared_control = runtime.block_on(RuntimeControlContext::new(
            runtime_config.clone(),
            owner.app_server_runtime(),
            None,
            RuntimeOwnershipMode::DesktopOwner,
        ))?;
        runtime.block_on(
            management_api.set_shared_control(Some(SharedControlHandle::new(shared_control))),
        );
        runtime.block_on(reconcile_runtime_owner(&management_api, &owner));
        maybe_open_first_run_welcome(&runtime, &management_api)?;

        if runtime
            .block_on(spawn_bot_runtime_from_env_with_runtimes(
                management_api.clone(),
                owner.app_server_runtime(),
            ))?
            .is_some()
        {
            info!(
                event = "desktop_runtime.bot.started",
                management_api_base_url = %management_api.base_url,
                "desktop runtime started Telegram bot"
            );
        } else {
            info!(
                event = "desktop_runtime.bot.deferred",
                management_api_base_url = %management_api.base_url,
                "desktop runtime started without Telegram credentials"
            );
        }

        let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        // Keep the desktop owner as a background menubar utility instead of a Dock app.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
        let proxy = event_loop.create_proxy();
        tray_icon::menu::MenuEvent::set_event_handler(Some({
            let proxy = proxy.clone();
            move |event| {
                let _ = proxy.send_event(UserEvent::Menu(event));
            }
        }));

        spawn_snapshot_poller(
            runtime.clone(),
            management_api.clone(),
            owner.clone(),
            proxy.clone(),
        );

        let mut app = DesktopApp {
            runtime,
            runtime_config,
            management_api,
            owner,
            tray_icon: None,
            tray_actions: HashMap::new(),
            latest_snapshot: None,
            latest_tray_signature: None,
        };

        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::NewEvents(StartCause::Init) => {
                    if app.tray_icon.is_none() {
                        match build_tray_icon() {
                            Ok(tray_icon) => app.tray_icon = Some(tray_icon),
                            Err(error) => {
                                warn!(event = "desktop_runtime.tray.init_failed", error = %error);
                            }
                        }
                    }
                }
                Event::UserEvent(UserEvent::Menu(event)) => {
                    handle_menu_event(&mut app, event, control_flow, &proxy);
                }
                Event::UserEvent(UserEvent::Snapshot(snapshot)) => {
                    app.latest_snapshot = Some((*snapshot).clone());
                    if let Err(error) = update_tray_snapshot(&mut app, &snapshot) {
                        warn!(event = "desktop_runtime.tray.update_failed", error = %error);
                    }
                }
                Event::UserEvent(UserEvent::Refresh) => {
                    spawn_refresh_cycle(
                        app.runtime.clone(),
                        app.management_api.clone(),
                        app.owner.clone(),
                        proxy.clone(),
                    );
                }
                Event::UserEvent(UserEvent::ShowSettings) => {
                    if let Err(error) = open_settings_url(&app) {
                        warn!(event = "desktop_runtime.settings.open_failed", error = %error);
                    }
                }
                Event::UserEvent(UserEvent::Quit) => {
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            }
        });
    }

    fn build_tray_icon() -> Result<TrayIcon> {
        let menu = Menu::new();
        let icon = build_icon()?;
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("threadBridge")
            .with_icon(icon)
            .with_icon_as_template(true)
            .build()?;
        Ok(tray_icon)
    }

    fn build_icon() -> Result<Icon> {
        let expected_len = (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize;
        anyhow::ensure!(
            TRAY_ICON_RGBA.len() == expected_len,
            "invalid tray icon asset length: expected {expected_len}, got {}",
            TRAY_ICON_RGBA.len()
        );
        Ok(Icon::from_rgba(
            TRAY_ICON_RGBA.to_vec(),
            TRAY_ICON_SIZE,
            TRAY_ICON_SIZE,
        )?)
    }

    fn spawn_snapshot_poller(
        runtime: Arc<Runtime>,
        management_api: ManagementApiHandle,
        owner: Arc<DesktopRuntimeOwner>,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        runtime.spawn(async move {
            let launched_at = Instant::now();
            let mut latest_snapshot: Option<DesktopSnapshot> = None;
            let mut next_snapshot_at = Instant::now();
            let mut next_reconcile_at = Instant::now();

            loop {
                let now = Instant::now();

                if now >= next_reconcile_at {
                    reconcile_runtime_owner(&management_api, &owner).await;
                    maybe_start_bot_runtime(&management_api, &owner).await;
                    next_reconcile_at =
                        Instant::now() + reconcile_interval(latest_snapshot.as_ref());
                }

                if now >= next_snapshot_at {
                    match collect_snapshot(&management_api).await {
                        Ok(snapshot) => {
                            latest_snapshot = Some(snapshot.clone());
                            let _ = proxy.send_event(UserEvent::Snapshot(Box::new(snapshot)));
                        }
                        Err(error) => {
                            warn!(event = "desktop_runtime.snapshot.failed", error = %error);
                        }
                    }
                    let poll_mode = snapshot_poll_mode(latest_snapshot.as_ref(), launched_at);
                    next_snapshot_at = Instant::now() + poll_mode.interval();
                    let desired_reconcile_at =
                        Instant::now() + reconcile_interval(latest_snapshot.as_ref());
                    if desired_reconcile_at < next_reconcile_at {
                        next_reconcile_at = desired_reconcile_at;
                    }
                }

                sleep_until(next_snapshot_at.min(next_reconcile_at)).await;
            }
        });
    }

    fn spawn_refresh_cycle(
        runtime: Arc<Runtime>,
        management_api: ManagementApiHandle,
        owner: Arc<DesktopRuntimeOwner>,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        runtime.spawn(async move {
            reconcile_runtime_owner(&management_api, &owner).await;
            maybe_start_bot_runtime(&management_api, &owner).await;
            send_snapshot(&management_api, &proxy).await;
        });
    }

    fn snapshot_poll_mode(
        snapshot: Option<&DesktopSnapshot>,
        launched_at: Instant,
    ) -> DesktopPollMode {
        if launched_at.elapsed() < Duration::from_secs(60) {
            return DesktopPollMode::Fast;
        }
        if snapshot.is_some_and(desktop_snapshot_requires_fast_poll) {
            return DesktopPollMode::Fast;
        }
        DesktopPollMode::Slow
    }

    fn desktop_snapshot_requires_fast_poll(snapshot: &DesktopSnapshot) -> bool {
        snapshot.workspaces.iter().any(|workspace| {
            workspace.run_status == "running"
                || workspace.runtime_readiness != "ready"
                || workspace.binding_status == "broken"
                || workspace.conflict
                || workspace.tui_session_adoption_pending
        })
    }

    fn reconcile_interval(snapshot: Option<&DesktopSnapshot>) -> Duration {
        if snapshot.is_some_and(desktop_snapshot_requires_frequent_reconcile) {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(60)
        }
    }

    fn desktop_snapshot_requires_frequent_reconcile(snapshot: &DesktopSnapshot) -> bool {
        snapshot.workspaces.iter().any(|workspace| {
            workspace.runtime_readiness != "ready"
                || workspace.binding_status == "broken"
                || workspace.conflict
        })
    }

    async fn collect_snapshot(management_api: &ManagementApiHandle) -> Result<DesktopSnapshot> {
        let started_at = Instant::now();
        let result = async {
            let setup = DesktopSetupSnapshot {
                telegram_polling_state: management_api.current_telegram_polling_state().await,
            };
            let runtime = management_api.runtime_overview().await?;
            Ok::<DesktopSnapshot, anyhow::Error>(DesktopSnapshot {
                setup,
                health: runtime.health,
                workspaces: runtime.workspaces,
            })
        }
        .await;

        let mut metrics = threadbridge_rust::telemetry::RuntimeTelemetryMetrics::new();
        match &result {
            Ok(snapshot) => {
                metrics.insert(
                    "workspace_count".to_owned(),
                    snapshot.workspaces.len() as u64,
                );
                metrics.insert(
                    "running_workspaces".to_owned(),
                    snapshot.health.running_workspaces as u64,
                );
                metrics.insert(
                    "telegram_polling_active".to_owned(),
                    u64::from(
                        snapshot.setup.telegram_polling_state == TelegramPollingState::Active,
                    ),
                );
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.collect_snapshot",
                    started_at.into(),
                    "ok",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
            }
            Err(error) => {
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.collect_snapshot",
                    started_at.into(),
                    "error",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    metrics,
                    Some(error.to_string()),
                );
            }
        }

        result
    }

    async fn send_snapshot(
        management_api: &ManagementApiHandle,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let started_at = Instant::now();
        match collect_snapshot(management_api).await {
            Ok(snapshot) => {
                let workspace_count = snapshot.workspaces.len() as u64;
                let _ = proxy.send_event(UserEvent::Snapshot(Box::new(snapshot)));
                let mut metrics = threadbridge_rust::telemetry::RuntimeTelemetryMetrics::new();
                metrics.insert("workspace_count".to_owned(), workspace_count);
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.send_snapshot",
                    started_at.into(),
                    "ok",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
            }
            Err(error) => {
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.send_snapshot",
                    started_at.into(),
                    "error",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    threadbridge_rust::telemetry::RuntimeTelemetryMetrics::new(),
                    Some(error.to_string()),
                );
                warn!(event = "desktop_runtime.snapshot.failed", error = %error);
            }
        }
    }

    async fn maybe_start_bot_runtime(
        management_api: &ManagementApiHandle,
        owner: &DesktopRuntimeOwner,
    ) {
        let Ok(setup) = management_api.setup_state().await else {
            return;
        };
        if !setup.telegram_token_configured
            || setup.telegram_polling_state
                != threadbridge_rust::management_api::TelegramPollingState::Disconnected
        {
            return;
        }
        match spawn_bot_runtime_from_env_with_runtimes(
            management_api.clone(),
            owner.app_server_runtime(),
        )
        .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(error) => {
                warn!(event = "desktop_runtime.bot.auto_start_failed", error = %error);
            }
        }
    }

    async fn reconcile_runtime_owner(
        management_api: &ManagementApiHandle,
        owner: &DesktopRuntimeOwner,
    ) {
        let started_at = Instant::now();
        let result = async {
            let workspaces = management_api.workspace_views().await?;
            let targets = workspaces
                .into_iter()
                .filter(|workspace| !workspace.conflict)
                .map(|workspace| workspace.workspace_cwd)
                .collect::<Vec<_>>();
            if targets.is_empty() {
                return Ok::<usize, anyhow::Error>(0);
            }
            owner.reconcile_managed_workspaces(targets.clone()).await?;
            Ok(targets.len())
        }
        .await;

        let mut metrics = threadbridge_rust::telemetry::RuntimeTelemetryMetrics::new();
        match result {
            Ok(target_count) => {
                metrics.insert("target_workspaces".to_owned(), target_count as u64);
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.reconcile_runtime_owner",
                    started_at.into(),
                    "ok",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    metrics,
                    None,
                );
            }
            Err(error) => {
                management_api.runtime_telemetry_handle().record_duration(
                    "desktop.reconcile_runtime_owner",
                    started_at.into(),
                    "error",
                    threadbridge_rust::telemetry::RuntimeTelemetryFields::new(),
                    metrics,
                    Some(error.to_string()),
                );
                warn!(
                    event = "desktop_runtime.owner.reconcile.failed",
                    error = %error,
                    "desktop runtime owner reconciliation failed"
                );
            }
        }
    }

    fn update_tray_snapshot(app: &mut DesktopApp, snapshot: &DesktopSnapshot) -> Result<()> {
        let signature = build_tray_snapshot_signature(snapshot);
        if app.tray_icon.is_some() && app.latest_tray_signature.as_ref() == Some(&signature) {
            return Ok(());
        }
        let model = build_menu_model(
            snapshot,
            app.runtime_config.supports_runtime_support_rebuild(),
        )?;
        if let Some(tray_icon) = app.tray_icon.as_ref() {
            tray_icon.set_menu(Some(Box::new(model.menu)));
            tray_icon.set_tooltip(Some(&signature.tooltip))?;
        }
        app.tray_actions = model.actions;
        app.latest_tray_signature = Some(signature);
        Ok(())
    }

    fn build_menu_model(
        snapshot: &DesktopSnapshot,
        supports_runtime_support_rebuild: bool,
    ) -> Result<MenuModel> {
        let menu = Menu::new();
        let mut actions = HashMap::new();
        for workspace in snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.thread_key.is_some() && !workspace.conflict)
        {
            let Some(thread_key) = workspace.thread_key.clone() else {
                continue;
            };
            let submenu = Submenu::new(workspace_tray_label(workspace), true);
            let can_launch_new = workspace_launch_ready(workspace);
            let can_continue_current = workspace_continue_current_ready(workspace);
            let start_item = MenuItem::new("New Session", can_launch_new, None);
            actions.insert(
                start_item.id().clone(),
                TrayAction::LaunchNew {
                    thread_key: thread_key.clone(),
                },
            );
            let continue_item =
                MenuItem::new("Continue Telegram Session", can_continue_current, None);
            actions.insert(
                continue_item.id().clone(),
                TrayAction::ContinueCurrent {
                    thread_key: thread_key.clone(),
                },
            );
            let separator = PredefinedMenuItem::separator();
            submenu.append_items(&[&start_item, &separator, &continue_item])?;
            menu.append(&submenu)?;
        }
        if !snapshot.workspaces.is_empty() {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        let add_workspace = MenuItem::new("Add Workspace", true, None);
        actions.insert(add_workspace.id().clone(), TrayAction::OpenAddWorkspace);
        let purge_archived = MenuItem::new("Purge Archived Threads", true, None);
        actions.insert(
            purge_archived.id().clone(),
            TrayAction::PurgeArchivedThreads,
        );
        let reset_titles = MenuItem::new("Reset Thread Titles", true, None);
        actions.insert(reset_titles.id().clone(), TrayAction::ResetThreadTitles);
        menu.append(&add_workspace)?;
        menu.append(&purge_archived)?;
        if supports_runtime_support_rebuild {
            let rebuild_support = MenuItem::new("Rebuild Runtime Support", true, None);
            actions.insert(
                rebuild_support.id().clone(),
                TrayAction::RebuildRuntimeSupport,
            );
            menu.append(&rebuild_support)?;
        }
        menu.append(&reset_titles)?;
        menu.append(&PredefinedMenuItem::separator())?;
        let settings = MenuItem::new("Settings", true, None);
        actions.insert(settings.id().clone(), TrayAction::OpenSettings);
        menu.append(&settings)?;
        menu.append(&PredefinedMenuItem::separator())?;
        let quit = MenuItem::new("Quit", true, None);
        actions.insert(quit.id().clone(), TrayAction::Quit);
        menu.append(&quit)?;
        Ok(MenuModel { menu, actions })
    }

    fn build_tray_snapshot_signature(snapshot: &DesktopSnapshot) -> TraySnapshotSignature {
        let ready_workspaces = snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.runtime_readiness == "ready")
            .count();
        let degraded_workspaces = snapshot
            .workspaces
            .iter()
            .filter(|workspace| {
                matches!(workspace.runtime_readiness, "degraded" | "pending_adoption")
            })
            .count();
        let unavailable_workspaces = snapshot
            .workspaces
            .iter()
            .filter(|workspace| {
                !matches!(
                    workspace.runtime_readiness,
                    "ready" | "degraded" | "pending_adoption"
                )
            })
            .count();
        TraySnapshotSignature {
            tooltip: format!(
                "threadBridge | owner {} | polling {:?} | ws ready {} degraded {} unavailable {} | broken {} | conflicted {}",
                snapshot.health.runtime_owner.state,
                snapshot.setup.telegram_polling_state,
                ready_workspaces,
                degraded_workspaces,
                unavailable_workspaces,
                snapshot.health.broken_threads,
                snapshot.health.conflicted_workspaces
            ),
            workspaces: snapshot
                .workspaces
                .iter()
                .filter_map(|workspace| {
                    Some(TrayWorkspaceSignature {
                        thread_key: workspace.thread_key.clone()?,
                        label: workspace_tray_label(workspace),
                        can_launch_new: workspace_launch_ready(workspace),
                        can_continue_current: workspace_continue_current_ready(workspace),
                    })
                })
                .collect(),
        }
    }

    fn workspace_tray_label(workspace: &ManagedWorkspaceView) -> String {
        let title = workspace
            .title
            .clone()
            .unwrap_or_else(|| workspace.workspace_cwd.clone());
        format!("{title} · {}", workspace.workspace_execution_mode.as_str())
    }

    fn workspace_launch_ready(workspace: &ManagedWorkspaceView) -> bool {
        workspace.hcodex_available
            && workspace.app_server_status == "running"
            && !matches!(workspace.runtime_readiness, "unavailable")
    }

    fn workspace_continue_current_ready(workspace: &ManagedWorkspaceView) -> bool {
        workspace_launch_ready(workspace)
            && workspace
                .current_codex_thread_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    fn handle_menu_event(
        app: &mut DesktopApp,
        event: MenuEvent,
        control_flow: &mut ControlFlow,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let Some(action) = app.tray_actions.get(&event.id).cloned() else {
            return;
        };
        match action {
            TrayAction::OpenSettings => {
                let _ = proxy.send_event(UserEvent::ShowSettings);
            }
            TrayAction::OpenAddWorkspace => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = add_workspace_via_tray(&management_api).await {
                        warn!(event = "desktop_runtime.add_workspace.failed", error = %error);
                        let _ = show_desktop_notification(
                            "threadBridge",
                            &format!("Add workspace failed: {}", short_error_message(&error)),
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::PurgeArchivedThreads => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = purge_archived_threads_via_tray(&management_api).await {
                        warn!(
                            event = "desktop_runtime.purge_archived_threads.failed",
                            error = %error
                        );
                        let _ = show_desktop_notification(
                            "threadBridge",
                            &format!(
                                "Purge archived threads failed: {}",
                                short_error_message(&error)
                            ),
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::ResetThreadTitles => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = reset_thread_titles_via_tray(&management_api).await {
                        warn!(
                            event = "desktop_runtime.reset_thread_titles.failed",
                            error = %error
                        );
                        let _ = show_desktop_notification(
                            "threadBridge",
                            &format!(
                                "Reset thread titles failed: {}",
                                short_error_message(&error)
                            ),
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::RebuildRuntimeSupport => {
                let runtime = app.runtime.clone();
                let runtime_config = app.runtime_config.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) =
                        rebuild_runtime_support_via_tray(&runtime_config, &management_api).await
                    {
                        warn!(
                            event = "desktop_runtime.rebuild_runtime_support.failed",
                            error = %error
                        );
                        let _ = show_desktop_notification(
                            "threadBridge",
                            &format!(
                                "Rebuild runtime support failed: {}",
                                short_error_message(&error)
                            ),
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::Quit => {
                let _ = proxy.send_event(UserEvent::Quit);
                *control_flow = ControlFlow::Exit;
            }
            TrayAction::LaunchNew { thread_key } => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = management_api
                        .run_runtime_control_action(
                            &thread_key,
                            RuntimeControlActionRequest::LaunchLocalSession {
                                target: LaunchLocalSessionTarget::New,
                                session_id: None,
                            },
                        )
                        .await
                    {
                        warn!(
                            event = "desktop_runtime.launch_new.failed",
                            error = %error,
                            thread_key = %thread_key
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::ContinueCurrent { thread_key } => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = management_api
                        .run_runtime_control_action(
                            &thread_key,
                            RuntimeControlActionRequest::LaunchLocalSession {
                                target: LaunchLocalSessionTarget::ContinueCurrent,
                                session_id: None,
                            },
                        )
                        .await
                    {
                        warn!(
                            event = "desktop_runtime.launch_current.failed",
                            error = %error,
                            thread_key = %thread_key,
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
        }
    }

    fn open_settings_url(app: &DesktopApp) -> Result<()> {
        open_management_url(&app.management_api, None)
    }

    fn maybe_open_first_run_welcome(
        runtime: &Runtime,
        management_api: &ManagementApiHandle,
    ) -> Result<()> {
        let setup = runtime.block_on(management_api.setup_state())?;
        if !setup.first_run {
            return Ok(());
        }
        if let Err(error) = show_first_run_alert() {
            warn!(
                event = "desktop_runtime.first_run.alert_failed",
                error = %error
            );
        }
        open_management_url(management_api, Some("#/welcome"))
    }

    async fn add_workspace_via_tray(management_api: &ManagementApiHandle) -> Result<()> {
        let Some(workspace_cwd) = tokio::task::spawn_blocking(pick_workspace_folder).await?? else {
            info!(event = "desktop_runtime.add_workspace.cancelled");
            return Ok(());
        };
        let result = management_api.add_workspace(&workspace_cwd).await?;
        let display_name =
            workspace_display_name(result.workspace_cwd.as_deref(), result.title.as_deref());
        let notification_body = if result.created {
            info!(
                event = "desktop_runtime.add_workspace.created",
                thread_key = %result.thread_key,
                workspace_cwd = %workspace_cwd
            );
            format!("Added workspace: {display_name}")
        } else {
            info!(
                event = "desktop_runtime.add_workspace.existing",
                thread_key = %result.thread_key,
                workspace_cwd = %workspace_cwd
            );
            format!("Workspace already managed: {display_name}")
        };
        show_desktop_notification("threadBridge", &notification_body)?;
        Ok(())
    }

    async fn purge_archived_threads_via_tray(management_api: &ManagementApiHandle) -> Result<()> {
        let confirmed = tokio::task::spawn_blocking(confirm_purge_archived_threads).await??;
        if !confirmed {
            info!(event = "desktop_runtime.purge_archived_threads.cancelled");
            return Ok(());
        }
        let purged = management_api.purge_all_archived_threads().await?;
        show_desktop_notification(
            "threadBridge",
            &format!("Purged {purged} archived thread record(s)."),
        )?;
        Ok(())
    }

    async fn reset_thread_titles_via_tray(management_api: &ManagementApiHandle) -> Result<()> {
        let confirmed = tokio::task::spawn_blocking(confirm_reset_thread_titles).await??;
        if !confirmed {
            info!(event = "desktop_runtime.reset_thread_titles.cancelled");
            return Ok(());
        }
        let report = management_api.reset_workspace_thread_titles().await?;
        let mut body = format!(
            "Reset {} thread title(s) to workspace folder names.",
            report.reset
        );
        if report.skipped_unbound > 0 {
            body.push_str(&format!(" Skipped {} unbound.", report.skipped_unbound));
        }
        if report.refresh_failed > 0 {
            body.push_str(&format!(
                " {} Telegram refresh(es) failed; metadata was updated.",
                report.refresh_failed
            ));
        }
        show_desktop_notification("threadBridge", &body)?;
        Ok(())
    }

    async fn rebuild_runtime_support_via_tray(
        runtime: &RuntimeConfig,
        management_api: &ManagementApiHandle,
    ) -> Result<()> {
        let confirmed = tokio::task::spawn_blocking(confirm_rebuild_runtime_support).await??;
        if !confirmed {
            info!(event = "desktop_runtime.rebuild_runtime_support.cancelled");
            return Ok(());
        }
        rebuild_runtime_support(runtime).await?;
        let synced_workspaces = management_api
            .sync_active_workspace_runtime_surfaces()
            .await?;
        let cleanup_report = management_api
            .cleanup_legacy_runtime_agents_appendices()
            .await?;
        info!(
            event = "desktop_runtime.rebuild_runtime_support.cleaned_legacy_agents",
            synced_workspaces = synced_workspaces,
            scanned_workspaces = cleanup_report.scanned_workspaces,
            cleaned = cleanup_report.cleaned,
            unchanged = cleanup_report.unchanged,
            skipped_unbound = cleanup_report.skipped_unbound,
            failed = cleanup_report.failed,
            "runtime support rebuild workspace migration completed"
        );
        let mut body = format!(
            "Rebuilt runtime support. Synced {} workspace(s); cleaned legacy AGENTS.md blocks in {}.",
            synced_workspaces, cleanup_report.cleaned
        );
        if cleanup_report.scanned_workspaces > cleanup_report.cleaned {
            body.push_str(&format!(
                " Checked {} workspace(s).",
                cleanup_report.scanned_workspaces
            ));
        }
        if cleanup_report.skipped_unbound > 0 {
            body.push_str(&format!(
                " Skipped {} unbound thread(s).",
                cleanup_report.skipped_unbound
            ));
        }
        if cleanup_report.failed > 0 {
            body.push_str(&format!(" {} cleanup(s) failed.", cleanup_report.failed));
        }
        show_desktop_notification("threadBridge", &body)?;
        Ok(())
    }

    fn open_management_url(
        management_api: &ManagementApiHandle,
        anchor: Option<&str>,
    ) -> Result<()> {
        let mut url = management_api.base_url.clone();
        if let Some(anchor) = anchor {
            url.push_str(anchor);
        }
        let status = Command::new("open").arg(&url).status()?;
        anyhow::ensure!(
            status.success(),
            "failed to open management URL in default browser"
        );
        Ok(())
    }

    fn show_first_run_alert() -> Result<()> {
        let script = r#"display dialog "threadBridge will open a welcome page to help you create a Telegram bot, collect your authorized user IDs, and finish the first local setup." buttons {"Continue"} default button "Continue" with icon note"#;
        let output = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!(
            "first-run alert failed: {}",
            stderr.trim().if_empty("unknown osascript error")
        ))
    }

    fn pick_workspace_folder() -> Result<Option<String>> {
        let script = r#"POSIX path of (choose folder with prompt "Select a workspace to bind to threadBridge")"#;
        let output = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output()?;
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
            stderr.trim().if_empty("unknown osascript error")
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

    fn confirm_purge_archived_threads() -> Result<bool> {
        let script = r#"button returned of (display dialog "Purge all archived threadBridge Telegram thread data? This cannot be undone." buttons {"Cancel", "Purge"} default button "Cancel" cancel button "Cancel" with icon caution)"#;
        let output = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).contains("Purge"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if apple_script_user_cancelled(output.status.code(), &stderr) {
            return Ok(false);
        }
        Err(anyhow!(
            "purge confirmation failed: {}",
            stderr.trim().if_empty("unknown osascript error")
        ))
    }

    fn confirm_reset_thread_titles() -> Result<bool> {
        let script = r#"button returned of (display dialog "Reset all active threadBridge Telegram topic titles to their workspace folder names?" buttons {"Cancel", "Reset"} default button "Cancel" cancel button "Cancel" with icon caution)"#;
        let output = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).contains("Reset"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if apple_script_user_cancelled(output.status.code(), &stderr) {
            return Ok(false);
        }
        Err(anyhow!(
            "thread-title reset confirmation failed: {}",
            stderr.trim().if_empty("unknown osascript error")
        ))
    }

    fn confirm_rebuild_runtime_support() -> Result<bool> {
        let script = r#"button returned of (display dialog "Delete installed runtime support and rebuild it from the bundled app resources? This also removes legacy threadBridge managed blocks from bound workspace AGENTS.md files. Your data and config will be kept." buttons {"Cancel", "Rebuild"} default button "Cancel" cancel button "Cancel" with icon caution)"#;
        let output = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).contains("Rebuild"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if apple_script_user_cancelled(output.status.code(), &stderr) {
            return Ok(false);
        }
        Err(anyhow!(
            "runtime-support rebuild confirmation failed: {}",
            stderr.trim().if_empty("unknown osascript error")
        ))
    }

    fn show_desktop_notification(title: &str, body: &str) -> Result<()> {
        let script = format!(
            "display notification {} with title {}",
            apple_script_string(body),
            apple_script_string(title)
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()?;
        anyhow::ensure!(status.success(), "failed to show desktop notification");
        Ok(())
    }

    fn workspace_display_name(workspace_cwd: Option<&str>, title: Option<&str>) -> String {
        if let Some(name) = workspace_cwd
            .and_then(|value| std::path::Path::new(value).file_name())
            .and_then(OsStr::to_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return name.to_owned();
        }
        title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| workspace_cwd.map(ToOwned::to_owned))
            .unwrap_or_else(|| "workspace".to_owned())
    }

    fn short_error_message(error: &anyhow::Error) -> String {
        let message = error
            .chain()
            .find_map(|cause| {
                let text = cause.to_string();
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text)
                }
            })
            .unwrap_or_else(|| "unknown error".to_owned());
        truncate_message(&message, 120)
    }

    fn apple_script_string(value: &str) -> String {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }

    fn truncate_message(value: &str, max_chars: usize) -> String {
        if value.chars().count() <= max_chars {
            return value.to_owned();
        }
        let mut truncated = value
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }

    trait IfEmpty {
        fn if_empty(self, fallback: &str) -> String;
    }

    impl IfEmpty for &str {
        fn if_empty(self, fallback: &str) -> String {
            if self.trim().is_empty() {
                fallback.to_owned()
            } else {
                self.to_owned()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            DesktopPollMode, apple_script_user_cancelled, parse_choose_folder_output,
            reconcile_interval, snapshot_poll_mode, workspace_display_name, workspace_tray_label,
        };
        use std::time::Duration;
        use threadbridge_rust::execution_mode::ExecutionMode;
        use threadbridge_rust::management_api::ManagedWorkspaceView;
        use threadbridge_rust::repository::{RecentCodexSessionEntry, RunningInputPolicy};
        use tokio::time::Instant;

        #[test]
        fn parse_choose_folder_output_trims_trailing_slash() {
            assert_eq!(
                parse_choose_folder_output("/tmp/threadBridge/workspaces/Trackly/\n"),
                Some("/tmp/threadBridge/workspaces/Trackly".to_owned())
            );
        }

        #[test]
        fn parse_choose_folder_output_rejects_blank_values() {
            assert_eq!(parse_choose_folder_output(" \n"), None);
        }

        #[test]
        fn workspace_display_name_prefers_folder_name() {
            assert_eq!(
                workspace_display_name(Some("/tmp/threadBridge/workspaces/Trackly"), None),
                "Trackly"
            );
        }

        #[test]
        fn workspace_display_name_falls_back_to_title() {
            assert_eq!(
                workspace_display_name(None, Some("Control Thread")),
                "Control Thread"
            );
        }

        #[test]
        fn workspace_tray_label_uses_workspace_execution_mode() {
            let workspace = ManagedWorkspaceView {
                workspace_cwd: "/tmp/threadBridge/workspaces/Trackly".to_owned(),
                title: Some("查看 TracklyReborn 專案結構".to_owned()),
                thread_key: Some("thread-1".to_owned()),
                running_input_policy: Some(RunningInputPolicy::Reject),
                workspace_execution_mode: ExecutionMode::Yolo,
                current_execution_mode: Some(ExecutionMode::FullAuto),
                current_approval_policy: Some("on-request".to_owned()),
                current_sandbox_policy: Some("workspace-write".to_owned()),
                current_collaboration_mode: None,
                mode_drift: true,
                binding_status: "healthy",
                run_status: "idle",
                run_phase: "idle",
                interrupt_status: "unavailable",
                interrupt_note: None,
                current_codex_thread_id: Some("thr_current".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                last_used_at: None,
                conflict: false,
                app_server_status: "running",
                hcodex_ingress_status: "running",
                runtime_readiness: "ready",
                runtime_health_source: "owner",
                heartbeat_last_checked_at: None,
                heartbeat_last_error: None,
                session_broken_reason: None,
                recovery_hint: None,
                hcodex_path: "/tmp/threadBridge/workspaces/Trackly/.threadbridge/bin/hcodex"
                    .to_owned(),
                hcodex_available: true,
                recent_codex_sessions: Vec::<RecentCodexSessionEntry>::new(),
            };

            assert_eq!(
                workspace_tray_label(&workspace),
                "查看 TracklyReborn 專案結構 · yolo"
            );
        }

        #[test]
        fn apple_script_user_cancelled_detects_standard_error() {
            assert!(apple_script_user_cancelled(
                Some(-128),
                "execution error: User canceled. (-128)"
            ));
        }

        #[test]
        fn snapshot_poll_mode_starts_fast_then_slows_when_idle() {
            let launched_at = Instant::now() - Duration::from_secs(61);
            let snapshot = super::DesktopSnapshot {
                setup: super::DesktopSetupSnapshot {
                    telegram_polling_state:
                        threadbridge_rust::management_api::TelegramPollingState::Active,
                },
                health: super::RuntimeHealthView {
                    management_bind_addr: "127.0.0.1:0".to_owned(),
                    broken_threads: 0,
                    running_workspaces: 0,
                    conflicted_workspaces: 0,
                    ready_workspaces: 1,
                    degraded_workspaces: 0,
                    unavailable_workspaces: 0,
                    app_server_status: "running",
                    hcodex_ingress_status: "running",
                    runtime_readiness: "ready",
                    recovery_hint: None,
                    runtime_owner: threadbridge_rust::runtime_owner::RuntimeOwnerStatus::inactive(),
                    managed_codex: threadbridge_rust::management_api::ManagedCodexView {
                        source: "brew",
                        source_file_path: "source.txt".to_owned(),
                        build_config_file_path: "build-config.json".to_owned(),
                        build_info_file_path: "build-info.txt".to_owned(),
                        binary_path: "codex".to_owned(),
                        binary_ready: true,
                        version: None,
                        build_defaults:
                            threadbridge_rust::management_api::ManagedCodexBuildDefaultsView {
                                source_repo: "repo".to_owned(),
                                source_rs_dir: "rs".to_owned(),
                                build_profile: "dev".to_owned(),
                            },
                        build_info: None,
                    },
                },
                workspaces: vec![ManagedWorkspaceView {
                    workspace_cwd: "/tmp/threadBridge/workspaces/Trackly".to_owned(),
                    title: Some("Trackly".to_owned()),
                    thread_key: Some("thread-1".to_owned()),
                    running_input_policy: Some(RunningInputPolicy::Reject),
                    workspace_execution_mode: ExecutionMode::FullAuto,
                    current_execution_mode: Some(ExecutionMode::FullAuto),
                    current_approval_policy: Some("on-request".to_owned()),
                    current_sandbox_policy: Some("workspace-write".to_owned()),
                    current_collaboration_mode: None,
                    mode_drift: false,
                    binding_status: "healthy",
                    run_status: "idle",
                    run_phase: "idle",
                    interrupt_status: "unavailable",
                    interrupt_note: None,
                    current_codex_thread_id: Some("thr_current".to_owned()),
                    tui_active_codex_thread_id: None,
                    tui_session_adoption_pending: false,
                    last_used_at: None,
                    conflict: false,
                    app_server_status: "running",
                    hcodex_ingress_status: "running",
                    runtime_readiness: "ready",
                    runtime_health_source: "owner",
                    heartbeat_last_checked_at: None,
                    heartbeat_last_error: None,
                    session_broken_reason: None,
                    recovery_hint: None,
                    hcodex_path: "/tmp/threadBridge/workspaces/Trackly/.threadbridge/bin/hcodex"
                        .to_owned(),
                    hcodex_available: true,
                    recent_codex_sessions: Vec::<RecentCodexSessionEntry>::new(),
                }],
            };

            assert_eq!(
                snapshot_poll_mode(Some(&snapshot), launched_at),
                DesktopPollMode::Slow
            );
            assert_eq!(reconcile_interval(Some(&snapshot)), Duration::from_secs(60));
        }

        #[test]
        fn snapshot_poll_mode_stays_fast_when_runtime_is_degraded() {
            let launched_at = Instant::now() - Duration::from_secs(61);
            let mut workspace = ManagedWorkspaceView {
                workspace_cwd: "/tmp/threadBridge/workspaces/Trackly".to_owned(),
                title: Some("Trackly".to_owned()),
                thread_key: Some("thread-1".to_owned()),
                running_input_policy: Some(RunningInputPolicy::Reject),
                workspace_execution_mode: ExecutionMode::FullAuto,
                current_execution_mode: Some(ExecutionMode::FullAuto),
                current_approval_policy: Some("on-request".to_owned()),
                current_sandbox_policy: Some("workspace-write".to_owned()),
                current_collaboration_mode: None,
                mode_drift: false,
                binding_status: "healthy",
                run_status: "idle",
                run_phase: "idle",
                interrupt_status: "unavailable",
                interrupt_note: None,
                current_codex_thread_id: Some("thr_current".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                last_used_at: None,
                conflict: false,
                app_server_status: "running",
                hcodex_ingress_status: "missing",
                runtime_readiness: "degraded",
                runtime_health_source: "owner",
                heartbeat_last_checked_at: None,
                heartbeat_last_error: Some("missing".to_owned()),
                session_broken_reason: None,
                recovery_hint: Some("repair".to_owned()),
                hcodex_path: "/tmp/threadBridge/workspaces/Trackly/.threadbridge/bin/hcodex"
                    .to_owned(),
                hcodex_available: true,
                recent_codex_sessions: Vec::<RecentCodexSessionEntry>::new(),
            };
            let snapshot = super::DesktopSnapshot {
                setup: super::DesktopSetupSnapshot {
                    telegram_polling_state:
                        threadbridge_rust::management_api::TelegramPollingState::Active,
                },
                health: super::RuntimeHealthView {
                    management_bind_addr: "127.0.0.1:0".to_owned(),
                    broken_threads: 0,
                    running_workspaces: 0,
                    conflicted_workspaces: 0,
                    ready_workspaces: 0,
                    degraded_workspaces: 1,
                    unavailable_workspaces: 0,
                    app_server_status: "running",
                    hcodex_ingress_status: "missing",
                    runtime_readiness: "degraded",
                    recovery_hint: None,
                    runtime_owner: threadbridge_rust::runtime_owner::RuntimeOwnerStatus::inactive(),
                    managed_codex: threadbridge_rust::management_api::ManagedCodexView {
                        source: "brew",
                        source_file_path: "source.txt".to_owned(),
                        build_config_file_path: "build-config.json".to_owned(),
                        build_info_file_path: "build-info.txt".to_owned(),
                        binary_path: "codex".to_owned(),
                        binary_ready: true,
                        version: None,
                        build_defaults:
                            threadbridge_rust::management_api::ManagedCodexBuildDefaultsView {
                                source_repo: "repo".to_owned(),
                                source_rs_dir: "rs".to_owned(),
                                build_profile: "dev".to_owned(),
                            },
                        build_info: None,
                    },
                },
                workspaces: vec![workspace.clone()],
            };

            assert_eq!(
                snapshot_poll_mode(Some(&snapshot), launched_at),
                DesktopPollMode::Fast
            );
            assert_eq!(reconcile_interval(Some(&snapshot)), Duration::from_secs(10));

            workspace.run_status = "running";
            let running_snapshot = super::DesktopSnapshot {
                workspaces: vec![workspace],
                ..snapshot
            };
            assert_eq!(
                snapshot_poll_mode(Some(&running_snapshot), launched_at),
                DesktopPollMode::Fast
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos_app::run()
}
