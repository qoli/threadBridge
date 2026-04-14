use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task;

const SQLITE_FILE_NAME: &str = "delivery.sqlite3";
const PROVISIONAL_BUCKET_SECONDS: i64 = 5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChannel {
    Telegram,
    DesktopWidget,
}

impl DeliveryChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::DesktopWidget => "desktop_widget",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    UserEcho,
    AssistantFinal,
    PreviewDraft,
    SystemNotice,
    RequestUserInputPrompt,
    ApprovalPrompt,
    OutboxText,
    OutboxMedia,
}

impl DeliveryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserEcho => "user_echo",
            Self::AssistantFinal => "assistant_final",
            Self::PreviewDraft => "preview_draft",
            Self::SystemNotice => "system_notice",
            Self::RequestUserInputPrompt => "request_user_input_prompt",
            Self::ApprovalPrompt => "approval_prompt",
            Self::OutboxText => "outbox_text",
            Self::OutboxMedia => "outbox_media",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Claimed,
    Committed,
    Failed,
}

impl DeliveryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryRecord {
    pub delivery_id: i64,
    pub thread_key: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub provisional_key: Option<String>,
    pub channel: DeliveryChannel,
    pub kind: DeliveryKind,
    pub state: DeliveryState,
    pub owner: String,
    pub claimed_at: String,
    pub committed_at: Option<String>,
    pub failed_at: Option<String>,
    pub latest_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryAttemptReport {
    pub executor: String,
    #[serde(default)]
    pub transport_ref: Option<String>,
    #[serde(default = "default_report_json")]
    pub report_json: Value,
}

fn default_report_json() -> Value {
    json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DeliveryEvent {
    Claimed { record: DeliveryRecord },
    Promoted { record: DeliveryRecord },
    Committed { record: DeliveryRecord },
    Failed { record: DeliveryRecord },
}

#[derive(Debug, Clone)]
pub struct DeliveryClaim {
    pub thread_key: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub provisional_key: Option<String>,
    pub channel: DeliveryChannel,
    pub kind: DeliveryKind,
    pub owner: String,
}

#[derive(Debug, Clone)]
pub struct DeliveryAttempt {
    pub thread_key: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub provisional_key: Option<String>,
    pub channel: DeliveryChannel,
    pub kind: DeliveryKind,
    pub executor: String,
    pub transport_ref: Option<String>,
    pub report_json: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimStatus {
    Claimed(DeliveryRecord),
    Existing(DeliveryRecord),
}

#[derive(Debug, Clone)]
pub struct DeliveryBusCoordinator {
    db_path: PathBuf,
    events: broadcast::Sender<DeliveryEvent>,
}

impl DeliveryBusCoordinator {
    pub async fn new(data_root: impl AsRef<Path>) -> Result<Self> {
        let db_path = data_root.as_ref().join(SQLITE_FILE_NAME);
        let init_path = db_path.clone();
        task::spawn_blocking(move || init_db(&init_path))
            .await
            .context("delivery bus init task failed")??;
        let (events, _) = broadcast::channel(128);
        Ok(Self { db_path, events })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DeliveryEvent> {
        self.events.subscribe()
    }

    pub async fn claim_delivery(&self, claim: DeliveryClaim) -> Result<ClaimStatus> {
        let db_path = self.db_path.clone();
        let result = task::spawn_blocking(move || claim_delivery_blocking(&db_path, claim))
            .await
            .context("claim delivery task failed")??;
        if let ClaimStatus::Claimed(record) = &result {
            let _ = self.events.send(DeliveryEvent::Claimed {
                record: record.clone(),
            });
        }
        Ok(result)
    }

    pub async fn promote_delivery_turn(
        &self,
        thread_key: &str,
        session_id: &str,
        provisional_key: &str,
        channel: DeliveryChannel,
        kind: DeliveryKind,
        turn_id: &str,
    ) -> Result<Option<DeliveryRecord>> {
        let db_path = self.db_path.clone();
        let thread_key = thread_key.to_owned();
        let session_id = session_id.to_owned();
        let provisional_key = provisional_key.to_owned();
        let turn_id = turn_id.to_owned();
        let record = task::spawn_blocking(move || {
            promote_delivery_turn_blocking(
                &db_path,
                &thread_key,
                &session_id,
                &provisional_key,
                channel,
                kind,
                &turn_id,
            )
        })
        .await
        .context("promote delivery task failed")??;
        if let Some(record) = &record {
            let _ = self.events.send(DeliveryEvent::Promoted {
                record: record.clone(),
            });
        }
        Ok(record)
    }

    pub async fn commit_delivery(
        &self,
        attempt: DeliveryAttempt,
    ) -> Result<Option<DeliveryRecord>> {
        let db_path = self.db_path.clone();
        let record = task::spawn_blocking(move || commit_delivery_blocking(&db_path, attempt))
            .await
            .context("commit delivery task failed")??;
        if let Some(record) = &record {
            let _ = self.events.send(DeliveryEvent::Committed {
                record: record.clone(),
            });
        }
        Ok(record)
    }

    pub async fn fail_delivery(
        &self,
        attempt: DeliveryAttempt,
        error: impl Into<String>,
    ) -> Result<Option<DeliveryRecord>> {
        let db_path = self.db_path.clone();
        let error = error.into();
        let record = task::spawn_blocking(move || fail_delivery_blocking(&db_path, attempt, error))
            .await
            .context("fail delivery task failed")??;
        if let Some(record) = &record {
            let _ = self.events.send(DeliveryEvent::Failed {
                record: record.clone(),
            });
        }
        Ok(record)
    }

    #[cfg(test)]
    pub async fn list_records(&self) -> Result<Vec<DeliveryRecord>> {
        let db_path = self.db_path.clone();
        task::spawn_blocking(move || list_records_blocking(&db_path))
            .await
            .context("list records task failed")?
    }
}

pub fn provisional_key_for_text(
    session_id: &str,
    kind: DeliveryKind,
    text: &str,
    occurred_at: &str,
) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let hash = fnv1a_64(&format!("{session_id}:{}:{normalized}", kind.as_str()));
    let bucket = provisional_bucket(occurred_at);
    format!("text:{hash:016x}:{bucket}")
}

pub fn provisional_key_for_request(session_id: &str, request_id: i64, item_id: &str) -> String {
    let hash = fnv1a_64(&format!("{session_id}:request:{request_id}:{item_id}"));
    format!("request:{hash:016x}")
}

pub fn provisional_key_for_outbox(
    session_id: &str,
    kind: DeliveryKind,
    descriptor: &str,
    occurred_at: &str,
) -> String {
    let hash = fnv1a_64(&format!("{session_id}:{}:{descriptor}", kind.as_str()));
    let bucket = provisional_bucket(occurred_at);
    format!("outbox:{hash:016x}:{bucket}")
}

fn init_db(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open delivery db {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS deliveries (
            delivery_id INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_key TEXT NOT NULL,
            session_id TEXT NOT NULL,
            turn_id TEXT,
            provisional_key TEXT,
            channel TEXT NOT NULL,
            kind TEXT NOT NULL,
            state TEXT NOT NULL,
            owner TEXT NOT NULL,
            claimed_at TEXT NOT NULL,
            committed_at TEXT,
            failed_at TEXT,
            latest_error TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_deliveries_turn_identity
            ON deliveries(thread_key, session_id, turn_id, channel, kind)
            WHERE turn_id IS NOT NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_deliveries_provisional_identity
            ON deliveries(thread_key, session_id, provisional_key, channel, kind)
            WHERE provisional_key IS NOT NULL AND turn_id IS NULL;
        CREATE TABLE IF NOT EXISTS delivery_attempts (
            attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
            delivery_id INTEGER NOT NULL,
            executor TEXT NOT NULL,
            state TEXT NOT NULL,
            transport_ref TEXT,
            error TEXT,
            report_json TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            FOREIGN KEY(delivery_id) REFERENCES deliveries(delivery_id)
        );
        "#,
    )?;
    Ok(())
}

fn claim_delivery_blocking(path: &Path, claim: DeliveryClaim) -> Result<ClaimStatus> {
    let conn = Connection::open(path)?;
    let tx = conn.unchecked_transaction()?;
    if let Some(existing) = find_delivery(
        &tx,
        &claim.thread_key,
        &claim.session_id,
        claim.turn_id.as_deref(),
        claim.provisional_key.as_deref(),
        claim.channel,
        claim.kind,
    )? {
        tx.commit()?;
        return Ok(ClaimStatus::Existing(existing));
    }
    let now = now_iso();
    tx.execute(
        "INSERT INTO deliveries (
            thread_key, session_id, turn_id, provisional_key, channel, kind, state,
            owner, claimed_at, committed_at, failed_at, latest_error
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, NULL)",
        params![
            claim.thread_key,
            claim.session_id,
            claim.turn_id,
            claim.provisional_key,
            claim.channel.as_str(),
            claim.kind.as_str(),
            DeliveryState::Claimed.as_str(),
            claim.owner,
            now,
        ],
    )?;
    let id = tx.last_insert_rowid();
    let record = read_record_by_id(&tx, id)?.context("missing inserted delivery record")?;
    tx.commit()?;
    Ok(ClaimStatus::Claimed(record))
}

fn promote_delivery_turn_blocking(
    path: &Path,
    thread_key: &str,
    session_id: &str,
    provisional_key: &str,
    channel: DeliveryChannel,
    kind: DeliveryKind,
    turn_id: &str,
) -> Result<Option<DeliveryRecord>> {
    let conn = Connection::open(path)?;
    let tx = conn.unchecked_transaction()?;
    if let Some(existing) = find_delivery(
        &tx,
        thread_key,
        session_id,
        Some(turn_id),
        None,
        channel,
        kind,
    )? {
        tx.commit()?;
        return Ok(Some(existing));
    }
    let id = {
        let mut stmt = tx.prepare(
            "SELECT delivery_id FROM deliveries
             WHERE thread_key = ?1 AND session_id = ?2 AND provisional_key = ?3
               AND channel = ?4 AND kind = ?5
             ORDER BY delivery_id DESC LIMIT 1",
        )?;
        stmt.query_row(
            params![
                thread_key,
                session_id,
                provisional_key,
                channel.as_str(),
                kind.as_str()
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    };
    let Some(id) = id else {
        tx.commit()?;
        return Ok(None);
    };
    tx.execute(
        "UPDATE deliveries
         SET turn_id = ?1
         WHERE delivery_id = ?2",
        params![turn_id, id],
    )?;
    let record = read_record_by_id(&tx, id)?;
    tx.commit()?;
    Ok(record)
}

fn commit_delivery_blocking(
    path: &Path,
    attempt: DeliveryAttempt,
) -> Result<Option<DeliveryRecord>> {
    let conn = Connection::open(path)?;
    let tx = conn.unchecked_transaction()?;
    let Some(record) = find_delivery(
        &tx,
        &attempt.thread_key,
        &attempt.session_id,
        attempt.turn_id.as_deref(),
        attempt.provisional_key.as_deref(),
        attempt.channel,
        attempt.kind,
    )?
    else {
        tx.commit()?;
        return Ok(None);
    };
    let now = now_iso();
    tx.execute(
        "INSERT INTO delivery_attempts (
            delivery_id, executor, state, transport_ref, error, report_json, started_at, finished_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
        params![
            record.delivery_id,
            attempt.executor,
            DeliveryState::Committed.as_str(),
            attempt.transport_ref,
            attempt.report_json.to_string(),
            now,
            now,
        ],
    )?;
    tx.execute(
        "UPDATE deliveries
         SET state = ?1, committed_at = ?2, failed_at = NULL, latest_error = NULL
         WHERE delivery_id = ?3",
        params![DeliveryState::Committed.as_str(), now, record.delivery_id],
    )?;
    let updated = read_record_by_id(&tx, record.delivery_id)?;
    tx.commit()?;
    Ok(updated)
}

fn fail_delivery_blocking(
    path: &Path,
    attempt: DeliveryAttempt,
    error: String,
) -> Result<Option<DeliveryRecord>> {
    let conn = Connection::open(path)?;
    let tx = conn.unchecked_transaction()?;
    let Some(record) = find_delivery(
        &tx,
        &attempt.thread_key,
        &attempt.session_id,
        attempt.turn_id.as_deref(),
        attempt.provisional_key.as_deref(),
        attempt.channel,
        attempt.kind,
    )?
    else {
        tx.commit()?;
        return Ok(None);
    };
    let now = now_iso();
    tx.execute(
        "INSERT INTO delivery_attempts (
            delivery_id, executor, state, transport_ref, error, report_json, started_at, finished_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.delivery_id,
            attempt.executor,
            DeliveryState::Failed.as_str(),
            attempt.transport_ref,
            error,
            attempt.report_json.to_string(),
            now,
            now,
        ],
    )?;
    tx.execute(
        "UPDATE deliveries
         SET state = ?1, failed_at = ?2, latest_error = ?3
         WHERE delivery_id = ?4",
        params![
            DeliveryState::Failed.as_str(),
            now,
            error,
            record.delivery_id
        ],
    )?;
    let updated = read_record_by_id(&tx, record.delivery_id)?;
    tx.commit()?;
    Ok(updated)
}

fn find_delivery(
    conn: &Connection,
    thread_key: &str,
    session_id: &str,
    turn_id: Option<&str>,
    provisional_key: Option<&str>,
    channel: DeliveryChannel,
    kind: DeliveryKind,
) -> Result<Option<DeliveryRecord>> {
    if let Some(turn_id) = turn_id {
        let mut stmt = conn.prepare(
            "SELECT * FROM deliveries
             WHERE thread_key = ?1 AND session_id = ?2 AND turn_id = ?3
               AND channel = ?4 AND kind = ?5
             ORDER BY delivery_id DESC LIMIT 1",
        )?;
        return stmt
            .query_row(
                params![
                    thread_key,
                    session_id,
                    turn_id,
                    channel.as_str(),
                    kind.as_str()
                ],
                read_record,
            )
            .optional()
            .map_err(Into::into);
    }
    let Some(provisional_key) = provisional_key else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT * FROM deliveries
         WHERE thread_key = ?1 AND session_id = ?2 AND provisional_key = ?3
           AND channel = ?4 AND kind = ?5
         ORDER BY delivery_id DESC LIMIT 1",
    )?;
    stmt.query_row(
        params![
            thread_key,
            session_id,
            provisional_key,
            channel.as_str(),
            kind.as_str()
        ],
        read_record,
    )
    .optional()
    .map_err(Into::into)
}

fn read_record_by_id(conn: &Connection, delivery_id: i64) -> Result<Option<DeliveryRecord>> {
    let mut stmt = conn.prepare("SELECT * FROM deliveries WHERE delivery_id = ?1")?;
    stmt.query_row(params![delivery_id], read_record)
        .optional()
        .map_err(Into::into)
}

fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRecord> {
    Ok(DeliveryRecord {
        delivery_id: row.get("delivery_id")?,
        thread_key: row.get("thread_key")?,
        session_id: row.get("session_id")?,
        turn_id: row.get("turn_id")?,
        provisional_key: row.get("provisional_key")?,
        channel: match row.get::<_, String>("channel")?.as_str() {
            "telegram" => DeliveryChannel::Telegram,
            "desktop_widget" => DeliveryChannel::DesktopWidget,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown delivery channel: {other}"),
                    )),
                ));
            }
        },
        kind: match row.get::<_, String>("kind")?.as_str() {
            "user_echo" => DeliveryKind::UserEcho,
            "assistant_final" => DeliveryKind::AssistantFinal,
            "preview_draft" => DeliveryKind::PreviewDraft,
            "system_notice" => DeliveryKind::SystemNotice,
            "request_user_input_prompt" => DeliveryKind::RequestUserInputPrompt,
            "approval_prompt" => DeliveryKind::ApprovalPrompt,
            "outbox_text" => DeliveryKind::OutboxText,
            "outbox_media" => DeliveryKind::OutboxMedia,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown delivery kind: {other}"),
                    )),
                ));
            }
        },
        state: match row.get::<_, String>("state")?.as_str() {
            "claimed" => DeliveryState::Claimed,
            "committed" => DeliveryState::Committed,
            "failed" => DeliveryState::Failed,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown delivery state: {other}"),
                    )),
                ));
            }
        },
        owner: row.get("owner")?,
        claimed_at: row.get("claimed_at")?,
        committed_at: row.get("committed_at")?,
        failed_at: row.get("failed_at")?,
        latest_error: row.get("latest_error")?,
    })
}

#[cfg(test)]
fn list_records_blocking(path: &Path) -> Result<Vec<DeliveryRecord>> {
    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare("SELECT * FROM deliveries ORDER BY delivery_id")?;
    let records = stmt
        .query_map([], read_record)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records)
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn provisional_bucket(occurred_at: &str) -> i64 {
    DateTime::parse_from_rfc3339(occurred_at)
        .map(|value| value.timestamp() / PROVISIONAL_BUCKET_SECONDS)
        .unwrap_or_else(|_| Utc::now().timestamp() / PROVISIONAL_BUCKET_SECONDS)
}

fn fnv1a_64(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ClaimStatus, DeliveryAttempt, DeliveryBusCoordinator, DeliveryChannel, DeliveryClaim,
        DeliveryKind, provisional_key_for_request, provisional_key_for_text,
    };
    use uuid::Uuid;

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("threadbridge-delivery-bus-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn provisional_claim_deduplicates_matching_text_bucket() {
        let root = temp_path();
        let bus = DeliveryBusCoordinator::new(&root).await.unwrap();
        let key = provisional_key_for_text(
            "sess-1",
            DeliveryKind::AssistantFinal,
            "hello world",
            "2026-03-26T09:23:57.800Z",
        );
        let first = bus
            .claim_delivery(DeliveryClaim {
                thread_key: "thread-1".to_owned(),
                session_id: "sess-1".to_owned(),
                turn_id: None,
                provisional_key: Some(key.clone()),
                channel: DeliveryChannel::Telegram,
                kind: DeliveryKind::AssistantFinal,
                owner: "thread_flow".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(first, ClaimStatus::Claimed(_)));
        let second = bus
            .claim_delivery(DeliveryClaim {
                thread_key: "thread-1".to_owned(),
                session_id: "sess-1".to_owned(),
                turn_id: None,
                provisional_key: Some(key),
                channel: DeliveryChannel::Telegram,
                kind: DeliveryKind::AssistantFinal,
                owner: "status_sync".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(second, ClaimStatus::Existing(_)));
    }

    #[tokio::test]
    async fn promote_and_commit_turn_bound_delivery() {
        let root = temp_path();
        let bus = DeliveryBusCoordinator::new(&root).await.unwrap();
        let provisional = provisional_key_for_request("sess-1", 17, "item-1");
        bus.claim_delivery(DeliveryClaim {
            thread_key: "thread-1".to_owned(),
            session_id: "sess-1".to_owned(),
            turn_id: None,
            provisional_key: Some(provisional.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::RequestUserInputPrompt,
            owner: "interactive".to_owned(),
        })
        .await
        .unwrap();
        let promoted = bus
            .promote_delivery_turn(
                "thread-1",
                "sess-1",
                &provisional,
                DeliveryChannel::Telegram,
                DeliveryKind::RequestUserInputPrompt,
                "turn-1",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.turn_id.as_deref(), Some("turn-1"));
        let committed = bus
            .commit_delivery(DeliveryAttempt {
                thread_key: "thread-1".to_owned(),
                session_id: "sess-1".to_owned(),
                turn_id: Some("turn-1".to_owned()),
                provisional_key: None,
                channel: DeliveryChannel::Telegram,
                kind: DeliveryKind::RequestUserInputPrompt,
                executor: "telegram_runtime".to_owned(),
                transport_ref: Some("message:42".to_owned()),
                report_json: json!({"targets":[{"type":"telegram_message"}]}),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(committed.state.as_str(), "committed");
        let records = bus.list_records().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].turn_id.as_deref(), Some("turn-1"));
    }

    #[tokio::test]
    async fn turn_bound_preview_draft_claim_deduplicates_same_turn() {
        let root = temp_path();
        let bus = DeliveryBusCoordinator::new(&root).await.unwrap();
        let first = bus
            .claim_delivery(DeliveryClaim {
                thread_key: "thread-1".to_owned(),
                session_id: "sess-1".to_owned(),
                turn_id: Some("turn-9".to_owned()),
                provisional_key: None,
                channel: DeliveryChannel::Telegram,
                kind: DeliveryKind::PreviewDraft,
                owner: "status_sync#1".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(first, ClaimStatus::Claimed(_)));

        let second = bus
            .claim_delivery(DeliveryClaim {
                thread_key: "thread-1".to_owned(),
                session_id: "sess-1".to_owned(),
                turn_id: Some("turn-9".to_owned()),
                provisional_key: None,
                channel: DeliveryChannel::Telegram,
                kind: DeliveryKind::PreviewDraft,
                owner: "status_sync#2".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(second, ClaimStatus::Existing(_)));
    }
}
