use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;

const WORKSPACE_EXECUTION_CONFIG_SCHEMA_VERSION: u32 = 1;
const WORKSPACE_EXECUTION_CONFIG_RELATIVE_PATH: &str = ".threadbridge/state/workspace-config.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    FullAuto,
    Yolo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceExecutionConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionExecutionSnapshot {
    #[serde(default)]
    pub execution_mode: Option<ExecutionMode>,
    #[serde(default)]
    pub approval_policy: Option<String>,
    #[serde(default)]
    pub sandbox_policy: Option<String>,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullAuto => "full_auto",
            Self::Yolo => "yolo",
        }
    }

    pub fn approval_policy(self) -> &'static str {
        match self {
            Self::FullAuto => "on-request",
            Self::Yolo => "never",
        }
    }

    pub fn sandbox_mode(self) -> &'static str {
        match self {
            Self::FullAuto => "workspace-write",
            Self::Yolo => "danger-full-access",
        }
    }

    pub fn hcodex_flag(self) -> &'static str {
        match self {
            Self::FullAuto => "--full-auto",
            Self::Yolo => "--dangerously-bypass-approvals-and-sandbox",
        }
    }

    pub fn from_policies(approval_policy: &str, sandbox_policy: &str) -> Option<Self> {
        match (approval_policy, sandbox_policy) {
            ("on-request", "workspace-write") => Some(Self::FullAuto),
            ("never", "danger-full-access") => Some(Self::Yolo),
            _ => None,
        }
    }
}

impl WorkspaceExecutionConfig {
    pub fn new(execution_mode: ExecutionMode) -> Self {
        Self {
            schema_version: WORKSPACE_EXECUTION_CONFIG_SCHEMA_VERSION,
            execution_mode,
            updated_at: now_iso(),
        }
    }
}

impl SessionExecutionSnapshot {
    pub fn from_mode(execution_mode: ExecutionMode) -> Self {
        Self {
            execution_mode: Some(execution_mode),
            approval_policy: Some(execution_mode.approval_policy().to_owned()),
            sandbox_policy: Some(execution_mode.sandbox_mode().to_owned()),
        }
    }

    pub fn from_thread_result(result: &Value) -> Self {
        let approval_policy = result
            .get("approvalPolicy")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let sandbox_policy = result.get("sandbox").and_then(normalize_sandbox_policy);
        let execution_mode = approval_policy
            .as_deref()
            .zip(sandbox_policy.as_deref())
            .and_then(|(approval_policy, sandbox_policy)| {
                ExecutionMode::from_policies(approval_policy, sandbox_policy)
            });
        Self {
            execution_mode,
            approval_policy,
            sandbox_policy,
        }
    }
}

pub fn workspace_execution_config_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(WORKSPACE_EXECUTION_CONFIG_RELATIVE_PATH)
}

pub async fn read_workspace_execution_config(
    workspace_path: &Path,
) -> Result<Option<WorkspaceExecutionConfig>> {
    let path = workspace_execution_config_path(workspace_path);
    match fs::read_to_string(&path).await {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub async fn ensure_workspace_execution_config(
    workspace_path: &Path,
) -> Result<WorkspaceExecutionConfig> {
    match read_workspace_execution_config(workspace_path).await? {
        Some(config) => Ok(config),
        None => write_workspace_execution_config(workspace_path, ExecutionMode::FullAuto).await,
    }
}

pub async fn workspace_execution_mode(workspace_path: &Path) -> Result<ExecutionMode> {
    Ok(ensure_workspace_execution_config(workspace_path)
        .await?
        .execution_mode)
}

pub async fn write_workspace_execution_config(
    workspace_path: &Path,
    execution_mode: ExecutionMode,
) -> Result<WorkspaceExecutionConfig> {
    let config = WorkspaceExecutionConfig::new(execution_mode);
    let path = workspace_execution_config_path(workspace_path);
    let parent = path
        .parent()
        .context("workspace execution config path missing parent")?;
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&config)?),
    )
    .await
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(config)
}

fn normalize_sandbox_policy(value: &Value) -> Option<String> {
    if let Some(policy) = value.as_str() {
        return Some(policy.to_owned());
    }
    match value.get("type").and_then(Value::as_str) {
        Some("workspaceWrite") => Some("workspace-write".to_owned()),
        Some("dangerFullAccess") => Some("danger-full-access".to_owned()),
        Some("readOnly") => Some("read-only".to_owned()),
        Some("externalSandbox") => Some("external-sandbox".to_owned()),
        Some(other) => Some(other.to_owned()),
        None => None,
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
