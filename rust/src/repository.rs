use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot, workspace_execution_mode};
use crate::image_artifacts::{ImageAnalysisArtifact, PendingImageBatch, PendingImageBatchEntry};

const MAIN_THREAD_KEY: &str = "main-thread";
const SESSION_BINDING_FILE_NAME: &str = "session-binding.json";
const TRANSCRIPT_MIRROR_FILE_NAME: &str = "transcript-mirror.jsonl";
const WORKSPACE_SESSION_HISTORY_FILE_NAME: &str = "workspace-session-history.json";
const WORKSPACE_SESSION_HISTORY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadScope {
    Main,
    Thread,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMetadata {
    pub archived_at: Option<String>,
    pub chat_id: i64,
    pub created_at: String,
    pub last_codex_turn_at: Option<String>,
    pub message_thread_id: Option<i32>,
    pub previous_message_thread_ids: Vec<i32>,
    pub scope: ThreadScope,
    pub session_broken: bool,
    pub session_broken_at: Option<String>,
    pub session_broken_reason: Option<String>,
    pub status: ThreadStatus,
    pub title: Option<String>,
    pub updated_at: String,
    pub thread_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBinding {
    pub schema_version: u32,
    #[serde(default)]
    pub current_codex_thread_id: Option<String>,
    #[serde(default)]
    pub current_execution_mode: Option<ExecutionMode>,
    #[serde(default)]
    pub current_approval_policy: Option<String>,
    #[serde(default)]
    pub current_sandbox_policy: Option<String>,
    pub workspace_cwd: Option<String>,
    pub bound_at: Option<String>,
    pub initialized_at: Option<String>,
    pub last_verified_at: Option<String>,
    pub session_broken: bool,
    pub session_broken_at: Option<String>,
    pub session_broken_reason: Option<String>,
    #[serde(default)]
    pub tui_active_codex_thread_id: Option<String>,
    #[serde(default)]
    pub tui_session_adoption_pending: bool,
    #[serde(default)]
    pub tui_session_adoption_prompt_message_id: Option<i32>,
    pub updated_at: String,
    #[serde(default, skip_serializing, rename = "codex_thread_id")]
    legacy_codex_thread_id: Option<String>,
    #[serde(default, skip_serializing, rename = "selected_session_id")]
    legacy_selected_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogDirection {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadLogEntry {
    pub timestamp: String,
    pub chat_id: i64,
    pub codex_thread_id: Option<String>,
    pub scope: ThreadScope,
    pub message_thread_id: Option<i32>,
    pub direction: LogDirection,
    pub text: String,
    pub user_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMirrorOrigin {
    Local,
    Telegram,
    Tui,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMirrorRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMirrorDelivery {
    Final,
    Process,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMirrorPhase {
    Plan,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptMirrorEntry {
    pub timestamp: String,
    pub session_id: String,
    pub origin: TranscriptMirrorOrigin,
    pub role: TranscriptMirrorRole,
    pub delivery: TranscriptMirrorDelivery,
    #[serde(default)]
    pub phase: Option<TranscriptMirrorPhase>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentCodexSessionEntry {
    pub session_id: String,
    pub updated_at: String,
    #[serde(default)]
    pub execution_mode: Option<ExecutionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct WorkspaceSessionHistoryStore {
    #[serde(default = "workspace_session_history_schema_version")]
    schema_version: u32,
    #[serde(default)]
    workspaces: Vec<WorkspaceSessionHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorkspaceSessionHistoryEntry {
    workspace_cwd: String,
    updated_at: String,
    #[serde(default)]
    sessions: Vec<RecentCodexSessionEntry>,
}

#[derive(Debug, Clone)]
pub struct ThreadRecord {
    pub conversation_key: String,
    pub folder_name: String,
    pub folder_path: PathBuf,
    pub log_path: PathBuf,
    pub metadata: ThreadMetadata,
    pub metadata_path: PathBuf,
}

impl ThreadRecord {
    pub fn state_path(&self) -> PathBuf {
        self.folder_path.join("state")
    }

    pub fn transcript_mirror_path(&self) -> PathBuf {
        self.state_path().join(TRANSCRIPT_MIRROR_FILE_NAME)
    }
}

#[derive(Debug, Clone)]
pub struct ThreadRepository {
    data_root_path: PathBuf,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn apply_execution_snapshot(binding: &mut SessionBinding, snapshot: &SessionExecutionSnapshot) {
    binding.current_execution_mode = snapshot.execution_mode;
    binding.current_approval_policy = snapshot.approval_policy.clone();
    binding.current_sandbox_policy = snapshot.sandbox_policy.clone();
}

fn workspace_session_history_schema_version() -> u32 {
    WORKSPACE_SESSION_HISTORY_SCHEMA_VERSION
}

fn canonical_workspace_string(workspace_path: &str) -> String {
    Path::new(workspace_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(workspace_path))
        .display()
        .to_string()
}

fn folder_name_for(scope: &ThreadScope, thread_key: &str) -> String {
    match scope {
        ThreadScope::Main => MAIN_THREAD_KEY.to_owned(),
        ThreadScope::Thread => thread_key.to_owned(),
    }
}

fn conversation_key_for(scope: &ThreadScope, thread_key: &str) -> String {
    match scope {
        ThreadScope::Main => MAIN_THREAD_KEY.to_owned(),
        ThreadScope::Thread => format!("thread:{thread_key}"),
    }
}

impl SessionBinding {
    fn normalize_legacy_fields(mut self) -> Self {
        if self.current_codex_thread_id.is_none() {
            self.current_codex_thread_id = self
                .legacy_selected_session_id
                .take()
                .or(self.legacy_codex_thread_id.take());
        }
        self.legacy_codex_thread_id = None;
        self.legacy_selected_session_id = None;
        self
    }

    fn fresh(
        workspace_cwd: Option<String>,
        current_codex_thread_id: Option<String>,
        execution: SessionExecutionSnapshot,
    ) -> Self {
        let now = now_iso();
        Self {
            schema_version: 3,
            current_codex_thread_id,
            current_execution_mode: execution.execution_mode,
            current_approval_policy: execution.approval_policy,
            current_sandbox_policy: execution.sandbox_policy,
            workspace_cwd,
            bound_at: Some(now.clone()),
            initialized_at: Some(now.clone()),
            last_verified_at: Some(now.clone()),
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            tui_active_codex_thread_id: None,
            tui_session_adoption_pending: false,
            tui_session_adoption_prompt_message_id: None,
            updated_at: now,
            legacy_codex_thread_id: None,
            legacy_selected_session_id: None,
        }
    }
}

impl ThreadRepository {
    pub async fn open(data_root_path: impl AsRef<Path>) -> Result<Self> {
        let data_root_path = data_root_path.as_ref().to_path_buf();
        fs::create_dir_all(&data_root_path).await?;
        Ok(Self { data_root_path })
    }

    pub async fn get_main_thread(&self, chat_id: i64) -> Result<ThreadRecord> {
        self.get_or_create(
            chat_id,
            ThreadScope::Main,
            None,
            None,
            MAIN_THREAD_KEY.to_owned(),
        )
        .await
    }

    pub async fn find_main_thread(&self) -> Result<Option<ThreadRecord>> {
        let folder_name = folder_name_for(&ThreadScope::Main, MAIN_THREAD_KEY);
        let metadata_path = self.data_root_path.join(&folder_name).join("metadata.json");
        if !fs::try_exists(&metadata_path).await? {
            return Ok(None);
        }
        let metadata: ThreadMetadata =
            serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
        if !matches!(metadata.scope, ThreadScope::Main) {
            return Ok(None);
        }
        Ok(Some(self.build_record(folder_name, metadata)))
    }

    pub async fn get_thread(&self, chat_id: i64, message_thread_id: i32) -> Result<ThreadRecord> {
        if let Some(record) = self
            .find_thread_by_message_thread_id(chat_id, message_thread_id)
            .await?
        {
            return Ok(record);
        }
        self.get_or_create(
            chat_id,
            ThreadScope::Thread,
            Some(message_thread_id),
            None,
            Uuid::new_v4().to_string(),
        )
        .await
    }

    pub async fn find_thread(
        &self,
        chat_id: i64,
        message_thread_id: i32,
    ) -> Result<Option<ThreadRecord>> {
        self.find_thread_by_message_thread_id(chat_id, message_thread_id)
            .await
    }

    pub async fn create_thread(
        &self,
        chat_id: i64,
        message_thread_id: i32,
        title: String,
    ) -> Result<ThreadRecord> {
        if let Some(record) = self
            .find_thread_by_message_thread_id(chat_id, message_thread_id)
            .await?
        {
            return self
                .update_metadata(ThreadRecord {
                    metadata: ThreadMetadata {
                        title: Some(title),
                        ..record.metadata.clone()
                    },
                    ..record
                })
                .await;
        }

        self.get_or_create(
            chat_id,
            ThreadScope::Thread,
            Some(message_thread_id),
            Some(title),
            Uuid::new_v4().to_string(),
        )
        .await
    }

    pub async fn append_log(
        &self,
        record: &ThreadRecord,
        direction: LogDirection,
        text: impl Into<String>,
        user_id: Option<i64>,
    ) -> Result<()> {
        let entry = ThreadLogEntry {
            timestamp: now_iso(),
            chat_id: record.metadata.chat_id,
            codex_thread_id: self
                .read_session_binding(record)
                .await?
                .and_then(|binding| binding.current_codex_thread_id),
            scope: record.metadata.scope.clone(),
            message_thread_id: record.metadata.message_thread_id,
            direction,
            text: text.into(),
            user_id,
        };
        let line = format!("{}\n", serde_json::to_string(&entry)?);
        let mut existing = String::new();
        if let Ok(content) = fs::read_to_string(&record.log_path).await {
            existing = content;
        }
        existing.push_str(&line);
        fs::write(&record.log_path, existing).await?;
        Ok(())
    }

    pub async fn append_transcript_mirror(
        &self,
        record: &ThreadRecord,
        entry: &TranscriptMirrorEntry,
    ) -> Result<bool> {
        let path = record.transcript_mirror_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut existing = String::new();
        if let Ok(content) = fs::read_to_string(&path).await {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let existing_entry: TranscriptMirrorEntry = serde_json::from_str(trimmed)?;
                if &existing_entry == entry {
                    return Ok(false);
                }
            }
            existing = content;
        }
        let line = format!("{}\n", serde_json::to_string(entry)?);
        existing.push_str(&line);
        fs::write(path, existing).await?;
        Ok(true)
    }

    pub async fn read_pending_image_batch(
        &self,
        record: &ThreadRecord,
    ) -> Result<Option<PendingImageBatch>> {
        let path = record.state_path().join("pending-image-batch.json");
        if !fs::try_exists(&path).await? {
            return Ok(None);
        }
        let batch = serde_json::from_str(&fs::read_to_string(&path).await?)?;
        Ok(Some(batch))
    }

    pub async fn get_or_create_pending_image_batch(
        &self,
        record: &ThreadRecord,
    ) -> Result<PendingImageBatch> {
        if let Some(batch) = self.read_pending_image_batch(record).await? {
            return Ok(batch);
        }
        let created_at = now_iso();
        let batch = PendingImageBatch {
            batch_id: format!(
                "batch-{}-{}",
                created_at.replace(['-', ':', '.'], ""),
                &Uuid::new_v4().to_string()[..8]
            ),
            control_message_id: None,
            created_at: created_at.clone(),
            images: Vec::new(),
            latest_caption: None,
            updated_at: created_at,
        };
        self.save_pending_image_batch(record, &batch).await?;
        Ok(batch)
    }

    pub async fn append_image_to_pending_batch(
        &self,
        record: &ThreadRecord,
        batch: PendingImageBatch,
        input: AppendPendingImageInput,
    ) -> Result<PendingImageBatch> {
        let batch_dir = record
            .state_path()
            .join("images")
            .join("source")
            .join(&batch.batch_id);
        fs::create_dir_all(&batch_dir).await?;
        let file_path = batch_dir.join(&input.file_name);
        fs::write(&file_path, input.data).await?;
        let entry = PendingImageBatchEntry {
            added_at: now_iso(),
            caption: input.caption.clone(),
            file_name: input.file_name,
            mime_type: input.mime_type,
            relative_path: file_path
                .strip_prefix(&record.folder_path)
                .unwrap_or(&file_path)
                .to_string_lossy()
                .to_string(),
            source_message_id: input.source_message_id,
            telegram_file_id: input.telegram_file_id,
        };
        let updated = PendingImageBatch {
            control_message_id: batch.control_message_id,
            batch_id: batch.batch_id,
            created_at: batch.created_at,
            images: {
                let mut images = batch.images;
                images.push(entry);
                images
            },
            latest_caption: input.caption.or(batch.latest_caption),
            updated_at: now_iso(),
        };
        self.save_pending_image_batch(record, &updated).await?;
        Ok(updated)
    }

    pub async fn set_pending_image_batch_control_message_id(
        &self,
        record: &ThreadRecord,
        batch: PendingImageBatch,
        control_message_id: i32,
    ) -> Result<PendingImageBatch> {
        let updated = PendingImageBatch {
            control_message_id: Some(control_message_id),
            updated_at: now_iso(),
            ..batch
        };
        self.save_pending_image_batch(record, &updated).await?;
        Ok(updated)
    }

    pub async fn clear_pending_image_batch(&self, record: &ThreadRecord) -> Result<()> {
        let path = record.state_path().join("pending-image-batch.json");
        if fs::try_exists(&path).await? {
            fs::remove_file(path).await?;
        }
        Ok(())
    }

    pub async fn write_image_analysis(
        &self,
        record: &ThreadRecord,
        artifact: &ImageAnalysisArtifact,
    ) -> Result<()> {
        let path = record
            .state_path()
            .join("images")
            .join("analysis")
            .join(format!("{}.json", artifact.batch_id));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(artifact)?),
        )
        .await?;
        Ok(())
    }

    pub async fn read_recent_transcript(
        &self,
        record: &ThreadRecord,
        limit: usize,
    ) -> Result<Vec<ThreadLogEntry>> {
        let content = match fs::read_to_string(&record.log_path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", record.log_path.display()));
            }
        };
        let mut entries = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: ThreadLogEntry = serde_json::from_str(trimmed)?;
            match entry.direction {
                LogDirection::User | LogDirection::Assistant => entries.push(entry),
                LogDirection::System => {}
            }
        }
        if entries.len() <= limit {
            return Ok(entries);
        }
        Ok(entries.split_off(entries.len() - limit))
    }

    pub async fn read_transcript_mirror(
        &self,
        record: &ThreadRecord,
        delivery: Option<TranscriptMirrorDelivery>,
        limit: usize,
    ) -> Result<Vec<TranscriptMirrorEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let path = record.transcript_mirror_path();
        let content = match fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let mut entries = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: TranscriptMirrorEntry = serde_json::from_str(trimmed)?;
            if delivery
                .as_ref()
                .is_some_and(|expected| expected != &entry.delivery)
            {
                continue;
            }
            entries.push(entry);
        }
        if entries.len() <= limit {
            return Ok(entries);
        }
        Ok(entries.split_off(entries.len() - limit))
    }

    pub async fn read_session_binding(
        &self,
        record: &ThreadRecord,
    ) -> Result<Option<SessionBinding>> {
        let path = self.session_binding_path(record);
        match fs::read_to_string(&path).await {
            Ok(content) => {
                let binding: SessionBinding = serde_json::from_str(&content)?;
                Ok(Some(binding.normalize_legacy_fields()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    pub async fn bind_workspace(
        &self,
        record: ThreadRecord,
        workspace_cwd: String,
        codex_thread_id: String,
        execution: SessionExecutionSnapshot,
    ) -> Result<ThreadRecord> {
        let now = now_iso();
        let mut binding =
            SessionBinding::fresh(Some(workspace_cwd), Some(codex_thread_id), execution);
        binding.updated_at = now.clone();
        if let (Some(workspace_cwd), Some(session_id)) = (
            binding.workspace_cwd.as_deref(),
            binding.current_codex_thread_id.as_deref(),
        ) {
            self.record_recent_workspace_session(
                workspace_cwd,
                session_id,
                binding.current_execution_mode,
            )
            .await?;
        }
        self.write_session_binding(&record, &binding).await?;
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                last_codex_turn_at: Some(now),
                session_broken: false,
                session_broken_at: None,
                session_broken_reason: None,
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn mark_session_binding_verified(
        &self,
        record: ThreadRecord,
    ) -> Result<ThreadRecord> {
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        let now = now_iso();
        binding.last_verified_at = Some(now.clone());
        binding.session_broken = false;
        binding.session_broken_at = None;
        binding.session_broken_reason = None;
        binding.updated_at = now.clone();
        self.write_session_binding(&record, &binding).await?;
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                last_codex_turn_at: Some(now),
                session_broken: false,
                session_broken_at: None,
                session_broken_reason: None,
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn select_session_binding_session(
        &self,
        record: ThreadRecord,
        session_id: impl Into<String>,
    ) -> Result<ThreadRecord> {
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        let now = now_iso();
        let session_id = session_id.into();
        binding.current_codex_thread_id = Some(session_id);
        binding.last_verified_at = Some(now.clone());
        binding.session_broken = false;
        binding.session_broken_at = None;
        binding.session_broken_reason = None;
        binding.tui_active_codex_thread_id = None;
        binding.tui_session_adoption_pending = false;
        binding.tui_session_adoption_prompt_message_id = None;
        binding.updated_at = now.clone();
        if let (Some(workspace_cwd), Some(session_id)) = (
            binding.workspace_cwd.as_deref(),
            binding.current_codex_thread_id.as_deref(),
        ) {
            self.record_recent_workspace_session(
                workspace_cwd,
                session_id,
                binding.current_execution_mode,
            )
            .await?;
        }
        self.write_session_binding(&record, &binding).await?;
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                last_codex_turn_at: Some(now),
                session_broken: false,
                session_broken_at: None,
                session_broken_reason: None,
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn mark_session_binding_broken(
        &self,
        record: ThreadRecord,
        reason: impl Into<String>,
    ) -> Result<ThreadRecord> {
        let reason = reason.into();
        let now = now_iso();
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .unwrap_or_else(|| {
                SessionBinding::fresh(None, None, SessionExecutionSnapshot::default())
            });
        binding.session_broken = true;
        binding.session_broken_at = Some(now.clone());
        binding.session_broken_reason = Some(reason.clone());
        binding.updated_at = now.clone();
        self.write_session_binding(&record, &binding).await?;
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                session_broken: true,
                session_broken_at: Some(now),
                session_broken_reason: Some(reason),
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn archive_thread(&self, record: ThreadRecord) -> Result<ThreadRecord> {
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                archived_at: Some(now_iso()),
                status: ThreadStatus::Archived,
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn restore_thread(
        &self,
        record: ThreadRecord,
        message_thread_id: i32,
        title: String,
    ) -> Result<ThreadRecord> {
        let mut previous = record.metadata.previous_message_thread_ids.clone();
        if let Some(current) = record.metadata.message_thread_id {
            if current != message_thread_id && !previous.contains(&current) {
                previous.push(current);
            }
        }
        self.update_metadata(ThreadRecord {
            metadata: ThreadMetadata {
                archived_at: None,
                message_thread_id: Some(message_thread_id),
                previous_message_thread_ids: previous,
                status: ThreadStatus::Active,
                title: Some(title),
                ..record.metadata.clone()
            },
            ..record
        })
        .await
    }

    pub async fn list_archived_threads(&self, chat_id: i64) -> Result<Vec<ThreadRecord>> {
        let mut dir = fs::read_dir(&self.data_root_path).await?;
        let mut records = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let metadata_path = path.join("metadata.json");
            if !fs::try_exists(&metadata_path).await? {
                continue;
            }
            let metadata: ThreadMetadata =
                serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
            if matches!(metadata.scope, ThreadScope::Thread)
                && metadata.chat_id == chat_id
                && matches!(metadata.status, ThreadStatus::Archived)
            {
                records.push(
                    self.build_record(entry.file_name().to_string_lossy().to_string(), metadata),
                );
            }
        }
        records.sort_by(|a, b| b.metadata.archived_at.cmp(&a.metadata.archived_at));
        Ok(records)
    }

    pub async fn list_all_archived_threads(&self) -> Result<Vec<ThreadRecord>> {
        let mut dir = fs::read_dir(&self.data_root_path).await?;
        let mut records = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let metadata_path = path.join("metadata.json");
            if !fs::try_exists(&metadata_path).await? {
                continue;
            }
            let metadata: ThreadMetadata =
                serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
            if matches!(metadata.scope, ThreadScope::Thread)
                && matches!(metadata.status, ThreadStatus::Archived)
            {
                records.push(
                    self.build_record(entry.file_name().to_string_lossy().to_string(), metadata),
                );
            }
        }
        records.sort_by(|a, b| b.metadata.archived_at.cmp(&a.metadata.archived_at));
        Ok(records)
    }

    pub async fn list_active_threads(&self) -> Result<Vec<ThreadRecord>> {
        let mut dir = fs::read_dir(&self.data_root_path).await?;
        let mut records = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let metadata_path = path.join("metadata.json");
            if !fs::try_exists(&metadata_path).await? {
                continue;
            }
            let metadata: ThreadMetadata =
                serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
            if matches!(metadata.scope, ThreadScope::Thread)
                && matches!(metadata.status, ThreadStatus::Active)
            {
                records.push(
                    self.build_record(entry.file_name().to_string_lossy().to_string(), metadata),
                );
            }
        }
        records.sort_by(|a, b| a.metadata.created_at.cmp(&b.metadata.created_at));
        Ok(records)
    }

    pub async fn get_thread_by_key(
        &self,
        chat_id: i64,
        thread_key: &str,
    ) -> Result<Option<ThreadRecord>> {
        let folder_name = folder_name_for(&ThreadScope::Thread, thread_key);
        let metadata_path = self.data_root_path.join(&folder_name).join("metadata.json");
        if !fs::try_exists(&metadata_path).await? {
            return Ok(None);
        }
        let metadata: ThreadMetadata =
            serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
        if metadata.chat_id != chat_id {
            return Ok(None);
        }
        Ok(Some(self.build_record(folder_name, metadata)))
    }

    pub async fn find_active_thread_by_key(
        &self,
        thread_key: &str,
    ) -> Result<Option<ThreadRecord>> {
        let folder_name = folder_name_for(&ThreadScope::Thread, thread_key);
        let metadata_path = self.data_root_path.join(&folder_name).join("metadata.json");
        if !fs::try_exists(&metadata_path).await? {
            return Ok(None);
        }
        let metadata: ThreadMetadata =
            serde_json::from_str(&fs::read_to_string(&metadata_path).await?)?;
        if !matches!(metadata.scope, ThreadScope::Thread)
            || !matches!(metadata.status, ThreadStatus::Active)
        {
            return Ok(None);
        }
        Ok(Some(self.build_record(folder_name, metadata)))
    }

    pub async fn find_active_threads_by_workspace(
        &self,
        workspace_cwd: &str,
    ) -> Result<Vec<ThreadRecord>> {
        let target = canonical_workspace_string(workspace_cwd);
        let mut records = Vec::new();
        for record in self.list_active_threads().await? {
            let Some(binding) = self.read_session_binding(&record).await? else {
                continue;
            };
            let Some(bound_workspace) = binding.workspace_cwd.as_deref() else {
                continue;
            };
            if canonical_workspace_string(bound_workspace) == target {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn data_root_path(&self) -> &Path {
        &self.data_root_path
    }

    pub async fn set_tui_active_session_for_thread_key(
        &self,
        thread_key: &str,
        session_id: impl Into<String>,
    ) -> Result<Option<ThreadRecord>> {
        let Some(record) = self.find_active_thread_by_key(thread_key).await? else {
            return Ok(None);
        };
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        let now = now_iso();
        binding.tui_active_codex_thread_id = Some(session_id.into());
        binding.tui_session_adoption_pending = false;
        binding.tui_session_adoption_prompt_message_id = None;
        binding.updated_at = now;
        if let (Some(workspace_cwd), Some(session_id)) = (
            binding.workspace_cwd.as_deref(),
            binding.tui_active_codex_thread_id.as_deref(),
        ) {
            let mode = workspace_execution_mode(Path::new(workspace_cwd))
                .await
                .unwrap_or_default();
            self.record_recent_workspace_session(workspace_cwd, session_id, Some(mode))
                .await?;
        }
        self.write_session_binding(&record, &binding).await?;
        Ok(Some(record))
    }

    pub async fn read_recent_workspace_sessions(
        &self,
        workspace_cwd: &str,
    ) -> Result<Vec<RecentCodexSessionEntry>> {
        let store = self.read_workspace_session_history_store().await?;
        let target = canonical_workspace_string(workspace_cwd);
        Ok(store
            .workspaces
            .into_iter()
            .find(|entry| canonical_workspace_string(&entry.workspace_cwd) == target)
            .map(|entry| entry.sessions)
            .unwrap_or_default())
    }

    pub async fn mark_tui_adoption_pending_for_thread_key(
        &self,
        thread_key: &str,
    ) -> Result<Option<ThreadRecord>> {
        let Some(record) = self.find_active_thread_by_key(thread_key).await? else {
            return Ok(None);
        };
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        let should_prompt = binding.tui_active_codex_thread_id.is_some()
            && binding.tui_active_codex_thread_id != binding.current_codex_thread_id;
        binding.tui_session_adoption_pending = should_prompt;
        binding.tui_session_adoption_prompt_message_id = None;
        binding.updated_at = now_iso();
        self.write_session_binding(&record, &binding).await?;
        Ok(Some(record))
    }

    pub async fn set_tui_adoption_prompt_message_id(
        &self,
        record: ThreadRecord,
        message_id: i32,
    ) -> Result<ThreadRecord> {
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        binding.tui_session_adoption_prompt_message_id = Some(message_id);
        binding.updated_at = now_iso();
        self.write_session_binding(&record, &binding).await?;
        Ok(record)
    }

    pub async fn clear_tui_adoption_state(&self, record: ThreadRecord) -> Result<ThreadRecord> {
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        binding.tui_active_codex_thread_id = None;
        binding.tui_session_adoption_pending = false;
        binding.tui_session_adoption_prompt_message_id = None;
        binding.updated_at = now_iso();
        self.write_session_binding(&record, &binding).await?;
        Ok(record)
    }

    pub async fn adopt_tui_active_session(&self, record: ThreadRecord) -> Result<ThreadRecord> {
        let binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        let session_id = binding
            .tui_active_codex_thread_id
            .clone()
            .context("tui_active_codex_thread_id is missing")?;
        let workspace_cwd = binding.workspace_cwd.clone();
        let updated = self
            .select_session_binding_session(record, session_id)
            .await?;
        let Some(workspace_cwd) = workspace_cwd else {
            return Ok(updated);
        };
        let mode = workspace_execution_mode(Path::new(&workspace_cwd))
            .await
            .unwrap_or_default();
        self.update_session_execution_snapshot(updated, &SessionExecutionSnapshot::from_mode(mode))
            .await
    }

    pub async fn update_session_execution_snapshot(
        &self,
        record: ThreadRecord,
        snapshot: &SessionExecutionSnapshot,
    ) -> Result<ThreadRecord> {
        let mut binding = self
            .read_session_binding(&record)
            .await?
            .context("session binding is missing")?;
        apply_execution_snapshot(&mut binding, snapshot);
        binding.updated_at = now_iso();
        if let (Some(workspace_cwd), Some(session_id)) = (
            binding.workspace_cwd.as_deref(),
            binding.current_codex_thread_id.as_deref(),
        ) {
            self.record_recent_workspace_session(
                workspace_cwd,
                session_id,
                binding.current_execution_mode,
            )
            .await?;
        }
        self.write_session_binding(&record, &binding).await?;
        Ok(record)
    }

    pub async fn update_metadata(&self, record: ThreadRecord) -> Result<ThreadRecord> {
        let updated = ThreadMetadata {
            updated_at: now_iso(),
            ..record.metadata.clone()
        };
        fs::write(
            &record.metadata_path,
            format!("{}\n", serde_json::to_string_pretty(&updated)?),
        )
        .await?;
        Ok(ThreadRecord {
            metadata: updated,
            ..record
        })
    }

    async fn get_or_create(
        &self,
        chat_id: i64,
        scope: ThreadScope,
        message_thread_id: Option<i32>,
        title: Option<String>,
        thread_key: String,
    ) -> Result<ThreadRecord> {
        let folder_name = folder_name_for(&scope, &thread_key);
        let folder_path = self.data_root_path.join(&folder_name);
        let metadata_path = folder_path.join("metadata.json");
        if fs::try_exists(&metadata_path).await? {
            return self.load_record(folder_name).await;
        }

        fs::create_dir_all(&folder_path).await?;
        let created_at = now_iso();
        let metadata = ThreadMetadata {
            archived_at: None,
            chat_id,
            created_at: created_at.clone(),
            last_codex_turn_at: None,
            message_thread_id,
            previous_message_thread_ids: Vec::new(),
            scope: scope.clone(),
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            status: ThreadStatus::Active,
            title,
            updated_at: created_at,
            thread_key: thread_key.clone(),
        };
        let record = self.build_record(folder_name, metadata);
        fs::write(
            &record.metadata_path,
            format!("{}\n", serde_json::to_string_pretty(&record.metadata)?),
        )
        .await?;
        fs::write(&record.log_path, "").await?;
        Ok(record)
    }

    async fn save_pending_image_batch(
        &self,
        record: &ThreadRecord,
        batch: &PendingImageBatch,
    ) -> Result<()> {
        let state_dir = record.state_path();
        fs::create_dir_all(&state_dir).await?;
        let path = state_dir.join("pending-image-batch.json");
        fs::write(path, format!("{}\n", serde_json::to_string_pretty(batch)?)).await?;
        Ok(())
    }

    async fn write_session_binding(
        &self,
        record: &ThreadRecord,
        session: &SessionBinding,
    ) -> Result<()> {
        let path = self.session_binding_path(record);
        let session = session.clone().normalize_legacy_fields();
        fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&session)?),
        )
        .await?;
        Ok(())
    }

    fn session_binding_path(&self, record: &ThreadRecord) -> PathBuf {
        record.folder_path.join(SESSION_BINDING_FILE_NAME)
    }

    fn workspace_session_history_path(&self) -> PathBuf {
        self.data_root_path
            .join(WORKSPACE_SESSION_HISTORY_FILE_NAME)
    }

    async fn load_record(&self, folder_name: String) -> Result<ThreadRecord> {
        let metadata_path = self.data_root_path.join(&folder_name).join("metadata.json");
        let metadata: ThreadMetadata = serde_json::from_str(
            &fs::read_to_string(&metadata_path)
                .await
                .with_context(|| format!("failed to read {}", metadata_path.display()))?,
        )?;
        Ok(self.build_record(folder_name, metadata))
    }

    fn build_record(&self, folder_name: String, metadata: ThreadMetadata) -> ThreadRecord {
        let folder_path = self.data_root_path.join(&folder_name);
        ThreadRecord {
            conversation_key: conversation_key_for(&metadata.scope, &metadata.thread_key),
            folder_name,
            log_path: folder_path.join("conversations.jsonl"),
            metadata_path: folder_path.join("metadata.json"),
            folder_path,
            metadata,
        }
    }

    async fn find_thread_by_message_thread_id(
        &self,
        chat_id: i64,
        message_thread_id: i32,
    ) -> Result<Option<ThreadRecord>> {
        let mut dir = fs::read_dir(&self.data_root_path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let metadata_path = path.join("metadata.json");
            if !fs::try_exists(&metadata_path).await? {
                continue;
            }
            let metadata: ThreadMetadata = serde_json::from_str(
                &fs::read_to_string(&metadata_path)
                    .await
                    .with_context(|| format!("failed to read {}", metadata_path.display()))?,
            )?;
            if matches!(metadata.scope, ThreadScope::Thread)
                && metadata.chat_id == chat_id
                && metadata.message_thread_id == Some(message_thread_id)
            {
                return Ok(Some(self.build_record(
                    entry.file_name().to_string_lossy().to_string(),
                    metadata,
                )));
            }
        }
        Ok(None)
    }

    async fn record_recent_workspace_session(
        &self,
        workspace_cwd: &str,
        session_id: &str,
        execution_mode: Option<ExecutionMode>,
    ) -> Result<()> {
        let mut store = self.read_workspace_session_history_store().await?;
        let workspace_cwd = canonical_workspace_string(workspace_cwd);
        let now = now_iso();
        let entry = store
            .workspaces
            .iter_mut()
            .find(|entry| canonical_workspace_string(&entry.workspace_cwd) == workspace_cwd);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                store.workspaces.push(WorkspaceSessionHistoryEntry {
                    workspace_cwd: workspace_cwd.clone(),
                    updated_at: now.clone(),
                    sessions: Vec::new(),
                });
                store
                    .workspaces
                    .last_mut()
                    .expect("workspace entry inserted")
            }
        };
        entry.updated_at = now.clone();
        entry
            .sessions
            .retain(|entry| entry.session_id != session_id);
        entry.sessions.insert(
            0,
            RecentCodexSessionEntry {
                session_id: session_id.to_owned(),
                updated_at: now,
                execution_mode,
            },
        );
        if entry.sessions.len() > 5 {
            entry.sessions.truncate(5);
        }
        self.write_workspace_session_history_store(&store).await
    }

    async fn read_workspace_session_history_store(&self) -> Result<WorkspaceSessionHistoryStore> {
        let path = self.workspace_session_history_path();
        match fs::read_to_string(&path).await {
            Ok(content) => Ok(serde_json::from_str(&content)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(WorkspaceSessionHistoryStore {
                    schema_version: WORKSPACE_SESSION_HISTORY_SCHEMA_VERSION,
                    workspaces: Vec::new(),
                })
            }
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    async fn write_workspace_session_history_store(
        &self,
        store: &WorkspaceSessionHistoryStore,
    ) -> Result<()> {
        let path = self.workspace_session_history_path();
        fs::write(&path, format!("{}\n", serde_json::to_string_pretty(store)?))
            .await
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

#[derive(Debug, Clone)]
pub struct AppendPendingImageInput {
    pub caption: Option<String>,
    pub data: Vec<u8>,
    pub file_name: String,
    pub mime_type: String,
    pub source_message_id: i32,
    pub telegram_file_id: String,
}

#[cfg(test)]
mod tests {
    use super::{
        AppendPendingImageInput, SessionBinding, ThreadRepository, ThreadScope, ThreadStatus,
        TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
        TranscriptMirrorPhase, TranscriptMirrorRole,
    };
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::image_artifacts::ImageAnalysisArtifact;
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-repo-test-{}", Uuid::new_v4()))
    }

    fn full_auto_snapshot() -> SessionExecutionSnapshot {
        SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto)
    }

    #[tokio::test]
    async fn create_thread_uses_minimal_thread_root_layout() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();

        assert!(
            fs::try_exists(record.folder_path.join("metadata.json"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(record.folder_path.join("conversations.jsonl"))
                .await
                .unwrap()
        );
        assert!(
            !fs::try_exists(record.folder_path.join("workspace"))
                .await
                .unwrap()
        );
        assert!(
            !fs::try_exists(record.folder_path.join("AGENTS.md"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn bind_workspace_persists_new_session_shape() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let updated = repo
            .bind_workspace(
                record,
                "/tmp/workspace".to_owned(),
                "thr_123".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let binding = repo.read_session_binding(&updated).await.unwrap().unwrap();
        assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_123"));
        assert_eq!(binding.tui_active_codex_thread_id, None);
        assert!(!binding.tui_session_adoption_pending);
        assert_eq!(binding.workspace_cwd.as_deref(), Some("/tmp/workspace"));
        assert!(!binding.session_broken);
    }

    #[tokio::test]
    async fn select_session_rewrites_current_codex_thread_id() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let record = repo
            .bind_workspace(
                record,
                "/tmp/workspace".to_owned(),
                "thr_bot".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let updated = repo
            .select_session_binding_session(record, "thr_cli".to_owned())
            .await
            .unwrap();
        let binding = repo.read_session_binding(&updated).await.unwrap().unwrap();
        assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_cli"));
        assert_eq!(binding.tui_active_codex_thread_id, None);
        assert!(!binding.tui_session_adoption_pending);
    }

    #[test]
    fn session_binding_ignores_legacy_attachment_state_key() {
        let binding: SessionBinding = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "workspace_cwd": "/tmp/workspace",
            "current_codex_thread_id": "thr_123",
            "bound_at": "2026-03-22T00:00:00.000Z",
            "initialized_at": "2026-03-22T00:00:00.000Z",
            "last_verified_at": "2026-03-22T00:00:00.000Z",
            "session_broken": false,
            "session_broken_at": null,
            "session_broken_reason": null,
            "tui_active_codex_thread_id": null,
            "tui_session_adoption_pending": false,
            "tui_session_adoption_prompt_message_id": null,
            "updated_at": "2026-03-22T00:00:00.000Z",
            "attachment_state": "local_handoff"
        }))
        .unwrap();

        assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_123"));
        assert_eq!(binding.workspace_cwd.as_deref(), Some("/tmp/workspace"));
        assert!(!binding.tui_session_adoption_pending);
    }

    #[tokio::test]
    async fn recent_workspace_sessions_track_current_and_tui_activity() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_initial".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        let record = repo
            .select_session_binding_session(record, "thr_second".to_owned())
            .await
            .unwrap();
        let _ = repo
            .set_tui_active_session_for_thread_key(&record.metadata.thread_key, "thr_tui")
            .await
            .unwrap();

        let sessions = repo
            .read_recent_workspace_sessions(&workspace.display().to_string())
            .await
            .unwrap();
        assert_eq!(
            sessions
                .iter()
                .map(|entry| entry.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["thr_tui", "thr_second", "thr_initial"]
        );
    }

    #[tokio::test]
    async fn find_active_threads_by_workspace_groups_conflicts() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();

        let first = repo.create_thread(1, 7, "One".to_owned()).await.unwrap();
        let second = repo.create_thread(1, 8, "Two".to_owned()).await.unwrap();
        let _ = repo
            .bind_workspace(
                first,
                workspace.display().to_string(),
                "thr_one".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        let _ = repo
            .bind_workspace(
                second,
                workspace.display().to_string(),
                "thr_two".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let records = repo
            .find_active_threads_by_workspace(&workspace.display().to_string())
            .await
            .unwrap();
        assert_eq!(records.len(), 2);
    }

    #[tokio::test]
    async fn tui_adoption_state_roundtrip() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let record = repo
            .bind_workspace(
                record,
                "/tmp/workspace".to_owned(),
                "thr_bot".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let _ = repo
            .set_tui_active_session_for_thread_key(&record.metadata.thread_key, "thr_tui")
            .await
            .unwrap();
        let record = repo
            .mark_tui_adoption_pending_for_thread_key(&record.metadata.thread_key)
            .await
            .unwrap()
            .unwrap();
        let binding = repo.read_session_binding(&record).await.unwrap().unwrap();
        assert_eq!(
            binding.tui_active_codex_thread_id.as_deref(),
            Some("thr_tui")
        );
        assert!(binding.tui_session_adoption_pending);

        let record = repo
            .set_tui_adoption_prompt_message_id(record, 42)
            .await
            .unwrap();
        let binding = repo.read_session_binding(&record).await.unwrap().unwrap();
        assert_eq!(binding.tui_session_adoption_prompt_message_id, Some(42));

        let record = repo.adopt_tui_active_session(record).await.unwrap();
        let binding = repo.read_session_binding(&record).await.unwrap().unwrap();
        assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_tui"));
        assert_eq!(binding.tui_active_codex_thread_id, None);
        assert!(!binding.tui_session_adoption_pending);
        assert_eq!(binding.tui_session_adoption_prompt_message_id, None);
    }

    #[tokio::test]
    async fn pending_image_batch_roundtrip_uses_state_directory() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let batch = repo
            .get_or_create_pending_image_batch(&record)
            .await
            .unwrap();
        let updated = repo
            .append_image_to_pending_batch(
                &record,
                batch,
                AppendPendingImageInput {
                    caption: Some("caption".to_owned()),
                    data: vec![1, 2, 3],
                    file_name: "image.png".to_owned(),
                    mime_type: "image/png".to_owned(),
                    source_message_id: 11,
                    telegram_file_id: "file".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.images.len(), 1);
        assert!(
            fs::try_exists(record.state_path().join("pending-image-batch.json"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn append_transcript_mirror_deduplicates_existing_entry() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let entry = TranscriptMirrorEntry {
            timestamp: "2026-03-21T00:00:00.000Z".to_owned(),
            session_id: "thr_123".to_owned(),
            origin: TranscriptMirrorOrigin::Tui,
            role: TranscriptMirrorRole::Assistant,
            delivery: TranscriptMirrorDelivery::Final,
            phase: None,
            text: "Hi.".to_owned(),
        };

        let inserted_first = repo
            .append_transcript_mirror(&record, &entry)
            .await
            .unwrap();
        let inserted_second = repo
            .append_transcript_mirror(&record, &entry)
            .await
            .unwrap();

        assert!(inserted_first);
        assert!(!inserted_second);

        let content = fs::read_to_string(record.transcript_mirror_path())
            .await
            .unwrap();
        let lines: Vec<_> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 1);
    }

    #[tokio::test]
    async fn read_transcript_mirror_filters_by_delivery_and_limit() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let entries = [
            TranscriptMirrorEntry {
                timestamp: "2026-03-21T00:00:00.000Z".to_owned(),
                session_id: "thr_123".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "hi".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-21T00:00:01.000Z".to_owned(),
                session_id: "thr_123".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Process,
                phase: Some(TranscriptMirrorPhase::Tool),
                text: "Command: cargo test".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-21T00:00:02.000Z".to_owned(),
                session_id: "thr_123".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "done".to_owned(),
            },
        ];
        for entry in &entries {
            repo.append_transcript_mirror(&record, entry).await.unwrap();
        }

        let process = repo
            .read_transcript_mirror(&record, Some(TranscriptMirrorDelivery::Process), 10)
            .await
            .unwrap();
        assert_eq!(process.len(), 1);
        assert_eq!(process[0].text, "Command: cargo test");

        let latest = repo.read_transcript_mirror(&record, None, 2).await.unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].text, "Command: cargo test");
        assert_eq!(latest[1].text, "done");
    }

    #[tokio::test]
    async fn write_image_analysis_stays_under_state() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let artifact = ImageAnalysisArtifact {
            batch_id: "batch-1".to_owned(),
            created_at: "2026-03-17T00:00:00.000Z".to_owned(),
            image_count: 1,
            images: Vec::new(),
            prompt: "prompt".to_owned(),
            result_text: "result".to_owned(),
        };
        repo.write_image_analysis(&record, &artifact).await.unwrap();
        assert!(
            fs::try_exists(record.state_path().join("images/analysis/batch-1.json"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn archive_and_restore_are_local_only() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Title".to_owned()).await.unwrap();
        let archived = repo.archive_thread(record).await.unwrap();
        assert!(matches!(archived.metadata.status, ThreadStatus::Archived));
        let restored = repo
            .restore_thread(archived, 9, "Restored".to_owned())
            .await
            .unwrap();
        assert!(matches!(restored.metadata.status, ThreadStatus::Active));
        assert_eq!(restored.metadata.message_thread_id, Some(9));
    }

    #[tokio::test]
    async fn find_thread_does_not_create_missing_thread() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        assert!(repo.find_thread(1, 7).await.unwrap().is_none());
        let entries = repo.list_active_threads().await.unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn thread_scope_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ThreadScope::Thread).unwrap(),
            "\"thread\""
        );
    }
}
