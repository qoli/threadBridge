use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingApprovalSourceKind {
    Direct,
    Tui,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalNetworkPermissions {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalFileSystemPermissions {
    #[serde(default)]
    pub read: Option<Vec<String>>,
    #[serde(default)]
    pub write: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfile {
    #[serde(default)]
    pub network: Option<AdditionalNetworkPermissions>,
    #[serde(default)]
    pub file_system: Option<AdditionalFileSystemPermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub network_approval_context: Option<NetworkApprovalContext>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub command_actions: Option<Vec<Value>>,
    #[serde(default)]
    pub additional_permissions: Option<PermissionProfile>,
    #[serde(default)]
    pub proposed_execpolicy_amendment: Option<Value>,
    #[serde(default)]
    pub proposed_network_policy_amendments: Option<Vec<Value>>,
    #[serde(default)]
    pub available_decisions: Option<Vec<CommandExecutionApprovalDecision>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub grant_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub permissions: PermissionProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecPolicyAmendmentDecision {
    pub execpolicy_amendment: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyAmendmentDecision {
    pub network_policy_amendment: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum CommandExecutionApprovalDecision {
    #[serde(rename_all = "camelCase")]
    AcceptWithExecpolicyAmendment {
        accept_with_execpolicy_amendment: ExecPolicyAmendmentDecision,
    },
    #[serde(rename_all = "camelCase")]
    ApplyNetworkPolicyAmendment {
        apply_network_policy_amendment: NetworkPolicyAmendmentDecision,
    },
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileChangeApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PermissionGrantScope {
    #[default]
    #[serde(rename = "turn")]
    Turn,
    #[serde(rename = "session")]
    Session,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingApprovalKind {
    CommandExecution,
    FileChange,
    Permissions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionOption {
    pub token: String,
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub management_only: bool,
    #[serde(default)]
    pub telegram_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingApprovalPayload {
    CommandExecution {
        #[serde(flatten)]
        params: CommandExecutionRequestApprovalParams,
    },
    FileChange {
        #[serde(flatten)]
        params: FileChangeRequestApprovalParams,
    },
    Permissions {
        #[serde(flatten)]
        params: PermissionsRequestApprovalParams,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingApprovalView {
    pub approval_key: String,
    pub request_id: i64,
    pub thread_key: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub kind: PendingApprovalKind,
    pub source: PendingApprovalSourceKind,
    pub created_at: String,
    pub decision_options: Vec<ApprovalDecisionOption>,
    pub payload: PendingApprovalPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPermissionsSubsetRequest {
    pub permissions: PermissionProfile,
    #[serde(default)]
    pub scope: PermissionGrantScope,
}

#[derive(Debug)]
enum PendingApprovalResponder {
    Direct(oneshot::Sender<Value>),
    Tui,
}

#[derive(Debug)]
struct PendingApproval {
    approval_key: String,
    request_id: i64,
    thread_key: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
    kind: PendingApprovalKind,
    source: PendingApprovalSourceKind,
    created_at: String,
    payload: PendingApprovalPayload,
    decision_options: Vec<ApprovalDecisionOption>,
    decision_payloads: HashMap<String, Value>,
    prompt_message_id: Option<i32>,
    responder: PendingApprovalResponder,
}

#[derive(Debug, Clone)]
pub struct ApprovalResolution {
    pub approval_key: String,
    pub thread_key: String,
    pub thread_id: String,
    pub request_id: i64,
    pub response: Value,
    pub prompt_message_id: Option<i32>,
    pub requires_runtime_forward: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedApprovalRequest {
    pub approval_key: String,
    pub thread_key: String,
    pub prompt_message_id: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ApprovalRegistration {
    pub approval_key: String,
    pub view: PendingApprovalView,
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalRequestRegistry {
    inner: Arc<Mutex<HashMap<String, PendingApproval>>>,
}

impl ApprovalRequestRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register_direct(
        &self,
        thread_key: String,
        request_id: i64,
        payload: PendingApprovalPayload,
        responder: oneshot::Sender<Value>,
    ) -> Result<ApprovalRegistration> {
        self.register(
            thread_key,
            request_id,
            payload,
            PendingApprovalResponder::Direct(responder),
            PendingApprovalSourceKind::Direct,
        )
        .await
    }

    pub async fn register_tui(
        &self,
        thread_key: String,
        request_id: i64,
        payload: PendingApprovalPayload,
    ) -> Result<ApprovalRegistration> {
        self.register(
            thread_key,
            request_id,
            payload,
            PendingApprovalResponder::Tui,
            PendingApprovalSourceKind::Tui,
        )
        .await
    }

    async fn register(
        &self,
        thread_key: String,
        request_id: i64,
        payload: PendingApprovalPayload,
        responder: PendingApprovalResponder,
        source: PendingApprovalSourceKind,
    ) -> Result<ApprovalRegistration> {
        let (thread_id, turn_id, item_id, kind) = payload_identity(&payload);
        let created_at = now_iso();
        let approval_key = format!("ap_{}", short_token());
        let (decision_options, decision_payloads) = build_decision_state(&payload)?;
        let pending = PendingApproval {
            approval_key: approval_key.clone(),
            request_id,
            thread_key: thread_key.clone(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            item_id: item_id.to_owned(),
            kind,
            source,
            created_at: created_at.clone(),
            payload: payload.clone(),
            decision_options,
            decision_payloads,
            prompt_message_id: None,
            responder,
        };
        let view = pending.view();
        self.inner.lock().await.insert(approval_key.clone(), pending);
        Ok(ApprovalRegistration { approval_key, view })
    }

    pub async fn list_views(&self) -> Vec<PendingApprovalView> {
        let mut views = self
            .inner
            .lock()
            .await
            .values()
            .map(PendingApproval::view)
            .collect::<Vec<_>>();
        views.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        views
    }

    pub async fn get_view(&self, approval_key: &str) -> Option<PendingApprovalView> {
        self.inner.lock().await.get(approval_key).map(PendingApproval::view)
    }

    pub async fn set_prompt_message_id(&self, approval_key: &str, message_id: i32) {
        if let Some(pending) = self.inner.lock().await.get_mut(approval_key) {
            pending.prompt_message_id = Some(message_id);
        }
    }

    pub async fn resolve_preset(
        &self,
        approval_key: &str,
        token: &str,
    ) -> Result<Option<ApprovalResolution>> {
        let mut inner = self.inner.lock().await;
        let Some(pending) = inner.remove(approval_key) else {
            return Ok(None);
        };
        let response = pending
            .decision_payloads
            .get(token)
            .cloned()
            .with_context(|| format!("unknown approval decision token `{token}`"))?;
        Ok(Some(finalize_resolution(pending, response)))
    }

    pub async fn resolve_permissions_subset(
        &self,
        approval_key: &str,
        request: SubmitPermissionsSubsetRequest,
    ) -> Result<Option<ApprovalResolution>> {
        let mut inner = self.inner.lock().await;
        let Some(pending) = inner.remove(approval_key) else {
            return Ok(None);
        };
        let PendingApprovalPayload::Permissions { params } = &pending.payload else {
            bail!("permission subset decisions only apply to permissions approvals");
        };
        validate_permission_subset(&params.permissions, &request.permissions)?;
        let response = json!({
            "permissions": request.permissions,
            "scope": request.scope,
        });
        Ok(Some(finalize_resolution(pending, response)))
    }

    pub async fn resolve_request_id(
        &self,
        thread_id: &str,
        request_id: &Value,
    ) -> Option<ResolvedApprovalRequest> {
        let request_id = request_id.as_i64()?;
        let mut inner = self.inner.lock().await;
        let approval_key = inner.iter().find_map(|(key, pending)| {
            (pending.thread_id == thread_id && pending.request_id == request_id)
                .then(|| key.clone())
        })?;
        inner.remove(&approval_key).map(|pending| ResolvedApprovalRequest {
            approval_key: pending.approval_key,
            thread_key: pending.thread_key,
            prompt_message_id: pending.prompt_message_id,
        })
    }
}

impl PendingApproval {
    fn view(&self) -> PendingApprovalView {
        PendingApprovalView {
            approval_key: self.approval_key.clone(),
            request_id: self.request_id,
            thread_key: self.thread_key.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            item_id: self.item_id.clone(),
            kind: self.kind,
            source: self.source,
            created_at: self.created_at.clone(),
            decision_options: self.decision_options.clone(),
            payload: self.payload.clone(),
        }
    }
}

fn finalize_resolution(pending: PendingApproval, response: Value) -> ApprovalResolution {
    let requires_runtime_forward = matches!(pending.responder, PendingApprovalResponder::Tui);
    if let PendingApprovalResponder::Direct(responder) = pending.responder {
        let _ = responder.send(response.clone());
    }
    ApprovalResolution {
        approval_key: pending.approval_key,
        thread_key: pending.thread_key,
        thread_id: pending.thread_id,
        request_id: pending.request_id,
        response,
        prompt_message_id: pending.prompt_message_id,
        requires_runtime_forward,
    }
}

fn payload_identity(payload: &PendingApprovalPayload) -> (&str, &str, &str, PendingApprovalKind) {
    match payload {
        PendingApprovalPayload::CommandExecution { params } => (
            &params.thread_id,
            &params.turn_id,
            &params.item_id,
            PendingApprovalKind::CommandExecution,
        ),
        PendingApprovalPayload::FileChange { params } => (
            &params.thread_id,
            &params.turn_id,
            &params.item_id,
            PendingApprovalKind::FileChange,
        ),
        PendingApprovalPayload::Permissions { params } => (
            &params.thread_id,
            &params.turn_id,
            &params.item_id,
            PendingApprovalKind::Permissions,
        ),
    }
}

fn build_decision_state(
    payload: &PendingApprovalPayload,
) -> Result<(Vec<ApprovalDecisionOption>, HashMap<String, Value>)> {
    let mut decision_payloads = HashMap::new();
    let mut decision_options = Vec::new();

    let mut push = |label: &str, description: &str, management_only: bool, telegram_supported: bool, response: Value| {
        let token = short_token();
        decision_payloads.insert(token.clone(), response);
        decision_options.push(ApprovalDecisionOption {
            token,
            label: label.to_owned(),
            description: description.to_owned(),
            management_only,
            telegram_supported,
        });
    };

    match payload {
        PendingApprovalPayload::CommandExecution { params } => {
            let decisions = params.available_decisions.clone().unwrap_or_else(|| {
                vec![
                    CommandExecutionApprovalDecision::Accept,
                    CommandExecutionApprovalDecision::AcceptForSession,
                    CommandExecutionApprovalDecision::Decline,
                    CommandExecutionApprovalDecision::Cancel,
                ]
            });
            for decision in decisions {
                match &decision {
                    CommandExecutionApprovalDecision::Accept => push(
                        "Approve Once",
                        "Allow this command execution for the current request.",
                        false,
                        true,
                        json!({ "decision": decision }),
                    ),
                    CommandExecutionApprovalDecision::AcceptForSession => push(
                        "Approve Session",
                        "Allow this command and similar requests for the rest of the session.",
                        false,
                        true,
                        json!({ "decision": decision }),
                    ),
                    CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment { .. } => push(
                        "Allow Similar Commands",
                        "Apply the proposed execpolicy amendment and continue.",
                        false,
                        true,
                        json!({ "decision": decision }),
                    ),
                    CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                        apply_network_policy_amendment,
                    } => {
                        let host = apply_network_policy_amendment
                            .network_policy_amendment
                            .get("host")
                            .and_then(Value::as_str)
                            .unwrap_or("host");
                        push(
                            "Allow Network Rule",
                            &format!("Apply the proposed network policy amendment for {host}."),
                            false,
                            true,
                            json!({ "decision": decision }),
                        );
                    }
                    CommandExecutionApprovalDecision::Decline => push(
                        "Reject",
                        "Decline this command execution request.",
                        false,
                        true,
                        json!({ "decision": decision }),
                    ),
                    CommandExecutionApprovalDecision::Cancel => push(
                        "Cancel",
                        "Cancel this approval request without approving it.",
                        false,
                        true,
                        json!({ "decision": decision }),
                    ),
                }
            }
        }
        PendingApprovalPayload::FileChange { .. } => {
            for (label, description, decision) in [
                (
                    "Approve Once",
                    "Apply this file change for the current request.",
                    FileChangeApprovalDecision::Accept,
                ),
                (
                    "Approve Session",
                    "Allow this file change type for the rest of the session.",
                    FileChangeApprovalDecision::AcceptForSession,
                ),
                (
                    "Reject",
                    "Decline this file change request.",
                    FileChangeApprovalDecision::Decline,
                ),
                (
                    "Cancel",
                    "Cancel this file change approval request.",
                    FileChangeApprovalDecision::Cancel,
                ),
            ] {
                push(
                    label,
                    description,
                    false,
                    true,
                    json!({ "decision": decision }),
                );
            }
        }
        PendingApprovalPayload::Permissions { params } => {
            push(
                "Approve Once",
                "Grant all requested permissions for the current turn.",
                false,
                true,
                json!({
                    "permissions": params.permissions,
                    "scope": PermissionGrantScope::Turn,
                }),
            );
            push(
                "Approve Session",
                "Grant all requested permissions for the rest of the session.",
                false,
                true,
                json!({
                    "permissions": params.permissions,
                    "scope": PermissionGrantScope::Session,
                }),
            );
            push(
                "Reject",
                "Deny the requested permissions.",
                false,
                true,
                json!({
                    "permissions": PermissionProfile::default(),
                    "scope": PermissionGrantScope::Turn,
                }),
            );
        }
    }

    Ok((decision_options, decision_payloads))
}

fn validate_permission_subset(
    requested: &PermissionProfile,
    granted: &PermissionProfile,
) -> Result<()> {
    validate_network_subset(requested.network.as_ref(), granted.network.as_ref())?;
    validate_file_system_subset(requested.file_system.as_ref(), granted.file_system.as_ref())?;
    Ok(())
}

fn validate_network_subset(
    requested: Option<&AdditionalNetworkPermissions>,
    granted: Option<&AdditionalNetworkPermissions>,
) -> Result<()> {
    let Some(granted) = granted else {
        return Ok(());
    };
    let Some(requested) = requested else {
        bail!("granted network permissions were not requested");
    };
    if let Some(enabled) = granted.enabled {
        let requested_enabled = requested
            .enabled
            .ok_or_else(|| anyhow!("granted network permission was not requested"))?;
        if enabled != requested_enabled {
            bail!("granted network permission must match the requested value");
        }
    }
    Ok(())
}

fn validate_file_system_subset(
    requested: Option<&AdditionalFileSystemPermissions>,
    granted: Option<&AdditionalFileSystemPermissions>,
) -> Result<()> {
    let Some(granted) = granted else {
        return Ok(());
    };
    let Some(requested) = requested else {
        bail!("granted filesystem permissions were not requested");
    };
    validate_path_subset("read", requested.read.as_ref(), granted.read.as_ref())?;
    validate_path_subset("write", requested.write.as_ref(), granted.write.as_ref())?;
    Ok(())
}

fn validate_path_subset(
    label: &str,
    requested: Option<&Vec<String>>,
    granted: Option<&Vec<String>>,
) -> Result<()> {
    let Some(granted) = granted else {
        return Ok(());
    };
    let requested = requested
        .ok_or_else(|| anyhow!("granted filesystem {label} permissions were not requested"))?;
    let requested_set = requested.iter().collect::<BTreeSet<_>>();
    for path in granted {
        if !requested_set.contains(path) {
            bail!("granted filesystem {label} permission `{path}` was not requested");
        }
    }
    Ok(())
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn short_token() -> String {
    Uuid::new_v4().simple().to_string()[..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        AdditionalFileSystemPermissions, ApprovalRequestRegistry, PendingApprovalPayload,
        PermissionGrantScope, PermissionProfile, PermissionsRequestApprovalParams,
        SubmitPermissionsSubsetRequest,
    };
    use serde_json::json;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn direct_resolution_sends_response_without_runtime_forward() {
        let registry = ApprovalRequestRegistry::new();
        let (tx, rx) = oneshot::channel();
        let registration = registry
            .register_direct(
                "thread-1".to_owned(),
                7,
                PendingApprovalPayload::Permissions {
                    params: PermissionsRequestApprovalParams {
                        thread_id: "thr_1".to_owned(),
                        turn_id: "turn_1".to_owned(),
                        item_id: "call_1".to_owned(),
                        reason: Some("Need write access".to_owned()),
                        permissions: PermissionProfile {
                            network: None,
                            file_system: Some(AdditionalFileSystemPermissions {
                                read: None,
                                write: Some(vec!["/tmp/a".to_owned()]),
                            }),
                        },
                    },
                },
                tx,
            )
            .await
            .expect("register");

        let option = registration
            .view
            .decision_options
            .iter()
            .find(|option| option.label == "Approve Once")
            .expect("approve option");
        let resolution = registry
            .resolve_preset(&registration.approval_key, &option.token)
            .await
            .expect("resolve")
            .expect("pending");
        assert!(!resolution.requires_runtime_forward);
        let response = rx.await.expect("response");
        assert_eq!(response, resolution.response);
    }

    #[tokio::test]
    async fn permission_subset_must_be_requested_subset() {
        let registry = ApprovalRequestRegistry::new();
        let registration = registry
            .register_tui(
                "thread-1".to_owned(),
                9,
                PendingApprovalPayload::Permissions {
                    params: PermissionsRequestApprovalParams {
                        thread_id: "thr_1".to_owned(),
                        turn_id: "turn_1".to_owned(),
                        item_id: "call_1".to_owned(),
                        reason: None,
                        permissions: PermissionProfile {
                            network: None,
                            file_system: Some(AdditionalFileSystemPermissions {
                                read: None,
                                write: Some(vec!["/tmp/a".to_owned()]),
                            }),
                        },
                    },
                },
            )
            .await
            .expect("register");

        let error = registry
            .resolve_permissions_subset(
                &registration.approval_key,
                SubmitPermissionsSubsetRequest {
                    permissions: PermissionProfile {
                        network: None,
                        file_system: Some(AdditionalFileSystemPermissions {
                            read: None,
                            write: Some(vec!["/tmp/b".to_owned()]),
                        }),
                    },
                    scope: PermissionGrantScope::Turn,
                },
            )
            .await
            .expect_err("subset should fail");
        assert!(error.to_string().contains("/tmp/b"));
    }

    #[tokio::test]
    async fn server_resolution_clears_matching_request() {
        let registry = ApprovalRequestRegistry::new();
        let registration = registry
            .register_tui(
                "thread-1".to_owned(),
                11,
                PendingApprovalPayload::Permissions {
                    params: PermissionsRequestApprovalParams {
                        thread_id: "thr_1".to_owned(),
                        turn_id: "turn_1".to_owned(),
                        item_id: "call_1".to_owned(),
                        reason: None,
                        permissions: PermissionProfile::default(),
                    },
                },
            )
            .await
            .expect("register");

        let cleared = registry
            .resolve_request_id("thr_1", &json!(11))
            .await
            .expect("cleared");
        assert_eq!(cleared.approval_key, registration.approval_key);
        assert!(registry.get_view(&registration.approval_key).await.is_none());
    }
}
