use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use teloxide::dispatching::UpdateFilterExt;
use teloxide::dptree;
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{
    BotCommand, CallbackQuery, ChatAction, FileId, InlineKeyboardButton, InlineKeyboardMarkup,
    ThreadId,
};
use teloxide::utils::command::BotCommands;
use threadbridge_rust::codex::{CodexInputItem, CodexRunner, CodexThreadEvent, CodexWorkspace};
use threadbridge_rust::codex_home::CodexHome;
use threadbridge_rust::config::{AppConfig, load_app_config};
use threadbridge_rust::image_artifacts::{
    ImageAnalysisArtifact, ImageAnalysisImage, build_image_analysis_prompt,
    render_pending_image_batch,
};
use threadbridge_rust::logging::init_json_logs;
use threadbridge_rust::repository::{
    AppendPendingImageInput, ConversationRecord, ConversationRepository, ConversationStatus,
    LogDirection, SessionBinding,
};
use threadbridge_rust::tool_results::{
    TelegramOutboxItem, parse_build_prompt_config_tool_result, parse_generate_image_tool_result,
    parse_telegram_outbox,
};
use threadbridge_rust::workspace::{ensure_linked_workspace_runtime, validate_seed_template};
use tokio::sync::{Mutex, oneshot};
use tracing::{error, info, warn};

#[derive(Clone, BotCommands)]
#[command(rename_rule = "snake_case")]
enum Command {
    #[command(description = "Show commands for the control chat and bound threads")]
    Start,
    #[command(description = "Create a new thread")]
    NewThread,
    #[command(description = "List recent Codex sessions from the local Codex home")]
    ListSessions,
    #[command(description = "Bind this Telegram thread to an existing Codex session id")]
    BindSession,
    #[command(description = "Generate a title for the current thread from chat history")]
    GenerateTitle,
    #[command(description = "Summarize the current conversation using Codex")]
    SummarizeThread,
    #[command(description = "Archive the current thread")]
    ArchiveThread,
    #[command(description = "Show archived threads and restore one interactively")]
    RestoreThread,
    #[command(description = "Build concept.json and prompt configs for the current thread")]
    BuildPromptConfig,
    #[command(description = "Generate images from the current thread workspace")]
    GenerateImage,
    #[command(description = "Update the current thread AGENTS.md from session context")]
    UpdateAgentsMd,
    #[command(description = "Revalidate the current bound Codex session for this thread")]
    ReconnectCodex,
}

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    repository: ConversationRepository,
    codex: CodexRunner,
    codex_home: CodexHome,
    seed_template_path: PathBuf,
}

const RESTORE_PAGE_SIZE: usize = 8;
const CALLBACK_RESTORE_PICK: &str = "restore_pick";
const CALLBACK_RESTORE_PAGE: &str = "restore_page";
const CALLBACK_IMAGE_BATCH_ANALYZE: &str = "image_batch_analyze";
const BUILD_PROMPT_RESULT_FILE: &str = "tool_results/build_prompt_config.result.json";
const GENERATE_IMAGE_RESULT_FILE: &str = "tool_results/generate_image.result.json";
const TELEGRAM_OUTBOX_FILE: &str = "tool_results/telegram_outbox.json";
const TYPING_HEARTBEAT_SECONDS: u64 = 4;
const PREVIEW_HEARTBEAT_SECONDS: u64 = 4;
static NEXT_PREVIEW_DRAFT_ID: AtomicI32 = AtomicI32::new(1);

struct TypingHeartbeat {
    stop_tx: Option<oneshot::Sender<()>>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl TypingHeartbeat {
    fn start(bot: Bot, chat_id: ChatId, thread_id: Option<ThreadId>) -> Self {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            loop {
                let request = bot.send_chat_action(chat_id, ChatAction::Typing);
                let _ = match thread_id {
                    Some(thread_id) => request.message_thread_id(thread_id).await,
                    None => request.await,
                };
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(TYPING_HEARTBEAT_SECONDS)) => {}
                    _ = &mut stop_rx => break,
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            join_handle,
        }
    }

    async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        let _ = self.join_handle.await;
    }
}

struct PreviewHeartbeat {
    stop_tx: Option<oneshot::Sender<()>>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl PreviewHeartbeat {
    fn start(preview: std::sync::Arc<Mutex<TurnPreviewController>>) -> Self {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(PREVIEW_HEARTBEAT_SECONDS)) => {
                        preview.lock().await.heartbeat().await;
                    }
                    _ = &mut stop_rx => break,
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            join_handle,
        }
    }

    async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        let _ = self.join_handle.await;
    }
}

fn summarize_text(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let truncated: String = compact.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}

fn truncate_preserving_layout(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_owned();
    }
    chars
        .into_iter()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn next_preview_draft_id() -> i32 {
    let draft_id = NEXT_PREVIEW_DRAFT_ID.fetch_add(1, Ordering::Relaxed);
    if draft_id <= 0 {
        NEXT_PREVIEW_DRAFT_ID.store(2, Ordering::Relaxed);
        return 1;
    }
    draft_id
}

#[derive(Debug, Serialize)]
struct SendMessageDraftRequest {
    chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i32>,
    draft_id: i32,
    text: String,
}

#[derive(Debug, Deserialize)]
struct TelegramMethodEnvelope {
    ok: bool,
    description: Option<String>,
}

async fn send_message_draft(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    draft_id: i32,
    text: &str,
) -> Result<()> {
    let mut url = bot.api_url();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("telegram api url cannot be a base url"))?;
        segments.push(&format!("bot{}", bot.token()));
        segments.push("sendMessageDraft");
    }

    let payload = SendMessageDraftRequest {
        chat_id: chat_id.0,
        message_thread_id: thread_id.map(thread_id_to_i32),
        draft_id,
        text: text.to_owned(),
    };
    let response = bot
        .client()
        .post(url)
        .json(&payload)
        .send()
        .await
        .context("failed to call Telegram sendMessageDraft")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read Telegram sendMessageDraft response")?;
    let envelope: TelegramMethodEnvelope = serde_json::from_str(&body)
        .with_context(|| format!("invalid Telegram sendMessageDraft response: {body}"))?;
    if !status.is_success() || !envelope.ok {
        anyhow::bail!(
            "Telegram sendMessageDraft failed (HTTP {}): {}",
            status,
            envelope
                .description
                .unwrap_or_else(|| "unknown Telegram error".to_owned())
        );
    }
    Ok(())
}

fn preview_status(label: &str) -> String {
    label.to_owned()
}

fn preview_heartbeat_marker(frame: usize) -> &'static str {
    const FRAMES: [&str; 2] = ["●", "○"];
    FRAMES[frame % FRAMES.len()]
}

struct PreviewRenderer {
    status: String,
    status_frame: usize,
    draft_text: String,
    final_response: String,
    latest_render: String,
    max_chars: usize,
    in_progress: bool,
}

impl PreviewRenderer {
    fn new(max_chars: usize, _command_output_tail_chars: usize) -> Self {
        Self {
            status: preview_status("Preparing reply..."),
            status_frame: 0,
            draft_text: String::new(),
            final_response: String::new(),
            latest_render: String::new(),
            max_chars,
            in_progress: true,
        }
    }

    fn consume(&mut self, event: &CodexThreadEvent) -> bool {
        match event {
            CodexThreadEvent::ThreadStarted { .. } => {
                self.in_progress = true;
                self.status = preview_status("Preparing reply...")
            }
            CodexThreadEvent::TurnStarted => {
                self.in_progress = true;
                self.status = preview_status("Reading context...");
            }
            CodexThreadEvent::TurnCompleted { .. } => {
                self.in_progress = false;
                self.status = preview_status("Finalizing...");
            }
            CodexThreadEvent::TurnFailed { error } => {
                self.in_progress = false;
                self.status = format!(
                    "Preview unavailable\n{}",
                    summarize_text(&error.to_string(), 120)
                );
            }
            CodexThreadEvent::Error { message } => {
                self.in_progress = false;
                self.status = format!("Preview unavailable\n{}", summarize_text(message, 120));
            }
            CodexThreadEvent::ItemStarted { item }
            | CodexThreadEvent::ItemUpdated { item }
            | CodexThreadEvent::ItemCompleted { item } => {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("agent_message") => {
                        let text = item
                            .get("text")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_owned();
                        if !text.is_empty() {
                            self.draft_text = text.clone();
                            self.final_response = text;
                            self.status = preview_status("Drafting...");
                        }
                    }
                    Some("reasoning") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Thinking...");
                        }
                    }
                    Some("command_execution") | Some("mcp_tool_call") | Some("web_search") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Using tools...");
                        }
                    }
                    Some("todo_list") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Planning...");
                        }
                    }
                    _ => {}
                }
            }
        }
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    fn heartbeat(&mut self) -> bool {
        if !self.in_progress {
            return false;
        }
        self.status_frame = self.status_frame.wrapping_add(1);
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    fn render_text(&self) -> String {
        let body = if !self.draft_text.is_empty() {
            self.draft_text.as_str()
        } else {
            self.status.as_str()
        };
        let text = if self.in_progress {
            format!("{} {}", preview_heartbeat_marker(self.status_frame), body)
        } else {
            body.to_owned()
        };
        truncate_preserving_layout(&text, self.max_chars)
    }

    fn get_render_text(&self) -> &str {
        &self.latest_render
    }

    fn get_final_response(&self) -> &str {
        &self.final_response
    }
}

struct TurnPreviewController {
    bot: Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    renderer: PreviewRenderer,
    draft_id: i32,
    last_sent_text: String,
    last_update_at: Option<Instant>,
    edit_interval: Duration,
}

impl TurnPreviewController {
    fn new(
        bot: Bot,
        chat_id: ChatId,
        thread_id: Option<ThreadId>,
        max_chars: usize,
        command_output_tail_chars: usize,
        edit_interval_ms: u64,
    ) -> Self {
        Self {
            bot,
            chat_id,
            thread_id,
            renderer: PreviewRenderer::new(max_chars, command_output_tail_chars),
            draft_id: next_preview_draft_id(),
            last_sent_text: String::new(),
            last_update_at: None,
            edit_interval: Duration::from_millis(edit_interval_ms),
        }
    }

    async fn consume(&mut self, event: &CodexThreadEvent) {
        if !self.renderer.consume(event) {
            return;
        }
        self.flush_render().await;
    }

    async fn heartbeat(&mut self) {
        if !self.renderer.heartbeat() {
            return;
        }
        self.flush_render().await;
    }

    async fn flush_render(&mut self) {
        let render_text = self.renderer.get_render_text().trim();
        if render_text.is_empty() || render_text == self.last_sent_text {
            return;
        }
        if let Some(last_update_at) = self.last_update_at {
            if last_update_at.elapsed() < self.edit_interval {
                return;
            }
        }
        let capped = truncate_preserving_layout(render_text, 4096);
        match send_message_draft(
            &self.bot,
            self.chat_id,
            self.thread_id,
            self.draft_id,
            &capped,
        )
        .await
        {
            Ok(()) => {
                self.last_sent_text = capped;
                self.last_update_at = Some(Instant::now());
            }
            Err(error) => {
                warn!(
                    event = "telegram.preview.draft.failed",
                    chat_id = self.chat_id.0,
                    draft_id = self.draft_id,
                    error = %error,
                    "failed to send preview draft"
                );
            }
        }
    }

    async fn complete(&mut self, _final_text: &str) -> bool {
        false
    }

    fn fallback_final_response(&self) -> &str {
        self.renderer.get_final_response()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_app_config()?;
    let _guard = init_json_logs(&config.runtime.debug_log_path)?;
    let repository = ConversationRepository::open(&config.runtime.data_root_path).await?;
    let codex_home = CodexHome::discover()?;
    let seed_template_path = validate_seed_template(
        &config
            .runtime
            .codex_working_directory
            .join("templates")
            .join("AGENTS.md"),
    )?;
    let state = AppState {
        codex: CodexRunner::new(config.runtime.codex_model.clone()),
        codex_home,
        repository,
        seed_template_path,
        config: config.clone(),
    };
    let bot = Bot::new(config.telegram_token.clone());

    bot.set_my_commands(command_list()).await?;
    info!(
        event = "bot.started",
        commands = ?command_list().into_iter().map(|c| c.command).collect::<Vec<_>>(),
        data_root_path = %config.runtime.data_root_path.display(),
        debug_log_path = %config.runtime.debug_log_path.display(),
        working_directory = %config.runtime.codex_working_directory.display(),
        "Telegram bot is running."
    );

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .branch(
                    dptree::entry()
                        .filter_command::<Command>()
                        .endpoint(handle_command),
                )
                .branch(dptree::endpoint(handle_message)),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback_query));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

fn command_list() -> Vec<BotCommand> {
    Command::bot_commands()
        .into_iter()
        .filter(|command| {
            !matches!(
                command.command.as_str(),
                "build_prompt_config" | "generate_image"
            )
        })
        .collect()
}

fn is_authorized(state: &AppState, msg: &Message) -> bool {
    msg.from
        .as_ref()
        .map(|user| {
            state
                .config
                .authorized_user_ids
                .contains(&(user.id.0 as i64))
        })
        .unwrap_or(false)
}

fn is_control_chat(msg: &Message) -> bool {
    matches!(&msg.chat.kind, teloxide::types::ChatKind::Private(_)) && msg.thread_id.is_none()
}

fn thread_id_to_i32(thread_id: ThreadId) -> i32 {
    thread_id.0.0
}

async fn send_scoped_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    let text = text.into();
    let request = bot.send_message(chat_id, text);
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await,
        None => request.await,
    }
}

fn usable_bound_session_id(session: Option<&SessionBinding>) -> Option<&str> {
    session
        .filter(|session| !session.session_broken)
        .and_then(|session| session.codex_session_id.as_deref())
}

fn session_binding_hint(session: Option<&SessionBinding>) -> &'static str {
    match session {
        Some(session) if session.session_broken => {
            "This thread's bound Codex session is invalid. Use /reconnect_codex to revalidate it or /bind_session <session_id> to attach another one."
        }
        _ => {
            "This thread is not bound to a Codex session. Use /list_sessions, then /bind_session <session_id>."
        }
    }
}

fn command_argument_text<'a>(msg: &'a Message, command_name: &str) -> Option<&'a str> {
    let text = msg.text()?.trim();
    let prefix = format!("/{command_name}");
    let remainder = text.strip_prefix(&prefix)?.trim();
    if remainder.is_empty() {
        None
    } else {
        Some(remainder)
    }
}

fn format_session_list_text(
    sessions: &[threadbridge_rust::codex_home::CodexSessionSummary],
) -> String {
    if sessions.is_empty() {
        return "No recent Codex sessions were found in ~/.codex.".to_owned();
    }

    let mut lines = vec!["Recent Codex sessions:".to_owned()];
    for session in sessions {
        lines.push(format!("- `{}`  {}", session.id, session.title.trim()));
    }
    lines.push("Bind one in a thread with /bind_session <session_id>.".to_owned());
    lines.join("\n")
}

async fn ensure_bound_workspace_runtime(
    state: &AppState,
    record: &ConversationRecord,
    binding: &SessionBinding,
) -> Result<PathBuf> {
    let session_id = binding
        .codex_session_id
        .as_deref()
        .context("session binding is missing a Codex session id")?;
    let resolved = state
        .codex_home
        .resolve_session(session_id)?
        .with_context(|| format!("Codex session not found in ~/.codex: {session_id}"))?;
    ensure_linked_workspace_runtime(
        &state.config.runtime.codex_working_directory,
        &state.seed_template_path,
        &record.workspace_link_path(),
        &resolved.cwd,
    )
    .await?;
    Ok(record.workspace_link_path())
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    command: Command,
    state: AppState,
) -> ResponseResult<()> {
    if !is_authorized(&state, &msg) {
        return Ok(());
    }
    if let Err(error) = run_command(&bot, &msg, command, &state).await {
        error!(event = "telegram.command.failed", error = %error, chat_id = msg.chat.id.0);
        let _ = send_scoped_message(
            &bot,
            msg.chat.id,
            msg.thread_id,
            format!("Command failed: {error}"),
        )
        .await;
    }
    Ok(())
}

#[derive(Clone)]
struct IncomingImage {
    caption: Option<String>,
    file_id: FileId,
    file_name: String,
    mime_type: String,
}

fn file_extension_for_image(mime_type: &str, file_name: Option<&str>) -> String {
    if let Some(file_name) = file_name {
        if let Some((_, ext)) = file_name.rsplit_once('.') {
            if !ext.is_empty() {
                return format!(".{}", ext.to_lowercase());
            }
        }
    }
    match mime_type {
        "image/jpeg" => ".jpg".to_owned(),
        "image/gif" => ".gif".to_owned(),
        "image/webp" => ".webp".to_owned(),
        _ => ".png".to_owned(),
    }
}

fn extract_incoming_image(msg: &Message) -> Option<IncomingImage> {
    let caption = msg
        .caption()
        .map(|value| value.trim().to_owned())
        .filter(|v| !v.is_empty());
    if let Some(photo) = msg.photo() {
        let best = photo.last()?;
        return Some(IncomingImage {
            caption,
            file_id: best.file.id.clone(),
            file_name: format!("photo_{}.jpg", msg.id.0),
            mime_type: "image/jpeg".to_owned(),
        });
    }
    if let Some(document) = msg.document() {
        let mime_type = document.mime_type.as_ref()?.essence_str().to_owned();
        if !mime_type.starts_with("image/") {
            return None;
        }
        let ext = file_extension_for_image(&mime_type, document.file_name.as_deref());
        let stem = document
            .file_name
            .as_deref()
            .and_then(|name| name.rsplit_once('.').map(|(left, _)| left.to_owned()))
            .unwrap_or_else(|| format!("document_{}", msg.id.0));
        return Some(IncomingImage {
            caption,
            file_id: document.file.id.clone(),
            file_name: format!("{stem}{ext}"),
            mime_type,
        });
    }
    None
}

async fn download_telegram_file(state: &AppState, bot: &Bot, file_id: FileId) -> Result<Vec<u8>> {
    let file = bot.get_file(file_id).await?;
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        state.config.telegram_token, file.path
    );
    let response = reqwest::get(url).await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("Failed to download Telegram file: HTTP {status}");
    }
    Ok(response.bytes().await?.to_vec())
}

async fn upsert_pending_image_batch_message(
    bot: &Bot,
    state: &AppState,
    chat_id: ChatId,
    thread_id: ThreadId,
    record: &threadbridge_rust::repository::ConversationRecord,
    batch: threadbridge_rust::image_artifacts::PendingImageBatch,
) -> Result<threadbridge_rust::image_artifacts::PendingImageBatch> {
    let text = render_pending_image_batch(&batch);
    let markup = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "直接分析",
        format!("{CALLBACK_IMAGE_BATCH_ANALYZE}:{}", batch.batch_id),
    )]]);
    if let Some(control_message_id) = batch.control_message_id {
        if bot
            .edit_message_text(
                chat_id,
                teloxide::types::MessageId(control_message_id),
                text.clone(),
            )
            .reply_markup(markup.clone())
            .await
            .is_ok()
        {
            return Ok(batch);
        }
    }
    let message = bot
        .send_message(chat_id, text)
        .message_thread_id(thread_id)
        .reply_markup(markup)
        .await?;
    state
        .repository
        .set_pending_image_batch_control_message_id(record, batch, message.id.0)
        .await
}

async fn read_file_or_none(path: impl Into<PathBuf>) -> Result<Option<String>> {
    let path = path.into();
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

async fn read_build_prompt_result(
    record: &threadbridge_rust::repository::ConversationRecord,
) -> Result<Option<(String, String)>> {
    let Some(result_text) =
        read_file_or_none(record.folder_path.join(BUILD_PROMPT_RESULT_FILE)).await?
    else {
        return Ok(None);
    };
    let result = parse_build_prompt_config_tool_result(&result_text)?;
    Ok(Some((result.concept_path, result.prompt_path)))
}

async fn read_generate_image_result(
    record: &threadbridge_rust::repository::ConversationRecord,
) -> Result<Option<threadbridge_rust::tool_results::GenerateImageToolResult>> {
    let Some(result_text) =
        read_file_or_none(record.folder_path.join(GENERATE_IMAGE_RESULT_FILE)).await?
    else {
        return Ok(None);
    };
    Ok(Some(parse_generate_image_tool_result(&result_text)?))
}

async fn read_telegram_outbox(
    record: &threadbridge_rust::repository::ConversationRecord,
) -> Result<Option<threadbridge_rust::tool_results::TelegramOutbox>> {
    let Some(result_text) =
        read_file_or_none(record.folder_path.join(TELEGRAM_OUTBOX_FILE)).await?
    else {
        return Ok(None);
    };
    Ok(Some(parse_telegram_outbox(&result_text)?))
}

async fn remove_file_if_exists(path: impl Into<PathBuf>) -> Result<()> {
    let path = path.into();
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn resolve_workspace_file_path(
    record: &threadbridge_rust::repository::ConversationRecord,
    relative_path: &str,
) -> PathBuf {
    record.folder_path.join(relative_path)
}

async fn dispatch_workspace_telegram_outbox(
    bot: &Bot,
    state: &AppState,
    record: &threadbridge_rust::repository::ConversationRecord,
    thread_id: ThreadId,
) -> Result<()> {
    let Some(outbox) = read_telegram_outbox(record).await? else {
        return Ok(());
    };
    if outbox.items.is_empty() {
        remove_file_if_exists(record.folder_path.join(TELEGRAM_OUTBOX_FILE)).await?;
        return Ok(());
    }

    for item in &outbox.items {
        match item {
            TelegramOutboxItem::Text { text } => {
                send_scoped_message(
                    bot,
                    ChatId(record.metadata.chat_id),
                    Some(thread_id),
                    text.clone(),
                )
                .await?;
            }
            TelegramOutboxItem::Photo { path, caption } => {
                let request = bot
                    .send_photo(
                        ChatId(record.metadata.chat_id),
                        teloxide::types::InputFile::file(resolve_workspace_file_path(record, path)),
                    )
                    .message_thread_id(thread_id);
                if let Some(caption) = caption {
                    request.caption(caption.clone()).await?;
                } else {
                    request.await?;
                }
            }
            TelegramOutboxItem::Document { path, caption } => {
                let request = bot
                    .send_document(
                        ChatId(record.metadata.chat_id),
                        teloxide::types::InputFile::file(resolve_workspace_file_path(record, path)),
                    )
                    .message_thread_id(thread_id);
                if let Some(caption) = caption {
                    request.caption(caption.clone()).await?;
                } else {
                    request.await?;
                }
            }
        }
    }

    remove_file_if_exists(record.folder_path.join(TELEGRAM_OUTBOX_FILE)).await?;
    state
        .repository
        .append_log(
            record,
            LogDirection::System,
            format!(
                "Dispatched {} Telegram outbox item(s) from workspace runtime.",
                outbox.items.len()
            ),
            None,
        )
        .await?;
    Ok(())
}

async fn send_generated_images(
    bot: &Bot,
    record: &threadbridge_rust::repository::ConversationRecord,
    thread_id: ThreadId,
    image_paths: &[String],
    summary: &str,
) -> Result<()> {
    for (index, relative_path) in image_paths.iter().enumerate() {
        let absolute_path = record.folder_path.join(relative_path);
        let request = bot
            .send_photo(
                ChatId(record.metadata.chat_id),
                teloxide::types::InputFile::file(absolute_path),
            )
            .message_thread_id(thread_id);
        if index == 0 {
            request.caption(summary.to_owned()).await?;
        } else {
            request.await?;
        }
    }
    Ok(())
}

async fn run_command(bot: &Bot, msg: &Message, command: Command, state: &AppState) -> Result<()> {
    match command {
        Command::Start => {
            let text = if is_control_chat(msg) {
                "Control console.\nUse /new_thread to create a thread."
            } else {
                "Thread workspace.\nUse /bind_session <session_id> after you pick a Codex session."
            };
            send_scoped_message(bot, msg.chat.id, msg.thread_id, text).await?;
        }
        Command::NewThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /new_thread from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let title = format!("Thread {}", chrono::Local::now().format("%m-%d %H:%M"));
            let topic = bot.create_forum_topic(msg.chat.id, title.clone()).await?;
            let record = state
                .repository
                .create_thread(
                    msg.chat.id.0,
                    thread_id_to_i32(topic.thread_id),
                    title.clone(),
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                None,
                format!("Created thread \"{}\".", topic.name),
            )
            .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    "Telegram thread created. Awaiting Codex session binding.",
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(topic.thread_id),
                "Thread created.\n\nUse /list_sessions to inspect local Codex sessions, then /bind_session <session_id> in this thread.",
            )
            .await?;
        }
        Command::ListSessions => {
            let sessions = state.codex_home.list_recent_sessions(8)?;
            send_scoped_message(
                bot,
                msg.chat.id,
                msg.thread_id,
                format_session_list_text(&sessions),
            )
            .await?;
        }
        Command::BindSession => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /bind_session <session_id> inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let Some(session_id) = command_argument_text(msg, "bind_session") else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Usage: /bind_session <session_id>",
                )
                .await?;
                return Ok(());
            };
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ConversationStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let resolved = match state.codex_home.resolve_session(session_id)? {
                Some(session) => session,
                None => {
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!("Codex session not found: `{session_id}`"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let workspace_path = record.workspace_link_path();
            ensure_linked_workspace_runtime(
                &state.config.runtime.codex_working_directory,
                &state.seed_template_path,
                &workspace_path,
                &resolved.cwd,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let verification = state
                .codex
                .reconnect_session(
                    &CodexWorkspace {
                        working_directory: workspace_path.clone(),
                    },
                    &resolved.id,
                )
                .await;
            typing.stop().await;
            let record = state
                .repository
                .bind_session(
                    record,
                    resolved.id.clone(),
                    Some(resolved.title.clone()),
                    resolved.cwd.display().to_string(),
                )
                .await?;
            match verification {
                Ok(_) => {
                    let record = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!(
                                "Bound Telegram thread to Codex session {} ({})",
                                resolved.id,
                                resolved.cwd.display()
                            ),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!(
                            "Bound to Codex session `{}`.\nWorkspace: `{}`",
                            resolved.id,
                            resolved.cwd.display()
                        ),
                    )
                    .await?;
                }
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    state
                        .repository
                        .append_log(
                            &record,
                            LogDirection::System,
                            format!("Codex session bind verification failed: {error}"),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        format!(
                            "Session was linked but verification failed.\nUse /reconnect_codex to retry or /bind_session <session_id> to attach another one.\n\nError: {error}"
                        ),
                    )
                    .await?;
                }
            }
        }
        Command::ReconnectCodex => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /reconnect_codex inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnect = state
                .codex
                .reconnect_session(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            match reconnect {
                Ok(_) => {
                    let updated = state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            "Codex session revalidated using the bound session id.",
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session reconnected for this thread.",
                    )
                    .await?;
                }
                Err(error) => {
                    let updated = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    state
                        .repository
                        .append_log(
                            &updated,
                            LogDirection::System,
                            format!("Codex reconnect failed: {error}"),
                            None,
                        )
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session revalidation failed. Use /bind_session <session_id> to reattach or /reconnect_codex to retry the current one.",
                    )
                    .await?;
                }
            }
        }
        Command::GenerateTitle => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /generate_title inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ConversationStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .generate_thread_title_from_session(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let mut updated = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let title = result.final_response.trim().to_owned();
            updated.metadata.title = Some(title.clone());
            let updated = state.repository.update_metadata(updated).await?;
            state
                .repository
                .append_log(
                    &updated,
                    LogDirection::System,
                    format!("Generated title: {title}"),
                    None,
                )
                .await?;
            let _ = bot
                .edit_forum_topic(msg.chat.id, thread_id)
                .name(title.clone())
                .await;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                format!("Title updated: {title}"),
            )
            .await?;
        }
        Command::SummarizeThread => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /summarize_thread inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            if matches!(record.metadata.status, ConversationStatus::Archived) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    "This thread is archived.",
                )
                .await?;
                return Ok(());
            }
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .summarize_thread_from_session(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let record = state
                .repository
                .write_summary(record, result.final_response.clone())
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::Assistant,
                    result.final_response.clone(),
                    None,
                )
                .await?;
            send_scoped_message(bot, msg.chat.id, Some(thread_id), result.final_response).await?;
        }
        Command::ArchiveThread => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /archive_thread inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let _ = bot.delete_forum_topic(msg.chat.id, thread_id).await;
            let record = state.repository.archive_thread(record).await?;
            state
                .repository
                .append_log(&record, LogDirection::System, "Thread archived.", None)
                .await?;
        }
        Command::BuildPromptConfig => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /build_prompt_config inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .build_prompt_config(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            if let Some((concept_path, prompt_path)) = read_build_prompt_result(&record).await? {
                let summary = format!(
                    "Built prompt config artifacts for this workspace:\n- {}\n- {}",
                    concept_path, prompt_path
                );
                state
                    .repository
                    .append_log(&record, LogDirection::Assistant, summary.clone(), None)
                    .await?;
                send_scoped_message(bot, msg.chat.id, Some(thread_id), summary).await?;
            } else {
                state
                    .repository
                    .append_log(
                        &record,
                        LogDirection::Assistant,
                        result.final_response.clone(),
                        None,
                    )
                    .await?;
                send_scoped_message(bot, msg.chat.id, Some(thread_id), result.final_response)
                    .await?;
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
        }
        Command::GenerateImage => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /generate_image inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .generate_image(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            if let Some(image_result) = read_generate_image_result(&record).await? {
                let summary = format!(
                    "Generated image artifacts for this workspace:\n- Prompt: {}\n- Output: {}\n- Images: {}",
                    image_result.prompt_path, image_result.run_dir, image_result.image_count
                );
                send_generated_images(bot, &record, thread_id, &image_result.image_paths, &summary)
                    .await?;
                state
                    .repository
                    .append_log(&record, LogDirection::Assistant, summary, None)
                    .await?;
            } else {
                state
                    .repository
                    .append_log(
                        &record,
                        LogDirection::Assistant,
                        result.final_response.clone(),
                        None,
                    )
                    .await?;
                send_scoped_message(bot, msg.chat.id, Some(thread_id), result.final_response)
                    .await?;
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
        }
        Command::UpdateAgentsMd => {
            if is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    None,
                    "Use /update_agents_md inside a thread.",
                )
                .await?;
                return Ok(());
            }
            let thread_id = msg.thread_id.context("thread message missing thread id")?;
            let record = state
                .repository
                .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            let session = state.repository.read_session_binding(&record).await?;
            let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    Some(thread_id),
                    session_binding_hint(session.as_ref()),
                )
                .await?;
                return Ok(());
            };
            let workspace_path = ensure_bound_workspace_runtime(
                state,
                &record,
                session.as_ref().context("missing session binding")?,
            )
            .await?;
            let agents_path = workspace_path.join("AGENTS.md");
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let result = state
                .codex
                .update_agents_md(
                    &CodexWorkspace {
                        working_directory: workspace_path.clone(),
                    },
                    existing_thread_id,
                    &agents_path,
                )
                .await;
            typing.stop().await;
            match result {
                Ok(_) => {}
                Err(error) => {
                    let record = state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    let _ = record;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Codex session is unavailable. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            }
            let record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
            let reconnected = state
                .codex
                .reconnect_session(
                    &CodexWorkspace {
                        working_directory: workspace_path,
                    },
                    existing_thread_id,
                )
                .await;
            typing.stop().await;
            let record = match reconnected {
                Ok(_) => {
                    state
                        .repository
                        .mark_session_binding_verified(record)
                        .await?
                }
                Err(error) => {
                    state
                        .repository
                        .mark_session_binding_broken(record, error.to_string())
                        .await?;
                    send_scoped_message(
                        bot,
                        msg.chat.id,
                        Some(thread_id),
                        "Updated AGENTS.md, but reconnecting the bound Codex session failed. Use /reconnect_codex or /bind_session <session_id>.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::System,
                    "Updated AGENTS.md and reconnected Codex.",
                    None,
                )
                .await?;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                "Updated this thread's AGENTS.md from session context and reconnected Codex.",
            )
            .await?;
        }
        Command::RestoreThread => {
            if !is_control_chat(msg) {
                send_scoped_message(
                    bot,
                    msg.chat.id,
                    msg.thread_id,
                    "Use /restore_thread from the main private chat.",
                )
                .await?;
                return Ok(());
            }
            let (text, markup) = render_restore_page(state, msg.chat.id.0, 0).await?;
            bot.send_message(msg.chat.id, text)
                .reply_markup(markup)
                .await?;
        }
    }
    Ok(())
}

async fn queue_image_for_thread(
    bot: &Bot,
    msg: &Message,
    state: &AppState,
    image: IncomingImage,
) -> Result<()> {
    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if matches!(record.metadata.status, ConversationStatus::Archived) {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This thread is archived.",
        )
        .await?;
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    if usable_bound_session_id(session.as_ref()).is_none() {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint(session.as_ref()),
        )
        .await?;
        return Ok(());
    }
    let _ = ensure_bound_workspace_runtime(
        state,
        &record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;
    let pending = state
        .repository
        .get_or_create_pending_image_batch(&record)
        .await?;
    let data = download_telegram_file(state, bot, image.file_id.clone()).await?;
    let updated = state
        .repository
        .append_image_to_pending_batch(
            &record,
            pending,
            AppendPendingImageInput {
                caption: image.caption.clone(),
                data,
                file_name: image.file_name.clone(),
                mime_type: image.mime_type.clone(),
                source_message_id: msg.id.0,
                telegram_file_id: image.file_id.0,
            },
        )
        .await?;
    let persisted =
        upsert_pending_image_batch_message(bot, state, msg.chat.id, thread_id, &record, updated)
            .await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            match image.caption {
                Some(caption) => format!("[image] {} | caption: {}", image.file_name, caption),
                None => format!("[image] {}", image.file_name),
            },
            msg.from.as_ref().map(|u| u.id.0 as i64),
        )
        .await?;
    if persisted.control_message_id.is_some() {
        state
            .repository
            .append_log(
                &record,
                LogDirection::System,
                format!(
                    "Image batch updated: {} image(s) waiting for analysis.",
                    persisted.images.len()
                ),
                None,
            )
            .await?;
    }
    Ok(())
}

async fn analyze_pending_image_batch(
    bot: &Bot,
    state: &AppState,
    record: threadbridge_rust::repository::ConversationRecord,
    thread_id: ThreadId,
    batch_id: &str,
    user_prompt: Option<&str>,
    callback_query_id: Option<&teloxide::types::CallbackQueryId>,
) -> Result<()> {
    if matches!(record.metadata.status, ConversationStatus::Archived) {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text("This thread is archived.")
                .show_alert(true)
                .await?;
        }
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        if let Some(callback_query_id) = callback_query_id {
            bot.answer_callback_query(callback_query_id.clone())
                .text(session_binding_hint(session.as_ref()))
                .show_alert(true)
                .await?;
        }
        return Ok(());
    };
    let workspace_path = ensure_bound_workspace_runtime(
        state,
        &record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;
    let Some(batch) = state.repository.read_pending_image_batch(&record).await? else {
        return Ok(());
    };
    if batch.batch_id != batch_id || batch.images.is_empty() {
        return Ok(());
    }
    if let Some(callback_query_id) = callback_query_id {
        bot.answer_callback_query(callback_query_id.clone())
            .text("Starting image analysis...")
            .await?;
    }
    let typing = TypingHeartbeat::start(
        bot.clone(),
        ChatId(record.metadata.chat_id),
        Some(thread_id),
    );
    let prompt = build_image_analysis_prompt(&batch, user_prompt);
    let preview = std::sync::Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        ChatId(record.metadata.chat_id),
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());
    let mut input = vec![CodexInputItem::Text {
        text: prompt.clone(),
    }];
    for image in &batch.images {
        input.push(CodexInputItem::LocalImage {
            path: record
                .folder_path
                .join(&image.relative_path)
                .display()
                .to_string(),
        });
    }
    let result = state
        .codex
        .run_locked_with_events(
            &CodexWorkspace {
                working_directory: workspace_path,
            },
            existing_thread_id,
            input,
            |event| {
                let preview = preview.clone();
                async move {
                    preview.lock().await.consume(&event).await;
                }
            },
        )
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            preview_heartbeat.stop().await;
            typing.stop().await;
            let record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            let _ = record;
            return Err(error);
        }
    };
    let record = state
        .repository
        .mark_session_binding_verified(record)
        .await?;
    let preview_finalized = preview.lock().await.complete(&result.final_response).await;
    preview_heartbeat.stop().await;
    typing.stop().await;
    let artifact = ImageAnalysisArtifact {
        batch_id: batch.batch_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        image_count: batch.images.len(),
        images: batch
            .images
            .iter()
            .map(|image| ImageAnalysisImage {
                file_name: image.file_name.clone(),
                mime_type: image.mime_type.clone(),
                relative_path: image.relative_path.clone(),
                source_message_id: image.source_message_id,
            })
            .collect(),
        prompt,
        result_text: if result.final_response.trim().is_empty() {
            let fallback = preview
                .lock()
                .await
                .fallback_final_response()
                .trim()
                .to_owned();
            if fallback.is_empty() {
                "Codex completed image analysis without a final answer.".to_owned()
            } else {
                fallback
            }
        } else {
            result.final_response.trim().to_owned()
        },
    };
    state
        .repository
        .write_image_analysis(&record, &artifact)
        .await?;
    state.repository.clear_pending_image_batch(&record).await?;
    state
        .repository
        .append_log(
            &record,
            LogDirection::Assistant,
            artifact.result_text.clone(),
            None,
        )
        .await?;
    if !preview_finalized {
        send_scoped_message(
            bot,
            ChatId(record.metadata.chat_id),
            Some(thread_id),
            artifact.result_text,
        )
        .await?;
    }
    dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
    return Ok(());
}

async fn handle_message(bot: Bot, msg: Message, state: AppState) -> ResponseResult<()> {
    if !is_authorized(&state, &msg) {
        return Ok(());
    }
    if let Some(image) = extract_incoming_image(&msg) {
        if let Err(error) = queue_image_for_thread(&bot, &msg, &state, image).await {
            error!(event = "telegram.image.failed", error = %error, chat_id = msg.chat.id.0);
            let _ = send_scoped_message(
                &bot,
                msg.chat.id,
                msg.thread_id,
                format!("Image handling failed: {error}"),
            )
            .await;
        }
        return Ok(());
    }
    let Some(text) = msg.text().map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    if text.starts_with('/') {
        return Ok(());
    }
    if let Err(error) = run_text_message(&bot, &msg, text, &state).await {
        error!(event = "telegram.message.failed", error = %error, chat_id = msg.chat.id.0);
        let _ = send_scoped_message(
            &bot,
            msg.chat.id,
            msg.thread_id,
            format!("Request failed: {error}"),
        )
        .await;
    }
    Ok(())
}

async fn handle_callback_query(
    bot: Bot,
    query: CallbackQuery,
    state: AppState,
) -> ResponseResult<()> {
    if !state
        .config
        .authorized_user_ids
        .contains(&(query.from.id.0 as i64))
    {
        return Ok(());
    }
    if let Err(error) = run_callback_query(&bot, &query, &state).await {
        error!(event = "telegram.callback.failed", error = %error);
        let _ = bot
            .answer_callback_query(query.id.clone())
            .text("Action failed.")
            .show_alert(true)
            .await;
    }
    Ok(())
}

fn restored_thread_title(title: Option<&str>, fallback_thread_id: Option<i32>) -> String {
    let base = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Thread {}", fallback_thread_id.unwrap_or_default()));
    format!("{base} · 已恢復")
}

async fn render_restore_page(
    state: &AppState,
    chat_id: i64,
    offset: usize,
) -> Result<(String, InlineKeyboardMarkup)> {
    let archived = state.repository.list_archived_threads(chat_id).await?;
    if archived.is_empty() {
        return Ok((
            "No archived threads are available.".to_owned(),
            InlineKeyboardMarkup::default(),
        ));
    }

    let slice = archived.iter().skip(offset).take(RESTORE_PAGE_SIZE);
    let mut lines = Vec::new();
    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    for record in slice {
        let label = record.metadata.title.clone().unwrap_or_else(|| {
            format!(
                "Thread {}",
                record.metadata.message_thread_id.unwrap_or_default()
            )
        });
        lines.push(format!("- {} [{}]", label, record.metadata.workspace_id));
        keyboard.push(vec![InlineKeyboardButton::callback(
            format!("Restore: {}", label),
            format!(
                "{CALLBACK_RESTORE_PICK}:{}:{offset}",
                record.metadata.workspace_id
            ),
        )]);
    }
    let mut pagination = Vec::new();
    if offset > 0 {
        pagination.push(InlineKeyboardButton::callback(
            "Previous",
            format!(
                "{CALLBACK_RESTORE_PAGE}:{}",
                offset.saturating_sub(RESTORE_PAGE_SIZE)
            ),
        ));
    }
    if offset + RESTORE_PAGE_SIZE < archived.len() {
        pagination.push(InlineKeyboardButton::callback(
            "Next",
            format!("{CALLBACK_RESTORE_PAGE}:{}", offset + RESTORE_PAGE_SIZE),
        ));
    }
    if !pagination.is_empty() {
        keyboard.push(pagination);
    }
    Ok((
        format!(
            "Archived threads:\n{}\n\nChoose one to restore.",
            lines.join("\n")
        ),
        InlineKeyboardMarkup::new(keyboard),
    ))
}

async fn run_callback_query(bot: &Bot, query: &CallbackQuery, state: &AppState) -> Result<()> {
    let Some(data) = query.data.as_deref() else {
        return Ok(());
    };
    let Some(message) = query.regular_message() else {
        return Ok(());
    };
    let parts: Vec<&str> = data.split(':').collect();
    let action = parts.first().copied().unwrap_or_default();
    match action {
        CALLBACK_IMAGE_BATCH_ANALYZE => {
            let batch_id = parts.get(1).copied().unwrap_or_default();
            let Some(thread_id) = message.thread_id else {
                bot.answer_callback_query(query.id.clone())
                    .text("This button only works inside a thread.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            let record = state
                .repository
                .get_thread(message.chat.id.0, thread_id_to_i32(thread_id))
                .await?;
            analyze_pending_image_batch(
                bot,
                state,
                record,
                thread_id,
                batch_id,
                None,
                Some(&query.id),
            )
            .await?;
        }
        CALLBACK_RESTORE_PAGE => {
            let offset = parts
                .get(1)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let (text, markup) = render_restore_page(state, message.chat.id.0, offset).await?;
            bot.edit_message_text(message.chat.id, message.id, text)
                .reply_markup(markup)
                .await?;
            bot.answer_callback_query(query.id.clone()).await?;
        }
        CALLBACK_RESTORE_PICK => {
            let workspace_id = parts.get(1).copied().unwrap_or_default();
            let offset = parts
                .get(2)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            restore_thread(bot, message, query, state, workspace_id, offset).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn restore_thread(
    bot: &Bot,
    message: &Message,
    query: &CallbackQuery,
    state: &AppState,
    workspace_id: &str,
    offset: usize,
) -> Result<()> {
    let Some(thread_record) = state
        .repository
        .get_workspace_by_id(message.chat.id.0, workspace_id)
        .await?
    else {
        bot.answer_callback_query(query.id.clone())
            .text("That archived workspace no longer exists.")
            .await?;
        return Ok(());
    };

    if !matches!(thread_record.metadata.status, ConversationStatus::Archived) {
        bot.answer_callback_query(query.id.clone())
            .text("That workspace is already active.")
            .await?;
        return Ok(());
    }

    let session = state
        .repository
        .read_session_binding(&thread_record)
        .await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        bot.answer_callback_query(query.id.clone())
            .text(session_binding_hint(session.as_ref()))
            .show_alert(true)
            .await?;
        return Ok(());
    };
    let workspace_path = ensure_bound_workspace_runtime(
        state,
        &thread_record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;

    let typing = TypingHeartbeat::start(bot.clone(), message.chat.id, None);
    let recap = state
        .codex
        .generate_restore_recap_from_session(
            &CodexWorkspace {
                working_directory: workspace_path.clone(),
            },
            existing_thread_id,
        )
        .await;
    typing.stop().await;
    let recap = match recap {
        Ok(recap) => recap,
        Err(error) => {
            let record = state
                .repository
                .mark_session_binding_broken(thread_record, error.to_string())
                .await?;
            let _ = record;
            bot.answer_callback_query(query.id.clone())
                .text("Restore failed because the bound Codex session could not be resumed. Use /bind_session <session_id>.")
                .show_alert(true)
                .await?;
            return Ok(());
        }
    };
    let restored_record = state
        .repository
        .mark_session_binding_verified(thread_record)
        .await?;
    let topic = bot
        .create_forum_topic(
            message.chat.id,
            restored_thread_title(
                restored_record.metadata.title.as_deref(),
                restored_record.metadata.message_thread_id,
            ),
        )
        .await?;
    let restored_record = state
        .repository
        .restore_thread(
            restored_record,
            thread_id_to_i32(topic.thread_id),
            topic.name.clone(),
        )
        .await?;
    let _ = ensure_bound_workspace_runtime(
        state,
        &restored_record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;
    state
        .repository
        .append_log(
            &restored_record,
            LogDirection::System,
            format!(
                "Thread restored from archive into Telegram thread \"{}\" (message_thread_id {}).",
                topic.name,
                thread_id_to_i32(topic.thread_id)
            ),
            Some(query.from.id.0 as i64),
        )
        .await?;
    bot.answer_callback_query(query.id.clone())
        .text("Thread restored.")
        .await?;
    send_scoped_message(
        bot,
        message.chat.id,
        None,
        format!("Restored into \"{}\". Continue there.", topic.name),
    )
    .await?;
    send_scoped_message(
        bot,
        message.chat.id,
        Some(topic.thread_id),
        format!(
            "This thread has been restored from archive.\n\nHere is a recap of our work so far:\n{}",
            if recap.final_response.trim().is_empty() {
                "This thread was restored from archive, but Codex did not return a recap."
            } else {
                recap.final_response.trim()
            }
        ),
    )
    .await?;
    let (text, markup) = render_restore_page(state, message.chat.id.0, offset).await?;
    bot.edit_message_text(message.chat.id, message.id, text)
        .reply_markup(markup)
        .await?;
    Ok(())
}

async fn run_text_message(bot: &Bot, msg: &Message, text: &str, state: &AppState) -> Result<()> {
    if is_control_chat(msg) {
        send_scoped_message(
            bot,
            msg.chat.id,
            None,
            "Main private chat is the control console. Use /new_thread first.",
        )
        .await?;
        return Ok(());
    }

    let thread_id = msg.thread_id.context("thread message missing thread id")?;
    let mut record = state
        .repository
        .get_thread(msg.chat.id.0, thread_id_to_i32(thread_id))
        .await?;
    if matches!(record.metadata.status, ConversationStatus::Archived) {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            "This thread is archived.",
        )
        .await?;
        return Ok(());
    }
    let session = state.repository.read_session_binding(&record).await?;
    let Some(existing_thread_id) = usable_bound_session_id(session.as_ref()) else {
        send_scoped_message(
            bot,
            msg.chat.id,
            Some(thread_id),
            session_binding_hint(session.as_ref()),
        )
        .await?;
        return Ok(());
    };
    let workspace_path = ensure_bound_workspace_runtime(
        state,
        &record,
        session.as_ref().context("missing session binding")?,
    )
    .await?;

    if let Some(batch) = state.repository.read_pending_image_batch(&record).await? {
        if !batch.images.is_empty() {
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::User,
                    text.to_owned(),
                    msg.from.as_ref().map(|user| user.id.0 as i64),
                )
                .await?;
            analyze_pending_image_batch(
                bot,
                state,
                record,
                thread_id,
                &batch.batch_id,
                Some(text),
                None,
            )
            .await?;
            return Ok(());
        }
    }

    state
        .repository
        .append_log(
            &record,
            LogDirection::User,
            text.to_owned(),
            msg.from.as_ref().map(|user| user.id.0 as i64),
        )
        .await?;

    let typing = TypingHeartbeat::start(bot.clone(), msg.chat.id, Some(thread_id));
    let preview = std::sync::Arc::new(Mutex::new(TurnPreviewController::new(
        bot.clone(),
        msg.chat.id,
        Some(thread_id),
        state.config.stream_message_max_chars,
        state.config.command_output_tail_chars,
        state.config.stream_edit_interval_ms,
    )));
    let preview_heartbeat = PreviewHeartbeat::start(preview.clone());

    let result = state
        .codex
        .run_locked_prompt_with_events(
            &CodexWorkspace {
                working_directory: workspace_path,
            },
            existing_thread_id,
            text,
            |event| {
                let preview = preview.clone();
                async move {
                    preview.lock().await.consume(&event).await;
                }
            },
        )
        .await;
    preview_heartbeat.stop().await;
    typing.stop().await;

    match result {
        Ok(result) => {
            record = state
                .repository
                .mark_session_binding_verified(record)
                .await?;
            state
                .repository
                .append_log(
                    &record,
                    LogDirection::Assistant,
                    result.final_response.clone(),
                    None,
                )
                .await?;
            if !preview.lock().await.complete(&result.final_response).await {
                let final_text = if result.final_response.trim().is_empty() {
                    preview
                        .lock()
                        .await
                        .fallback_final_response()
                        .trim()
                        .to_owned()
                } else {
                    result.final_response
                };
                if !final_text.trim().is_empty() {
                    send_scoped_message(bot, msg.chat.id, Some(thread_id), final_text).await?;
                }
            }
            dispatch_workspace_telegram_outbox(bot, state, &record, thread_id).await?;
        }
        Err(error) => {
            record = state
                .repository
                .mark_session_binding_broken(record, error.to_string())
                .await?;
            let _ = record;
            send_scoped_message(
                bot,
                msg.chat.id,
                Some(thread_id),
                "Codex session is unavailable. Use /reconnect_codex to retry or /bind_session <session_id> to attach another one.",
            )
            .await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PreviewRenderer, SendMessageDraftRequest};
    use serde_json::json;
    use teloxide::types::ChatId;
    use threadbridge_rust::codex::CodexThreadEvent;

    #[test]
    fn preview_renderer_applies_heartbeat_to_draft_text() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume(&CodexThreadEvent::TurnStarted);
        assert_eq!(renderer.get_render_text(), "● Reading context...");

        renderer.consume(&CodexThreadEvent::ItemUpdated {
            item: json!({
                "type": "agent_message",
                "text": "First draft paragraph"
            }),
        });
        assert_eq!(renderer.get_render_text(), "● First draft paragraph");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "○ First draft paragraph");
    }

    #[test]
    fn preview_renderer_heartbeat_rotates_prefix_marker() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume(&CodexThreadEvent::TurnStarted);
        assert_eq!(renderer.get_render_text(), "● Reading context...");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "○ Reading context...");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "● Reading context...");
    }

    #[test]
    fn send_message_draft_payload_serializes_thread_id_only_when_present() {
        let with_thread = SendMessageDraftRequest {
            chat_id: ChatId(42).0,
            message_thread_id: Some(7),
            draft_id: 11,
            text: "draft".to_owned(),
        };
        let without_thread = SendMessageDraftRequest {
            chat_id: ChatId(42).0,
            message_thread_id: None,
            draft_id: 11,
            text: "draft".to_owned(),
        };

        let with_thread = serde_json::to_value(with_thread).unwrap();
        let without_thread = serde_json::to_value(without_thread).unwrap();

        assert_eq!(with_thread["message_thread_id"], 7);
        assert!(without_thread.get("message_thread_id").is_none());
    }
}
