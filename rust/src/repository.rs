use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::image_artifacts::{ImageAnalysisArtifact, PendingImageBatch, PendingImageBatchEntry};

const MAIN_THREAD_KEY: &str = "main-thread";
const SESSION_BINDING_FILE_NAME: &str = "session-binding.json";
const TRANSCRIPT_MIRROR_FILE_NAME: &str = "transcript-mirror.jsonl";

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
    #[serde(default, skip_serializing, rename = "attachment_state")]
    legacy_attachment_state: Option<SessionAttachmentState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttachmentState {
    #[default]
    None,
    CliHandoff,
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
    Cli,
    Telegram,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptMirrorEntry {
    pub timestamp: String,
    pub session_id: String,
    pub origin: TranscriptMirrorOrigin,
    pub role: TranscriptMirrorRole,
    pub delivery: TranscriptMirrorDelivery,
    pub text: String,
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
        self.legacy_attachment_state = None;
        self
    }

    fn fresh(workspace_cwd: Option<String>, current_codex_thread_id: Option<String>) -> Self {
        let now = now_iso();
        Self {
            schema_version: 3,
            current_codex_thread_id,
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
            legacy_attachment_state: None,
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
    ) -> Result<()> {
        let path = record.transcript_mirror_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let line = format!("{}\n", serde_json::to_string(entry)?);
        let mut existing = String::new();
        if let Ok(content) = fs::read_to_string(&path).await {
            existing = content;
        }
        existing.push_str(&line);
        fs::write(path, existing).await?;
        Ok(())
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
    ) -> Result<ThreadRecord> {
        let now = now_iso();
        let mut binding = SessionBinding::fresh(Some(workspace_cwd), Some(codex_thread_id));
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
            .unwrap_or_else(|| SessionBinding::fresh(None, None));
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

    pub fn data_root_path(&self) -> &Path {
        &self.data_root_path
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
    use super::{AppendPendingImageInput, ThreadRepository, ThreadScope, ThreadStatus};
    use crate::image_artifacts::ImageAnalysisArtifact;
    use std::path::PathBuf;
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-repo-test-{}", Uuid::new_v4()))
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
            .bind_workspace(record, "/tmp/workspace".to_owned(), "thr_123".to_owned())
            .await
            .unwrap();

        let binding = repo.read_session_binding(&updated).await.unwrap().unwrap();
        assert_eq!(
            binding.current_codex_thread_id.as_deref(),
            Some("thr_123")
        );
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
            .bind_workspace(record, "/tmp/workspace".to_owned(), "thr_bot".to_owned())
            .await
            .unwrap();

        let updated = repo
            .select_session_binding_session(record, "thr_cli".to_owned())
            .await
            .unwrap();
        let binding = repo.read_session_binding(&updated).await.unwrap().unwrap();
        assert_eq!(
            binding.current_codex_thread_id.as_deref(),
            Some("thr_cli")
        );
        assert_eq!(binding.tui_active_codex_thread_id, None);
        assert!(!binding.tui_session_adoption_pending);
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
