use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::codex::CodexThreadEvent;
use crate::repository::{
    TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorPhase,
    TranscriptMirrorRole,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceItemDiagnostic {
    pub method: String,
    pub item_type: String,
    pub item_keys: Vec<String>,
}

pub fn process_entry_from_codex_event(
    event: &CodexThreadEvent,
    session_id: &str,
    origin: TranscriptMirrorOrigin,
) -> Option<TranscriptMirrorEntry> {
    let (lifecycle, item) = match event {
        CodexThreadEvent::ItemStarted { item } => ("item.started", item),
        CodexThreadEvent::ItemCompleted { item } => ("item.completed", item),
        _ => return None,
    };
    process_entry_from_item(lifecycle, item, now_iso(), session_id, origin)
}

pub fn process_entry_from_workspace_message(
    message: &WsMessage,
    session_id: &str,
    origin: TranscriptMirrorOrigin,
) -> Result<Option<TranscriptMirrorEntry>> {
    let Some(parsed) = parse_workspace_item_message(message)? else {
        return Ok(None);
    };
    match (Some(parsed.method.as_str()), Some(&parsed.item)) {
        (Some(method), Some(item)) => Ok(process_entry_from_item(
            method,
            item,
            now_iso(),
            session_id,
            origin,
        )),
        _ => Ok(None),
    }
}

pub fn workspace_item_diagnostic(message: &WsMessage) -> Result<Option<WorkspaceItemDiagnostic>> {
    let Some(parsed) = parse_workspace_item_message(message)? else {
        return Ok(None);
    };
    let mut item_keys = parsed
        .item
        .as_object()
        .map(|map| map.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    item_keys.sort();
    Ok(Some(WorkspaceItemDiagnostic {
        method: parsed.method,
        item_type: parsed.item_type,
        item_keys,
    }))
}

struct ParsedWorkspaceItemMessage {
    method: String,
    item_type: String,
    item: Value,
}

fn parse_workspace_item_message(message: &WsMessage) -> Result<Option<ParsedWorkspaceItemMessage>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    let Some(method) = payload
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let Some(item) = payload
        .get("params")
        .and_then(|params| params.get("item"))
        .cloned()
    else {
        return Ok(None);
    };
    let Some(item_type) = item.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(None);
    };
    Ok(Some(ParsedWorkspaceItemMessage {
        method,
        item_type,
        item,
    }))
}

fn process_entry_from_item(
    lifecycle: &str,
    item: &Value,
    timestamp: String,
    session_id: &str,
    origin: TranscriptMirrorOrigin,
) -> Option<TranscriptMirrorEntry> {
    let item_type = normalize_item_type(item.get("type").and_then(Value::as_str)?)?;
    let normalized_lifecycle = match lifecycle {
        "item.completed" | "item/completed" => "item.completed",
        "item.started" | "item/started" => "item.started",
        "item.updated" | "item/updated" => "item.updated",
        other => other,
    };
    let (phase, text) = match (normalized_lifecycle, item_type) {
        ("item.started", "plan") | ("item.updated", "plan") | ("item.completed", "plan") => {
            (TranscriptMirrorPhase::Plan, summarize_plan_item(item)?)
        }
        ("item.started", "todo_list")
        | ("item.updated", "todo_list")
        | ("item.completed", "todo_list") => {
            (TranscriptMirrorPhase::Plan, summarize_todo_list_item(item)?)
        }
        ("item.started", "command_execution") => (
            TranscriptMirrorPhase::Tool,
            summarize_tool_item("Command", item)?,
        ),
        ("item.started", "mcp_tool_call") => (
            TranscriptMirrorPhase::Tool,
            summarize_tool_item("MCP tool", item)?,
        ),
        ("item.started", "web_search") => (
            TranscriptMirrorPhase::Tool,
            summarize_tool_item("Web search", item)?,
        ),
        _ => return None,
    };
    Some(TranscriptMirrorEntry {
        timestamp,
        session_id: session_id.to_owned(),
        origin,
        role: TranscriptMirrorRole::Assistant,
        delivery: TranscriptMirrorDelivery::Process,
        phase: Some(phase),
        text,
    })
}

fn normalize_item_type(item_type: &str) -> Option<&'static str> {
    match item_type {
        "plan" => Some("plan"),
        "todo_list" => Some("todo_list"),
        "commandExecution" | "command_execution" => Some("command_execution"),
        "mcpToolCall" | "mcp_tool_call" => Some("mcp_tool_call"),
        "webSearch" | "web_search" => Some("web_search"),
        "agentMessage" | "agent_message" => Some("agent_message"),
        _ => None,
    }
}

fn summarize_plan_item(item: &Value) -> Option<String> {
    let text = item.get("text").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    Some(format!("Plan: {text}"))
}

fn summarize_todo_list_item(item: &Value) -> Option<String> {
    let items = item.get("items")?.as_array()?;
    let todos = items
        .iter()
        .filter_map(|entry| {
            entry
                .get("content")
                .or_else(|| entry.get("text"))
                .or_else(|| entry.get("title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_owned())
        })
        .collect::<Vec<_>>();
    if todos.is_empty() {
        return None;
    }
    Some(format!("Plan: {}", todos.join(" | ")))
}

fn summarize_tool_item(prefix: &str, item: &Value) -> Option<String> {
    let detail = item
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| item.get("query").and_then(Value::as_str))
        .or_else(|| item.get("toolName").and_then(Value::as_str))
        .or_else(|| item.get("tool_name").and_then(Value::as_str))
        .or_else(|| item.get("server").and_then(Value::as_str))
        .or_else(|| item.get("tool").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(format!("{prefix}: {detail}"))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::{
        process_entry_from_codex_event, process_entry_from_workspace_message,
        workspace_item_diagnostic,
    };
    use crate::codex::CodexThreadEvent;
    use crate::repository::{TranscriptMirrorOrigin, TranscriptMirrorPhase};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[test]
    fn codex_plan_event_maps_to_plan_process_entry() {
        let event = CodexThreadEvent::ItemCompleted {
            item: json!({
                "type": "plan",
                "text": "inspect runtime owner | wire transcript API"
            }),
        };
        let entry =
            process_entry_from_codex_event(&event, "session-1", TranscriptMirrorOrigin::Telegram)
                .expect("process entry");
        assert_eq!(entry.phase, Some(TranscriptMirrorPhase::Plan));
        assert_eq!(
            entry.text,
            "Plan: inspect runtime owner | wire transcript API"
        );
    }

    #[test]
    fn workspace_tool_message_maps_to_tool_process_entry() {
        let message = WsMessage::Text(
            json!({
                "method": "item/started",
                "params": {
                    "item": {
                        "type": "commandExecution",
                        "command": "cargo test"
                    }
                }
            })
            .to_string()
            .into(),
        );
        let entry = process_entry_from_workspace_message(
            &message,
            "session-1",
            TranscriptMirrorOrigin::Tui,
        )
        .unwrap()
        .expect("process entry");
        assert_eq!(entry.phase, Some(TranscriptMirrorPhase::Tool));
        assert_eq!(entry.text, "Command: cargo test");
    }

    #[test]
    fn workspace_plan_message_maps_to_plan_process_entry() {
        let message = WsMessage::Text(
            json!({
                "method": "item/completed",
                "params": {
                    "item": {
                        "type": "plan",
                        "text": "look at latest commits"
                    }
                }
            })
            .to_string()
            .into(),
        );
        let entry = process_entry_from_workspace_message(
            &message,
            "session-1",
            TranscriptMirrorOrigin::Tui,
        )
        .unwrap()
        .expect("process entry");
        assert_eq!(entry.phase, Some(TranscriptMirrorPhase::Plan));
        assert_eq!(entry.text, "Plan: look at latest commits");
    }

    #[test]
    fn workspace_item_diagnostic_returns_method_type_and_keys() {
        let message = WsMessage::Text(
            json!({
                "method": "item/updated",
                "params": {
                    "item": {
                        "type": "plan",
                        "id": "item_1",
                        "text": "check repo"
                    }
                }
            })
            .to_string()
            .into(),
        );
        let diagnostic = workspace_item_diagnostic(&message)
            .unwrap()
            .expect("diagnostic");
        assert_eq!(diagnostic.method, "item/updated");
        assert_eq!(diagnostic.item_type, "plan");
        assert_eq!(diagnostic.item_keys, vec!["id", "text", "type"]);
    }
}
