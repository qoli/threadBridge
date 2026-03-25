use tokio::sync::mpsc;

use crate::collaboration_mode::CollaborationMode;
use crate::interactive::ToolRequestUserInputParams;

#[derive(Debug, Clone)]
pub struct RuntimeInteractionRequest {
    pub thread_key: String,
    pub request_id: i64,
    pub params: ToolRequestUserInputParams,
}

#[derive(Debug, Clone)]
pub struct RuntimeInteractionResolved {
    pub thread_id: String,
    pub request_id: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct TurnCompletionSummary {
    pub thread_key: String,
    pub collaboration_mode: CollaborationMode,
    pub final_text: Option<String>,
    pub has_plan: bool,
}

#[derive(Debug, Clone)]
pub enum RuntimeInteractionEvent {
    RequestUserInput(RuntimeInteractionRequest),
    RequestResolved(RuntimeInteractionResolved),
    TurnCompleted(TurnCompletionSummary),
}

pub type RuntimeInteractionSender = mpsc::UnboundedSender<RuntimeInteractionEvent>;
