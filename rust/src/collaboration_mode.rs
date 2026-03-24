use serde::{Deserialize, Serialize};
use serde_json::Value;

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

    pub fn from_wire_value(value: &Value) -> Option<Self> {
        value.as_str().and_then(Self::from_wire).or_else(|| {
            value
                .get("mode")
                .and_then(Value::as_str)
                .and_then(Self::from_wire)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CollaborationMode;
    use serde_json::json;

    #[test]
    fn from_wire_value_accepts_string_and_object_shapes() {
        assert_eq!(
            CollaborationMode::from_wire_value(&json!("plan")),
            Some(CollaborationMode::Plan)
        );
        assert_eq!(
            CollaborationMode::from_wire_value(&json!({
                "mode": "default",
                "settings": {
                    "model": "gpt-test",
                }
            })),
            Some(CollaborationMode::Default)
        );
    }
}
