use anyhow::{Result, bail};
use serde::Serialize;

use crate::config::RuntimeConfig;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LaunchAtLoginView {
    pub supported: bool,
    pub enabled: bool,
    pub status: &'static str,
    pub note: Option<String>,
}

impl LaunchAtLoginView {
    fn unsupported() -> Self {
        Self {
            supported: false,
            enabled: false,
            status: "unsupported",
            note: None,
        }
    }
}

pub fn current_view(runtime: &RuntimeConfig) -> LaunchAtLoginView {
    if !runtime.supports_runtime_support_rebuild() {
        return LaunchAtLoginView::unsupported();
    }

    current_view_impl()
}

pub fn set_enabled(runtime: &RuntimeConfig, enabled: bool) -> Result<LaunchAtLoginView> {
    if !runtime.supports_runtime_support_rebuild() {
        bail!("Launch at Login is only available from the bundled desktop app.");
    }

    set_enabled_impl(enabled)?;
    Ok(current_view_impl())
}

#[cfg(target_os = "macos")]
fn current_view_impl() -> LaunchAtLoginView {
    use smappservice_rs::{AppService, ServiceStatus, ServiceType};

    let service = AppService::new(ServiceType::MainApp);
    match service.status() {
        ServiceStatus::Enabled => LaunchAtLoginView {
            supported: true,
            enabled: true,
            status: "enabled",
            note: None,
        },
        ServiceStatus::NotRegistered => LaunchAtLoginView {
            supported: true,
            enabled: false,
            status: "disabled",
            note: None,
        },
        ServiceStatus::RequiresApproval => LaunchAtLoginView {
            supported: true,
            enabled: true,
            status: "requires_approval",
            note: Some(
                "macOS requires approval in System Settings > General > Login Items before threadBridge can launch at login."
                    .to_owned(),
            ),
        },
        ServiceStatus::NotFound => LaunchAtLoginView {
            supported: true,
            enabled: false,
            status: "not_found",
            note: Some(
                "macOS could not find the bundled app service for Launch at Login.".to_owned(),
            ),
        },
    }
}

#[cfg(target_os = "macos")]
fn set_enabled_impl(enabled: bool) -> Result<()> {
    use smappservice_rs::{AppService, ServiceType};

    let service = AppService::new(ServiceType::MainApp);
    if enabled {
        service.register()?;
    } else {
        service.unregister()?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn current_view_impl() -> LaunchAtLoginView {
    LaunchAtLoginView::unsupported()
}

#[cfg(not(target_os = "macos"))]
fn set_enabled_impl(_enabled: bool) -> Result<()> {
    bail!("Launch at Login is only available on macOS.")
}

#[cfg(test)]
mod tests {
    use super::{current_view, set_enabled};
    use crate::config::RuntimeConfig;
    use std::path::PathBuf;

    fn source_tree_runtime() -> RuntimeConfig {
        RuntimeConfig {
            data_root_path: PathBuf::from("/tmp/threadbridge-test/data"),
            debug_log_path: PathBuf::from("/tmp/threadbridge-test/debug.log"),
            runtime_support_root_path: PathBuf::from("/tmp/threadbridge-test/runtime_support"),
            runtime_support_seed_root_path: PathBuf::from("/tmp/threadbridge-test/runtime_support"),
            codex_model: None,
            management_bind_addr: "127.0.0.1:0".parse().unwrap(),
        }
    }

    #[test]
    fn source_tree_runtime_reports_launch_at_login_as_unsupported() {
        let view = current_view(&source_tree_runtime());
        assert!(!view.supported);
        assert!(!view.enabled);
        assert_eq!(view.status, "unsupported");
        assert_eq!(view.note, None);
    }

    #[test]
    fn source_tree_runtime_rejects_launch_at_login_mutation() {
        let error = set_enabled(&source_tree_runtime(), true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Launch at Login is only available from the bundled desktop app")
        );
    }
}
