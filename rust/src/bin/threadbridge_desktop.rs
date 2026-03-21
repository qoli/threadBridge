#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("threadbridge_desktop is currently implemented for macOS only")
}

#[cfg(target_os = "macos")]
mod macos_app {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use tao::event::{Event, StartCause, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
    use tao::window::{Window, WindowBuilder};
    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use tracing::{info, warn};
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
    use wry::{WebView, WebViewBuilder};

    use threadbridge_rust::bot_runner::spawn_bot_runtime_from_env_with_runtimes;
    use threadbridge_rust::config::load_runtime_config;
    use threadbridge_rust::hcodex_runtime;
    use threadbridge_rust::hcodex_ws_bridge;
    use threadbridge_rust::logging::init_json_logs;
    use threadbridge_rust::management_api::{
        ManagedWorkspaceView, ManagementApiHandle, RuntimeHealthView, SetupStateView,
        spawn_management_api,
    };
    use threadbridge_rust::runtime_owner::DesktopRuntimeOwner;

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
        setup: SetupStateView,
        health: RuntimeHealthView,
        workspaces: Vec<ManagedWorkspaceView>,
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
        launch_ready: bool,
        recent_session_ids: Vec<String>,
    }

    #[derive(Debug, Clone)]
    enum TrayAction {
        OpenSettings,
        Quit,
        LaunchNew {
            thread_key: String,
        },
        Resume {
            thread_key: String,
            session_id: String,
        },
    }

    struct MenuModel {
        menu: Menu,
        actions: HashMap<MenuId, TrayAction>,
    }

    struct SettingsWindow {
        window: Window,
        _webview: WebView,
    }

    struct DesktopApp {
        runtime: Arc<Runtime>,
        management_api: ManagementApiHandle,
        owner: Arc<DesktopRuntimeOwner>,
        tray_icon: Option<TrayIcon>,
        tray_actions: HashMap<MenuId, TrayAction>,
        settings_window: Option<SettingsWindow>,
        latest_snapshot: Option<DesktopSnapshot>,
        latest_tray_signature: Option<TraySnapshotSignature>,
    }

    pub fn run() -> Result<()> {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        let runtime = Arc::new(
            RuntimeBuilder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime"),
        );
        if runtime.block_on(hcodex_runtime::maybe_run_from_args(args.clone()))? {
            return Ok(());
        }
        if runtime.block_on(hcodex_ws_bridge::maybe_run_from_args(args))? {
            return Ok(());
        }

        let runtime_config = load_runtime_config()?;
        let _guard = init_json_logs(&runtime_config.debug_log_path)?;
        let management_api = runtime.block_on(spawn_management_api(runtime_config.clone()))?;
        let owner = Arc::new(runtime.block_on(DesktopRuntimeOwner::new(runtime_config.clone()))?);
        runtime.block_on(management_api.set_runtime_owner(Some((*owner).clone())));
        runtime.block_on(reconcile_runtime_owner(&management_api, &owner));

        if runtime
            .block_on(spawn_bot_runtime_from_env_with_runtimes(
                management_api.clone(),
                owner.app_server_runtime(),
                owner.tui_proxy_runtime(),
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

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
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
        let _ = proxy.send_event(UserEvent::Refresh);

        let mut app = DesktopApp {
            runtime,
            management_api,
            owner,
            tray_icon: None,
            tray_actions: HashMap::new(),
            settings_window: None,
            latest_snapshot: None,
            latest_tray_signature: None,
        };

        event_loop.run(move |event, event_loop_window_target, control_flow| {
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
                    if let Err(error) = show_settings_window(&mut app, event_loop_window_target) {
                        warn!(event = "desktop_runtime.settings.open_failed", error = %error);
                    }
                }
                Event::UserEvent(UserEvent::Quit) => {
                    *control_flow = ControlFlow::Exit;
                }
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    window_id,
                    ..
                } => {
                    if let Some(settings) = app.settings_window.as_ref()
                        && settings.window.id() == window_id
                    {
                        settings.window.set_visible(false);
                    }
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
            .build()?;
        Ok(tray_icon)
    }

    fn build_icon() -> Result<Icon> {
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16 {
            for x in 0..16 {
                let accent = x > 2 && x < 13 && y > 2 && y < 13;
                let color = if accent {
                    [155, 77, 34, 255]
                } else {
                    [244, 239, 230, 255]
                };
                rgba.extend_from_slice(&color);
            }
        }
        Ok(Icon::from_rgba(rgba, 16, 16)?)
    }

    fn spawn_snapshot_poller(
        runtime: Arc<Runtime>,
        management_api: ManagementApiHandle,
        owner: Arc<DesktopRuntimeOwner>,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        runtime.spawn(async move {
            loop {
                reconcile_runtime_owner(&management_api, &owner).await;
                maybe_start_bot_runtime(&management_api, &owner).await;
                send_snapshot(&management_api, &proxy).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
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

    async fn send_snapshot(
        management_api: &ManagementApiHandle,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let snapshot = async {
            Ok::<_, anyhow::Error>(DesktopSnapshot {
                setup: management_api.setup_state().await?,
                health: management_api.runtime_health().await?,
                workspaces: management_api.workspace_views().await?,
            })
        }
        .await;
        match snapshot {
            Ok(snapshot) => {
                let _ = proxy.send_event(UserEvent::Snapshot(Box::new(snapshot)));
            }
            Err(error) => {
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
        if let Err(error) = spawn_bot_runtime_from_env_with_runtimes(
            management_api.clone(),
            owner.app_server_runtime(),
            owner.tui_proxy_runtime(),
        )
        .await
        {
            warn!(event = "desktop_runtime.bot.auto_start_failed", error = %error);
        }
    }

    async fn reconcile_runtime_owner(
        management_api: &ManagementApiHandle,
        owner: &DesktopRuntimeOwner,
    ) {
        let Ok(workspaces) = management_api.workspace_views().await else {
            return;
        };
        let targets = workspaces
            .into_iter()
            .filter(|workspace| !workspace.conflict)
            .map(|workspace| workspace.workspace_cwd)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        if let Err(error) = owner.reconcile_managed_workspaces(targets).await {
            warn!(
                event = "desktop_runtime.owner.reconcile.failed",
                error = %error,
                "desktop runtime owner reconciliation failed"
            );
        }
    }

    fn update_tray_snapshot(app: &mut DesktopApp, snapshot: &DesktopSnapshot) -> Result<()> {
        let signature = build_tray_snapshot_signature(snapshot);
        if app.tray_icon.is_some() && app.latest_tray_signature.as_ref() == Some(&signature) {
            return Ok(());
        }
        let model = build_menu_model(snapshot)?;
        if let Some(tray_icon) = app.tray_icon.as_ref() {
            tray_icon.set_menu(Some(Box::new(model.menu)));
            tray_icon.set_tooltip(Some(&signature.tooltip))?;
        }
        app.tray_actions = model.actions;
        app.latest_tray_signature = Some(signature);
        Ok(())
    }

    fn build_menu_model(snapshot: &DesktopSnapshot) -> Result<MenuModel> {
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
            let launch_ready = workspace_launch_ready(workspace);
            let start_item = MenuItem::new("Start New hcodex Session", launch_ready, None);
            actions.insert(
                start_item.id().clone(),
                TrayAction::LaunchNew {
                    thread_key: thread_key.clone(),
                },
            );
            submenu.append_items(&[&start_item, &PredefinedMenuItem::separator()])?;
            if workspace.recent_codex_sessions.is_empty() {
                let empty_item = MenuItem::new("No recent sessions", false, None);
                submenu.append(&empty_item)?;
            } else {
                for session in workspace.recent_codex_sessions.iter().take(5) {
                    let item = MenuItem::new(session.session_id.clone(), launch_ready, None);
                    actions.insert(
                        item.id().clone(),
                        TrayAction::Resume {
                            thread_key: thread_key.clone(),
                            session_id: session.session_id.clone(),
                        },
                    );
                    submenu.append(&item)?;
                }
            }
            menu.append(&submenu)?;
        }
        if !snapshot.workspaces.is_empty() {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        let settings = MenuItem::new("Settings", true, None);
        actions.insert(settings.id().clone(), TrayAction::OpenSettings);
        let quit = MenuItem::new("Quit", true, None);
        actions.insert(quit.id().clone(), TrayAction::Quit);
        menu.append_items(&[&settings, &quit])?;
        Ok(MenuModel { menu, actions })
    }

    fn build_tray_snapshot_signature(snapshot: &DesktopSnapshot) -> TraySnapshotSignature {
        TraySnapshotSignature {
            tooltip: format!(
                "threadBridge | polling {:?} | running {} | broken {} | conflicted {}",
                snapshot.setup.telegram_polling_state,
                snapshot.health.running_workspaces,
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
                        launch_ready: workspace_launch_ready(workspace),
                        recent_session_ids: workspace
                            .recent_codex_sessions
                            .iter()
                            .take(5)
                            .map(|session| session.session_id.clone())
                            .collect(),
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
        let heartbeat = match (workspace.app_server_status, workspace.handoff_readiness) {
            ("running", "ready") => "ready",
            ("running", "degraded") | ("running", "pending_adoption") => "degraded",
            ("running", other) => other,
            (app, _) => app,
        };
        format!("{title} · {heartbeat}")
    }

    fn workspace_launch_ready(workspace: &ManagedWorkspaceView) -> bool {
        workspace.hcodex_available
            && workspace.app_server_status == "running"
            && !matches!(workspace.handoff_readiness, "unavailable")
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
            TrayAction::Quit => {
                let _ = proxy.send_event(UserEvent::Quit);
                *control_flow = ControlFlow::Exit;
            }
            TrayAction::LaunchNew { thread_key } => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = management_api.launch_workspace_new(&thread_key).await {
                        warn!(
                            event = "desktop_runtime.launch_new.failed",
                            error = %error,
                            thread_key = %thread_key
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
            TrayAction::Resume {
                thread_key,
                session_id,
            } => {
                let runtime = app.runtime.clone();
                let management_api = app.management_api.clone();
                let proxy = proxy.clone();
                runtime.spawn(async move {
                    if let Err(error) = management_api
                        .launch_workspace_resume(&thread_key, &session_id)
                        .await
                    {
                        warn!(
                            event = "desktop_runtime.launch_resume.failed",
                            error = %error,
                            thread_key = %thread_key,
                            session_id = %session_id
                        );
                    }
                    let _ = proxy.send_event(UserEvent::Refresh);
                });
            }
        }
    }

    fn show_settings_window(
        app: &mut DesktopApp,
        event_loop_window_target: &tao::event_loop::EventLoopWindowTarget<UserEvent>,
    ) -> Result<()> {
        if let Some(settings) = app.settings_window.as_ref() {
            settings.window.set_visible(true);
            settings.window.set_focus();
            return Ok(());
        }
        let window = WindowBuilder::new()
            .with_title("threadBridge Settings")
            .with_inner_size(tao::dpi::LogicalSize::new(1080.0, 760.0))
            .build(event_loop_window_target)?;
        let webview = WebViewBuilder::new()
            .with_url(&app.management_api.base_url)
            .build(&window)?;
        app.settings_window = Some(SettingsWindow {
            window,
            _webview: webview,
        });
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos_app::run()
}
