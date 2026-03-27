use tokio::sync::mpsc;

use crate::collaboration_mode::CollaborationMode;
use crate::interactive::ToolRequestUserInputParams;
use crate::runtime_protocol::RuntimeInteractionKind;

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

impl RuntimeInteractionEvent {
    pub fn kind(&self) -> RuntimeInteractionKind {
        match self {
            Self::RequestUserInput(_) => RuntimeInteractionKind::RequestUserInput,
            Self::RequestResolved(_) => RuntimeInteractionKind::RequestResolved,
            Self::TurnCompleted(_) => RuntimeInteractionKind::TurnCompleted,
        }
    }
}

impl TurnCompletionSummary {
    pub fn plan_follow_up_requested(&self) -> bool {
        self.collaboration_mode == CollaborationMode::Plan && self.has_plan
    }
}

pub type RuntimeInteractionSender = mpsc::UnboundedSender<RuntimeInteractionEvent>;

#[cfg(test)]
mod tests {
    use super::{RuntimeInteractionEvent, TurnCompletionSummary};
    use crate::collaboration_mode::CollaborationMode;
    use crate::runtime_protocol::RuntimeInteractionKind;

    #[test]
    fn interaction_event_kind_maps_to_canonical_kind() {
        let event = RuntimeInteractionEvent::TurnCompleted(TurnCompletionSummary {
            thread_key: "thread-1".to_owned(),
            collaboration_mode: CollaborationMode::Default,
            final_text: None,
            has_plan: false,
        });
        assert_eq!(event.kind(), RuntimeInteractionKind::TurnCompleted);
    }

    #[test]
    fn plan_follow_up_requested_requires_plan_mode_with_plan() {
        let summary = TurnCompletionSummary {
            thread_key: "thread-1".to_owned(),
            collaboration_mode: CollaborationMode::Plan,
            final_text: Some("plan".to_owned()),
            has_plan: true,
        };
        assert!(summary.plan_follow_up_requested());

        let default_mode = TurnCompletionSummary {
            collaboration_mode: CollaborationMode::Default,
            ..summary.clone()
        };
        assert!(!default_mode.plan_follow_up_requested());
    }
}
