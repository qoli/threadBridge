use std::ffi::OsString;
use std::path::PathBuf;

use threadbridge_rust::app_server_runtime::WorkspaceRuntimeManager;
use threadbridge_rust::config::RuntimeConfig;
use threadbridge_rust::runtime_control::{RuntimeControlContext, RuntimeOwnershipMode};
use threadbridge_rust::workspace::ensure_workspace_runtime;
use tokio::fs;
use uuid::Uuid;

fn temp_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "threadbridge-runtime-control-owner-managed-test-{}",
        Uuid::new_v4()
    ))
}

fn runtime_config(root: &PathBuf) -> RuntimeConfig {
    RuntimeConfig {
        data_root_path: root.join("data"),
        debug_log_path: root.join("debug.log"),
        runtime_assets_root_path: root.join("runtime_assets"),
        runtime_assets_seed_root_path: root.join("runtime_assets"),
        codex_model: None,
        management_bind_addr: "127.0.0.1:0".parse().unwrap(),
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.as_ref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[tokio::test]
async fn owner_managed_control_runtime_bootstraps_missing_state_file() {
    let root = temp_path();
    fs::create_dir_all(&root).await.unwrap();

    let runtime = runtime_config(&root);
    let template_dir = runtime.runtime_assets_root_path.join("templates");
    fs::create_dir_all(&template_dir).await.unwrap();
    let seed_template_path = template_dir.join("AGENTS.md");
    fs::write(&seed_template_path, "test template")
        .await
        .unwrap();

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).await.unwrap();
    ensure_workspace_runtime(
        &runtime.runtime_assets_root_path,
        &runtime.data_root_path,
        &seed_template_path,
        &workspace,
    )
    .await
    .unwrap();

    let state_path = workspace.join(".threadbridge/state/app-server/current.json");
    assert!(!state_path.exists());

    let _worker_bin = EnvVarGuard::set(
        "THREADBRIDGE_APP_SERVER_WS_WORKER_BIN",
        env!("CARGO_BIN_EXE_app_server_ws_worker"),
    );

    let control = RuntimeControlContext::new(
        runtime.clone(),
        WorkspaceRuntimeManager::new_with_data_root(runtime.data_root_path.clone()),
        None,
        RuntimeOwnershipMode::DesktopOwner,
    )
    .await
    .unwrap();

    let codex_workspace = control
        .workspace_runtime_service()
        .prepare_workspace_runtime_for_control(workspace.clone())
        .await
        .unwrap();

    assert_eq!(codex_workspace.working_directory, workspace);
    assert!(codex_workspace.app_server_url.is_some());
    assert!(state_path.exists());

    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn owner_managed_bind_workspace_record_creates_session_binding() {
    let root = temp_path();
    fs::create_dir_all(&root).await.unwrap();

    let runtime = runtime_config(&root);
    let template_dir = runtime.runtime_assets_root_path.join("templates");
    fs::create_dir_all(&template_dir).await.unwrap();
    let seed_template_path = template_dir.join("AGENTS.md");
    fs::write(&seed_template_path, "test template")
        .await
        .unwrap();

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).await.unwrap();

    let _worker_bin = EnvVarGuard::set(
        "THREADBRIDGE_APP_SERVER_WS_WORKER_BIN",
        env!("CARGO_BIN_EXE_app_server_ws_worker"),
    );

    let control = RuntimeControlContext::new(
        runtime.clone(),
        WorkspaceRuntimeManager::new_with_data_root(runtime.data_root_path.clone()),
        None,
        RuntimeOwnershipMode::DesktopOwner,
    )
    .await
    .unwrap();

    let record = control
        .repository
        .create_thread(1, 7, "Workspace".to_owned())
        .await
        .unwrap();
    let updated = control
        .workspace_session_service()
        .bind_workspace_record(record, &workspace)
        .await
        .unwrap();
    let binding = control
        .repository
        .read_session_binding(&updated)
        .await
        .unwrap()
        .unwrap();

    let bound_workspace = PathBuf::from(binding.workspace_cwd.as_deref().unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(bound_workspace, workspace.canonicalize().unwrap());
    assert!(binding.current_codex_thread_id.is_some());

    let _ = fs::remove_dir_all(root).await;
}
