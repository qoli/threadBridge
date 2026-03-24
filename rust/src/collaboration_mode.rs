use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationMode {
    Default,
    Plan,
}

impl CollaborationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }
}
