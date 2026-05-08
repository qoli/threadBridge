use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ThreadId};
use walkdir::WalkDir;

use super::{AppState, SessionBinding, ThreadRecord, format_prefixed_text};
use crate::delivery_bus::{
    ClaimStatus, DeliveryAttempt, DeliveryChannel, DeliveryClaim, DeliveryKind,
};
use crate::repository::{
    TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorRole,
};
use crate::thread_state::{BindingStatus, resolve_binding_status};

const CURSOR_FILE_NAME: &str = "codex-home-session-mirror.json";
const CURSOR_SCHEMA_VERSION: u32 = 1;

fn thread_id_from_i32(value: i32) -> ThreadId {
    ThreadId(MessageId(value))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CodexHomeMirrorCursorStore {
    #[serde(default = "cursor_schema_version")]
    schema_version: u32,
    #[serde(default)]
    sessions: BTreeMap<String, CodexHomeMirrorCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CodexHomeMirrorCursor {
    pub thread_key: String,
    pub session_id: String,
    pub log_path: String,
    pub last_offset: u64,
    pub last_line: u64,
    #[serde(default)]
    pub last_turn_id: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexHomeSessionLog {
    pub path: PathBuf,
    pub len: u64,
    pub cwd: String,
    pub source: Option<String>,
    pub originator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexHomeMirrorCandidate {
    pub line_no: u64,
    pub offset_end: u64,
    pub timestamp: String,
    pub turn_id: String,
    pub role: TranscriptMirrorRole,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CodexHomeParseResult {
    pub candidates: Vec<CodexHomeMirrorCandidate>,
    pub last_line: u64,
    pub last_turn_id: Option<String>,
    pub malformed_lines: u64,
}

#[derive(Debug, Default)]
struct PendingTurn {
    turn_id: String,
    user: Option<CodexHomeMirrorCandidate>,
    latest_assistant: Option<CodexHomeMirrorCandidate>,
}

fn cursor_schema_version() -> u32 {
    CURSOR_SCHEMA_VERSION
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) async fn sync_codex_home_session_mirrors_once(
    bot: &Bot,
    state: &AppState,
    active_threads: &[(ThreadRecord, SessionBinding)],
) -> Result<()> {
    if !codex_home_mirror_enabled() {
        return Ok(());
    }
    let codex_home = codex_home_dir();
    for (record, binding) in active_threads {
        if !binding_is_eligible(record, binding) {
            continue;
        }
        let Some(session_id) = binding.current_codex_thread_id.as_deref() else {
            continue;
        };
        let Some(workspace_cwd) = binding.workspace_cwd.as_deref() else {
            continue;
        };
        let Some(message_thread_id) = record.metadata.message_thread_id else {
            continue;
        };
        let result = sync_one_bound_session(
            bot,
            state,
            record,
            session_id,
            workspace_cwd,
            message_thread_id,
            &codex_home,
        )
        .await;
        if let Err(error) = result {
            write_cursor_error(record, session_id, error.to_string()).await?;
        }
    }
    Ok(())
}

fn binding_is_eligible(record: &ThreadRecord, binding: &SessionBinding) -> bool {
    resolve_binding_status(&record.metadata, Some(binding)) == BindingStatus::Healthy
        && binding.current_codex_thread_id.is_some()
        && binding.workspace_cwd.is_some()
}

async fn sync_one_bound_session(
    bot: &Bot,
    state: &AppState,
    record: &ThreadRecord,
    session_id: &str,
    workspace_cwd: &str,
    message_thread_id: i32,
    codex_home: &Path,
) -> Result<()> {
    let log = locate_session_log(codex_home, session_id)
        .with_context(|| format!("failed to locate Codex home session log for {session_id}"))?;
    if normalize_path_string(&log.cwd) != normalize_path_string(workspace_cwd) {
        bail!(
            "Codex home session cwd mismatch for {session_id}: log cwd {} != binding cwd {}",
            log.cwd,
            workspace_cwd
        );
    }

    let mut cursor_store = read_cursor(record).await?;
    let cursor = cursor_store.sessions.remove(session_id);
    if cursor
        .as_ref()
        .is_none_or(|cursor| cursor.log_path != log.path.display().to_string())
    {
        let content = tokio::fs::read_to_string(&log.path).await?;
        let parse_result = parse_codex_home_log_slice(&content, 0, 0);
        mirror_candidates_after_transcript_watermark(
            bot,
            state,
            record,
            session_id,
            message_thread_id,
            &parse_result,
        )
        .await?;
        upsert_cursor(
            record,
            CodexHomeMirrorCursor {
                thread_key: record.metadata.thread_key.clone(),
                session_id: session_id.to_owned(),
                log_path: log.path.display().to_string(),
                last_offset: log.len,
                last_line: parse_result.last_line,
                last_turn_id: parse_result.last_turn_id,
                updated_at: now_iso(),
                last_error: (parse_result.malformed_lines > 0)
                    .then(|| format!("malformed_jsonl_lines={}", parse_result.malformed_lines)),
            },
        )
        .await?;
        return Ok(());
    }
    let cursor = cursor.expect("checked Some above");
    if log.len < cursor.last_offset {
        upsert_cursor(
            record,
            CodexHomeMirrorCursor {
                last_offset: log.len,
                last_error: Some("log_shrank".to_owned()),
                updated_at: now_iso(),
                ..cursor
            },
        )
        .await?;
        return Ok(());
    }
    if log.len == cursor.last_offset {
        if cursor.last_turn_id.is_none() {
            let content = tokio::fs::read_to_string(&log.path)
                .await
                .with_context(|| format!("failed to read {}", log.path.display()))?;
            let parse_result = parse_codex_home_log_slice(&content, 0, 0);
            mirror_candidates_after_transcript_watermark(
                bot,
                state,
                record,
                session_id,
                message_thread_id,
                &parse_result,
            )
            .await?;
            upsert_cursor(
                record,
                CodexHomeMirrorCursor {
                    last_turn_id: parse_result.last_turn_id,
                    last_error: (parse_result.malformed_lines > 0)
                        .then(|| format!("malformed_jsonl_lines={}", parse_result.malformed_lines)),
                    updated_at: now_iso(),
                    ..cursor
                },
            )
            .await?;
        }
        return Ok(());
    }

    let content = tokio::fs::read_to_string(&log.path)
        .await
        .with_context(|| format!("failed to read {}", log.path.display()))?;
    let Some(slice) = content.get(cursor.last_offset as usize..) else {
        bail!(
            "cursor offset {} is not a valid utf8 boundary for {}",
            cursor.last_offset,
            log.path.display()
        );
    };
    let parse_result = parse_codex_home_log_slice(slice, cursor.last_line, cursor.last_offset);
    for candidate in &parse_result.candidates {
        mirror_candidate(bot, state, record, session_id, message_thread_id, candidate).await?;
    }
    upsert_cursor(
        record,
        CodexHomeMirrorCursor {
            thread_key: record.metadata.thread_key.clone(),
            session_id: session_id.to_owned(),
            log_path: log.path.display().to_string(),
            last_offset: log.len,
            last_line: parse_result.last_line,
            last_turn_id: parse_result.last_turn_id,
            updated_at: now_iso(),
            last_error: (parse_result.malformed_lines > 0)
                .then(|| format!("malformed_jsonl_lines={}", parse_result.malformed_lines)),
        },
    )
    .await?;
    Ok(())
}

async fn mirror_candidates_after_transcript_watermark(
    bot: &Bot,
    state: &AppState,
    record: &ThreadRecord,
    session_id: &str,
    message_thread_id: i32,
    parse_result: &CodexHomeParseResult,
) -> Result<()> {
    let Some(watermark) = latest_transcript_final_timestamp(state, record, session_id).await?
    else {
        return Ok(());
    };
    for candidate in parse_result
        .candidates
        .iter()
        .filter(|candidate| timestamp_is_after(&candidate.timestamp, &watermark))
    {
        mirror_candidate(bot, state, record, session_id, message_thread_id, candidate).await?;
    }
    Ok(())
}

async fn latest_transcript_final_timestamp(
    state: &AppState,
    record: &ThreadRecord,
    session_id: &str,
) -> Result<Option<String>> {
    let entries = state
        .repository
        .read_transcript_mirror(record, Some(TranscriptMirrorDelivery::Final), usize::MAX)
        .await?;
    Ok(entries
        .into_iter()
        .filter(|entry| entry.session_id == session_id)
        .map(|entry| entry.timestamp)
        .max())
}

async fn mirror_candidate(
    bot: &Bot,
    state: &AppState,
    record: &ThreadRecord,
    session_id: &str,
    message_thread_id: i32,
    candidate: &CodexHomeMirrorCandidate,
) -> Result<()> {
    let kind = match candidate.role {
        TranscriptMirrorRole::User => DeliveryKind::UserEcho,
        TranscriptMirrorRole::Assistant => DeliveryKind::AssistantFinal,
    };
    let claim = state
        .control
        .delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: record.metadata.thread_key.clone(),
            session_id: session_id.to_owned(),
            turn_id: Some(candidate.turn_id.clone()),
            provisional_key: None,
            channel: DeliveryChannel::Telegram,
            kind,
            owner: "codex_home_session_mirror".to_owned(),
        })
        .await?;
    if matches!(claim, ClaimStatus::Existing(_)) {
        return Ok(());
    }

    let inserted = state
        .repository
        .append_transcript_mirror(
            record,
            &TranscriptMirrorEntry {
                timestamp: candidate.timestamp.clone(),
                session_id: session_id.to_owned(),
                turn_id: Some(candidate.turn_id.clone()),
                origin: TranscriptMirrorOrigin::Local,
                role: candidate.role.clone(),
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: candidate.text.clone(),
            },
        )
        .await?;
    if !inserted {
        let _ = state
            .control
            .delivery_bus
            .commit_delivery(DeliveryAttempt {
                thread_key: record.metadata.thread_key.clone(),
                session_id: session_id.to_owned(),
                turn_id: Some(candidate.turn_id.clone()),
                provisional_key: None,
                channel: DeliveryChannel::Telegram,
                kind,
                executor: "codex_home_session_mirror".to_owned(),
                transport_ref: None,
                report_json: serde_json::json!({
                    "targets": [],
                    "skipped": "existing_transcript_mirror",
                    "source_ref": "codex_home_dir",
                    "line_no": candidate.line_no,
                }),
            })
            .await;
        return Ok(());
    }

    match candidate.role {
        TranscriptMirrorRole::User => {
            let text = format_prefixed_text("›", &candidate.text);
            bot.send_message(ChatId(record.metadata.chat_id), text)
                .message_thread_id(thread_id_from_i32(message_thread_id))
                .link_preview_options(super::disabled_link_preview_options())
                .await?;
        }
        TranscriptMirrorRole::Assistant => {
            super::final_reply::send_final_assistant_reply(
                bot,
                record,
                Some(thread_id_from_i32(message_thread_id)),
                &candidate.text,
            )
            .await?;
        }
    }

    let _ = state
        .control
        .delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key: record.metadata.thread_key.clone(),
            session_id: session_id.to_owned(),
            turn_id: Some(candidate.turn_id.clone()),
            provisional_key: None,
            channel: DeliveryChannel::Telegram,
            kind,
            executor: "codex_home_session_mirror".to_owned(),
            transport_ref: None,
            report_json: serde_json::json!({
                "targets": [{
                    "type": match candidate.role {
                        TranscriptMirrorRole::User => "telegram_codex_home_user_echo",
                        TranscriptMirrorRole::Assistant => "telegram_codex_home_assistant_final",
                    },
                    "target_ref": format!(
                        "chat:{}/thread:{}",
                        record.metadata.chat_id,
                        message_thread_id
                    ),
                    "state": "committed",
                }],
                "source_ref": "codex_home_dir",
                "line_no": candidate.line_no,
            }),
        })
        .await;
    Ok(())
}

pub(crate) fn parse_codex_home_log_slice(
    slice: &str,
    base_line: u64,
    base_offset: u64,
) -> CodexHomeParseResult {
    let mut result = CodexHomeParseResult::default();
    let mut current_turn: Option<PendingTurn> = None;
    let mut offset = base_offset;
    for (index, raw_line) in slice.lines().enumerate() {
        let line_no = base_line + index as u64 + 1;
        let offset_end = offset + raw_line.len() as u64 + 1;
        offset = offset_end;
        let Ok(value) = serde_json::from_str::<Value>(raw_line) else {
            result.malformed_lines += 1;
            result.last_line = line_no;
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(now_iso);
        let event_type = value.get("type").and_then(Value::as_str);
        if event_type == Some("event_msg")
            && value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("task_started")
            && let Some(turn_id) =
                turn_id_from_payload(value.get("payload").unwrap_or(&Value::Null))
        {
            current_turn = Some(PendingTurn {
                turn_id: turn_id.to_owned(),
                user: None,
                latest_assistant: None,
            });
            result.last_turn_id = Some(turn_id.to_owned());
            result.last_line = line_no;
            continue;
        }
        if event_type == Some("turn_context")
            && let Some(turn_id) =
                turn_id_from_payload(value.get("payload").unwrap_or(&Value::Null))
        {
            if current_turn.is_none() {
                current_turn = Some(PendingTurn {
                    turn_id: turn_id.to_owned(),
                    user: None,
                    latest_assistant: None,
                });
            }
            result.last_turn_id = Some(turn_id.to_owned());
            result.last_line = line_no;
            continue;
        }
        if event_type == Some("response_item") {
            let payload = value.get("payload").unwrap_or(&Value::Null);
            if payload.get("type").and_then(Value::as_str) == Some("message")
                && let Some(role) = payload.get("role").and_then(Value::as_str)
                && let Some(turn) = current_turn.as_mut()
                && let Some(text) = text_from_message_payload(payload)
            {
                match role {
                    "user" if turn.user.is_none() && !is_bootstrap_user_message(&text) => {
                        turn.user = Some(CodexHomeMirrorCandidate {
                            line_no,
                            offset_end,
                            timestamp,
                            turn_id: turn.turn_id.clone(),
                            role: TranscriptMirrorRole::User,
                            text,
                        });
                    }
                    "assistant" => {
                        turn.latest_assistant = Some(CodexHomeMirrorCandidate {
                            line_no,
                            offset_end,
                            timestamp,
                            turn_id: turn.turn_id.clone(),
                            role: TranscriptMirrorRole::Assistant,
                            text,
                        });
                    }
                    _ => {}
                }
            }
            result.last_line = line_no;
            continue;
        }
        if event_type == Some("event_msg")
            && value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("task_complete")
        {
            if let Some(turn) = current_turn.take() {
                if let Some(user) = turn.user {
                    result.candidates.push(user);
                    if let Some(assistant) = turn.latest_assistant {
                        result.candidates.push(assistant);
                    }
                }
            }
            result.last_line = line_no;
            continue;
        }
        result.last_line = line_no;
    }
    result
}

fn text_from_message_payload(payload: &Value) -> Option<String> {
    let parts = payload
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| {
            item.get("text")
                .or_else(|| item.get("output_text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn turn_id_from_payload(payload: &Value) -> Option<&str> {
    payload
        .get("id")
        .or_else(|| payload.get("turn_id"))
        .and_then(Value::as_str)
}

fn is_bootstrap_user_message(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("# AGENTS.md instructions")
        || trimmed.starts_with("Read and follow the workspace AGENTS.md if present")
}

pub(crate) fn locate_session_log(
    codex_home: &Path,
    session_id: &str,
) -> Result<CodexHomeSessionLog> {
    let _ = session_index_contains(codex_home, session_id);
    for root_name in ["sessions", "archived_sessions"] {
        let root = codex_home.join(root_name);
        if !root.exists() {
            continue;
        }
        let mut matches = WalkDir::new(&root)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy();
                name.starts_with("rollout-")
                    && name.ends_with(".jsonl")
                    && name.contains(session_id)
            })
            .map(|entry| entry.path().to_path_buf())
            .collect::<Vec<_>>();
        matches.sort();
        if let Some(path) = matches.into_iter().next() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let cwd = session_meta_cwd(&content)
                .with_context(|| format!("session_meta cwd missing in {}", path.display()))?;
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("failed to stat {}", path.display()))?;
            let (source, originator) = session_meta_source_originator(&content);
            return Ok(CodexHomeSessionLog {
                path,
                len: metadata.len(),
                cwd,
                source,
                originator,
            });
        }
    }
    bail!("Codex home session log not found for {session_id}")
}

fn session_index_contains(codex_home: &Path, session_id: &str) -> Result<bool> {
    let path = codex_home.join("session_index.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    Ok(content.lines().any(|line| line.contains(session_id)))
}

fn session_meta_cwd(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let value = serde_json::from_str::<Value>(line).ok()?;
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            return None;
        }
        value
            .get("payload")?
            .get("cwd")?
            .as_str()
            .map(str::to_owned)
    })
}

fn session_meta_source_originator(content: &str) -> (Option<String>, Option<String>) {
    content
        .lines()
        .find_map(|line| {
            let value = serde_json::from_str::<Value>(line).ok()?;
            if value.get("type").and_then(Value::as_str) != Some("session_meta") {
                return None;
            }
            let payload = value.get("payload")?;
            Some((
                payload
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                payload
                    .get("originator")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ))
        })
        .unwrap_or_default()
}

async fn read_cursor(record: &ThreadRecord) -> Result<CodexHomeMirrorCursorStore> {
    let path = cursor_path(record);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CodexHomeMirrorCursorStore {
                schema_version: CURSOR_SCHEMA_VERSION,
                sessions: BTreeMap::new(),
            })
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

async fn upsert_cursor(record: &ThreadRecord, cursor: CodexHomeMirrorCursor) -> Result<()> {
    let mut store = read_cursor(record).await?;
    store.schema_version = CURSOR_SCHEMA_VERSION;
    store.sessions.insert(cursor.session_id.clone(), cursor);
    let path = cursor_path(record);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&store)?),
    )
    .await
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

async fn write_cursor_error(record: &ThreadRecord, session_id: &str, error: String) -> Result<()> {
    let mut store = read_cursor(record).await?;
    let mut cursor = store
        .sessions
        .remove(session_id)
        .unwrap_or(CodexHomeMirrorCursor {
            thread_key: record.metadata.thread_key.clone(),
            session_id: session_id.to_owned(),
            log_path: String::new(),
            last_offset: 0,
            last_line: 0,
            last_turn_id: None,
            updated_at: now_iso(),
            last_error: None,
        });
    cursor.updated_at = now_iso();
    cursor.last_error = Some(error);
    upsert_cursor(record, cursor).await
}

fn cursor_path(record: &ThreadRecord) -> PathBuf {
    record.state_path().join(CURSOR_FILE_NAME)
}

fn codex_home_dir() -> PathBuf {
    env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            env::var("HOME")
                .map(|home| PathBuf::from(home).join(".codex"))
                .unwrap_or_else(|_| PathBuf::from(".codex"))
        })
}

fn codex_home_mirror_enabled() -> bool {
    env::var("THREADBRIDGE_CODEX_HOME_MIRROR")
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "0" | "false" | "off" | "no")
        })
        .unwrap_or(true)
}

fn normalize_path_string(value: &str) -> String {
    Path::new(value)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(value))
        .display()
        .to_string()
}

fn timestamp_is_after(candidate: &str, watermark: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(candidate),
        chrono::DateTime::parse_from_rfc3339(watermark),
    ) {
        (Ok(candidate), Ok(watermark)) => candidate > watermark,
        _ => candidate > watermark,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CodexHomeMirrorCandidate, locate_session_log, parse_codex_home_log_slice,
        session_index_contains, timestamp_is_after,
    };
    use crate::repository::TranscriptMirrorRole;
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-codex-home-test-{}", Uuid::new_v4()))
    }

    fn line(value: serde_json::Value) -> String {
        format!("{}\n", serde_json::to_string(&value).unwrap())
    }

    #[test]
    fn parser_uses_turn_lifecycle_and_last_assistant_only() {
        let slice = [
            line(json!({"timestamp":"2026-05-08T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.001Z","type":"turn_context","payload":{"turn_id":"turn-1"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.002Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real prompt"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.003Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"first commentary"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.004Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"final answer"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.005Z","type":"event_msg","payload":{"type":"task_complete","id":"turn-1"}})),
        ]
        .concat();
        let parsed = parse_codex_home_log_slice(&slice, 0, 0);
        assert_eq!(
            parsed.candidates,
            vec![
                CodexHomeMirrorCandidate {
                    line_no: 3,
                    offset_end: parsed.candidates[0].offset_end,
                    timestamp: "2026-05-08T00:00:00.002Z".to_owned(),
                    turn_id: "turn-1".to_owned(),
                    role: TranscriptMirrorRole::User,
                    text: "real prompt".to_owned(),
                },
                CodexHomeMirrorCandidate {
                    line_no: 5,
                    offset_end: parsed.candidates[1].offset_end,
                    timestamp: "2026-05-08T00:00:00.004Z".to_owned(),
                    turn_id: "turn-1".to_owned(),
                    role: TranscriptMirrorRole::Assistant,
                    text: "final answer".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn parser_skips_bootstrap_user_messages() {
        let slice = [
            line(json!({"timestamp":"2026-05-08T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","id":"turn-1"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.001Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /tmp\n<INSTRUCTIONS>"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.002Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"READY"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.003Z","type":"event_msg","payload":{"type":"task_complete","id":"turn-1"}})),
        ]
        .concat();
        let parsed = parse_codex_home_log_slice(&slice, 0, 0);
        assert!(parsed.candidates.is_empty());
    }

    #[test]
    fn parser_keeps_identical_text_in_different_turns_and_counts_malformed_lines() {
        let slice = [
            "{not-json}\n".to_owned(),
            line(json!({"timestamp":"2026-05-08T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","id":"turn-1"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.001Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"same text"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.002Z","type":"event_msg","payload":{"type":"task_complete","id":"turn-1"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.003Z","type":"event_msg","payload":{"type":"task_started","id":"turn-2"}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.004Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"same text"}]}})),
            line(json!({"timestamp":"2026-05-08T00:00:00.005Z","type":"event_msg","payload":{"type":"task_complete","id":"turn-2"}})),
        ]
        .concat();
        let parsed = parse_codex_home_log_slice(&slice, 0, 0);
        assert_eq!(parsed.malformed_lines, 1);
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].turn_id, "turn-1");
        assert_eq!(parsed.candidates[1].turn_id, "turn-2");
        assert_eq!(parsed.candidates[0].text, parsed.candidates[1].text);
    }

    #[test]
    fn timestamp_comparison_uses_rfc3339_order() {
        assert!(timestamp_is_after(
            "2026-05-08T07:04:06.906Z",
            "2026-05-08T05:34:05.367Z"
        ));
        assert!(!timestamp_is_after(
            "2026-05-08T05:34:05.367Z",
            "2026-05-08T07:04:06.906Z"
        ));
    }

    #[tokio::test]
    async fn locate_session_log_falls_back_when_index_misses() {
        let root = temp_path();
        let sessions = root.join("sessions/2026/05/07");
        fs::create_dir_all(&sessions).await.unwrap();
        fs::write(root.join("session_index.jsonl"), "")
            .await
            .unwrap();
        let session_id = "019dff0f-ccf1-7852-924d-bb8a1b986ee9";
        let log = sessions.join(format!("rollout-2026-05-07T04-51-58-{session_id}.jsonl"));
        fs::write(
            &log,
            line(json!({"type":"session_meta","payload":{"id":session_id,"cwd":"/tmp/workspace","source":"vscode","originator":"threadbridge"}})),
        )
        .await
        .unwrap();
        assert!(!session_index_contains(&root, session_id).unwrap());
        let located = locate_session_log(&root, session_id).unwrap();
        assert_eq!(located.path, log);
        assert_eq!(located.cwd, "/tmp/workspace");
    }
}
