use std::path::PathBuf;

use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{BotCommand, CallbackQuery, LinkPreviewOptions, ThreadId};
use teloxide::utils::command::BotCommands;
use tracing::{error, warn};

pub(crate) use crate::codex::{CodexInputItem, CodexRunner, CodexThreadEvent, CodexWorkspace};
pub(crate) use crate::config::AppConfig;
pub(crate) use crate::image_artifacts::{
    ImageAnalysisArtifact, ImageAnalysisImage, build_image_analysis_prompt,
    render_pending_image_batch,
};
use crate::app_server_runtime::WorkspaceRuntimeManager;
pub(crate) use crate::repository::{
    AppendPendingImageInput, LogDirection, SessionBinding, ThreadRecord, ThreadRepository,
    ThreadStatus, TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
    TranscriptMirrorRole,
};
pub(crate) use crate::tool_results::{TelegramOutboxItem, parse_telegram_outbox};
use crate::tui_proxy::TuiProxyManager;
pub(crate) use crate::workspace::{ensure_workspace_runtime, validate_seed_template};
pub(crate) use crate::workspace_status::{
    WorkspaceStatusCache, busy_selected_session_status, read_session_status,
    read_workspace_status_with_cache, record_bot_status_event,
};

pub mod final_reply;
mod media;
pub mod preview;
mod restore;
pub mod status_sync;
mod thread_flow;

#[derive(Clone, BotCommands)]
#[command(rename_rule = "snake_case")]
pub enum Command {
    #[command(description = "Show commands for the control chat and bound threads")]
    Start,
    #[command(description = "Create a new Telegram thread")]
    NewThread,
    #[command(description = "Start a fresh Codex session for this thread")]
    New,
    #[command(description = "Bind this Telegram thread to a workspace path")]
    BindWorkspace,
    #[command(description = "Generate a title for the current thread from chat history")]
    GenerateTitle,
    #[command(description = "Archive the current thread")]
    ArchiveThread,
    #[command(description = "Show archived threads and restore one interactively")]
    RestoreThread,
    #[command(description = "Revalidate the current bound Codex session for this thread")]
    ReconnectCodex,
    #[command(description = "Show this thread's key, workspace, session, and CLI owner state")]
    ThreadInfo,
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) config: AppConfig,
    pub(crate) repository: ThreadRepository,
    pub(crate) codex: CodexRunner,
    pub(crate) app_server_runtime: WorkspaceRuntimeManager,
    pub(crate) tui_proxy: TuiProxyManager,
    pub(crate) seed_template_path: PathBuf,
    pub(crate) workspace_status_cache: WorkspaceStatusCache,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let repository = ThreadRepository::open(&config.runtime.data_root_path).await?;
        let seed_template_path = validate_seed_template(
            &config
                .runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        Ok(Self {
            codex: CodexRunner::new(config.runtime.codex_model.clone()),
            app_server_runtime: WorkspaceRuntimeManager::new(),
            tui_proxy: TuiProxyManager::new(repository.clone()),
            repository,
            seed_template_path,
            workspace_status_cache: WorkspaceStatusCache::new(),
            config,
        })
    }
}

pub fn command_list() -> Vec<BotCommand> {
    Command::bot_commands()
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    command: Command,
    state: AppState,
) -> ResponseResult<()> {
    if !is_authorized(&state, &msg) {
        return Ok(());
    }
    if let Err(error) = thread_flow::run_command(&bot, &msg, command, &state).await {
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

pub async fn handle_message(bot: Bot, msg: Message, state: AppState) -> ResponseResult<()> {
    if handle_topic_rename_service_message(&bot, &msg, &state.repository).await {
        return Ok(());
    }
    if !is_authorized(&state, &msg) {
        return Ok(());
    }
    if let Some(image) = media::extract_incoming_image(&msg) {
        if let Err(error) = media::queue_image_for_thread(&bot, &msg, &state, image).await {
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
    if let Err(error) = thread_flow::run_text_message(&bot, &msg, text, &state).await {
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

async fn handle_topic_rename_service_message(
    bot: &Bot,
    msg: &Message,
    repository: &ThreadRepository,
) -> bool {
    let should_cleanup = match should_cleanup_topic_rename_service_message(repository, msg).await {
        Ok(value) => value,
        Err(error) => {
            warn!(
                event = "telegram.topic_rename_cleanup.lookup_failed",
                error = %error,
                chat_id = msg.chat.id.0,
                message_id = msg.id.0
            );
            return false;
        }
    };
    if !should_cleanup {
        return false;
    }
    if let Err(error) = bot.delete_message(msg.chat.id, msg.id).await {
        warn!(
            event = "telegram.topic_rename_cleanup.delete_failed",
            error = %error,
            chat_id = msg.chat.id.0,
            message_id = msg.id.0,
            message_thread_id = msg.thread_id.map(thread_id_to_i32).unwrap_or_default()
        );
    }
    true
}

async fn should_cleanup_topic_rename_service_message(
    repository: &ThreadRepository,
    msg: &Message,
) -> Result<bool> {
    let Some(thread_id) = topic_rename_service_message_thread_id(msg) else {
        return Ok(false);
    };
    Ok(repository.find_thread(msg.chat.id.0, thread_id).await?.is_some())
}

fn topic_rename_service_message_thread_id(msg: &Message) -> Option<i32> {
    msg.forum_topic_edited()
        .and(msg.thread_id)
        .map(thread_id_to_i32)
}

pub async fn handle_callback_query(
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
        media::CALLBACK_IMAGE_BATCH_ANALYZE => {
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
            media::analyze_pending_image_batch(
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
        restore::CALLBACK_RESTORE_PAGE => {
            let offset = parts
                .get(1)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let (text, markup) =
                restore::render_restore_page(state, message.chat.id.0, offset).await?;
            bot.edit_message_text(message.chat.id, message.id, text)
                .link_preview_options(disabled_link_preview_options())
                .reply_markup(markup)
                .await?;
            bot.answer_callback_query(query.id.clone()).await?;
        }
        restore::CALLBACK_RESTORE_PICK => {
            let thread_key = parts.get(1).copied().unwrap_or_default();
            let offset = parts
                .get(2)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            restore::restore_thread(bot, message, query, state, thread_key, offset).await?;
        }
        status_sync::CALLBACK_TUI_ADOPT_ACCEPT => {
            let thread_key = parts.get(1).copied().unwrap_or_default();
            let Some(record) = state
                .repository
                .get_thread_by_key(message.chat.id.0, thread_key)
                .await?
            else {
                bot.answer_callback_query(query.id.clone())
                    .text("Thread not found.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            let updated = state.repository.adopt_tui_active_session(record).await?;
            bot.edit_message_text(
                message.chat.id,
                message.id,
                "已採納 TUI session，後續 Telegram 對話將接續該 session。",
            )
            .await?;
            let _ =
                status_sync::refresh_thread_topic_title(bot, state, &updated, "tui_adopt_accept")
                    .await;
            bot.answer_callback_query(query.id.clone()).await?;
        }
        status_sync::CALLBACK_TUI_ADOPT_REJECT => {
            let thread_key = parts.get(1).copied().unwrap_or_default();
            let Some(record) = state
                .repository
                .get_thread_by_key(message.chat.id.0, thread_key)
                .await?
            else {
                bot.answer_callback_query(query.id.clone())
                    .text("Thread not found.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            let updated = state.repository.clear_tui_adoption_state(record).await?;
            bot.edit_message_text(
                message.chat.id,
                message.id,
                "已保留原對話，TUI session 不會被採納為目前 Telegram session。",
            )
            .await?;
            let _ =
                status_sync::refresh_thread_topic_title(bot, state, &updated, "tui_adopt_reject")
                    .await;
            bot.answer_callback_query(query.id.clone()).await?;
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn is_authorized(state: &AppState, msg: &Message) -> bool {
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

pub(crate) fn is_control_chat(msg: &Message) -> bool {
    matches!(&msg.chat.kind, teloxide::types::ChatKind::Private(_)) && msg.thread_id.is_none()
}

pub(crate) fn thread_id_to_i32(thread_id: ThreadId) -> i32 {
    thread_id.0.0
}

pub(crate) async fn send_scoped_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    let text = text.into();
    let request = bot
        .send_message(chat_id, text)
        .link_preview_options(disabled_link_preview_options());
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await,
        None => request.await,
    }
}

pub(crate) fn disabled_link_preview_options() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

pub(crate) fn usable_bound_session_id(session: Option<&SessionBinding>) -> Option<&str> {
    session
        .filter(|session| !session.session_broken)
        .and_then(|session| session.current_codex_thread_id.as_deref())
}

pub(crate) fn workspace_path_from_binding(session: &SessionBinding) -> Result<PathBuf> {
    let workspace = session
        .workspace_cwd
        .as_deref()
        .context("session binding is missing workspace_cwd")?;
    Ok(PathBuf::from(workspace))
}

pub(crate) fn session_binding_hint(session: Option<&SessionBinding>) -> &'static str {
    match session {
        Some(session) if session.session_broken => {
            "This thread's Codex session is invalid. Use /reconnect_codex to verify it again or /new to start a fresh one for the same workspace."
        }
        Some(_) => {
            "This thread is missing a usable Codex thread id. Use /new to start a fresh one."
        }
        None => "This thread is not bound to a workspace yet. Use /bind_workspace <absolute-path>.",
    }
}

pub(crate) fn command_argument_text<'a>(msg: &'a Message, command_name: &str) -> Option<&'a str> {
    let text = msg.text()?.trim();
    let prefix = format!("/{command_name}");
    let remainder = text.strip_prefix(&prefix)?.trim();
    if remainder.is_empty() {
        None
    } else {
        Some(remainder)
    }
}

pub(crate) async fn ensure_bound_workspace_runtime(
    state: &AppState,
    binding: &SessionBinding,
) -> Result<PathBuf> {
    let workspace = workspace_path_from_binding(binding)?;
    ensure_workspace_runtime(
        &state.config.runtime.codex_working_directory,
        &state.config.runtime.data_root_path,
        &state.seed_template_path,
        &workspace,
    )
    .await?;
    let _ = state
        .app_server_runtime
        .ensure_workspace_daemon(&workspace)
        .await?;
    Ok(workspace)
}

pub(crate) async fn shared_codex_workspace(
    state: &AppState,
    workspace: PathBuf,
) -> Result<CodexWorkspace> {
    let runtime = state
        .app_server_runtime
        .ensure_workspace_daemon(&workspace)
        .await?;
    let _ = state
        .tui_proxy
        .ensure_workspace_proxy(&workspace, &runtime.daemon_ws_url)
        .await?;
    Ok(CodexWorkspace {
        working_directory: workspace,
        app_server_url: Some(runtime.daemon_ws_url),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Command, command_list, should_cleanup_topic_rename_service_message,
        topic_rename_service_message_thread_id,
    };
    use crate::repository::ThreadRepository;
    use serde_json::json;
    use std::path::PathBuf;
    use teloxide::utils::command::BotCommands;
    use teloxide::types::Message;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-telegram-test-{}", Uuid::new_v4()))
    }

    fn forum_topic_edited_message(thread_id: Option<i32>) -> Message {
        serde_json::from_value(json!({
            "message_id": 42,
            "message_thread_id": thread_id,
            "date": 1_742_342_400,
            "chat": {
                "id": -1001234567890i64,
                "title": "threadBridge",
                "type": "supergroup",
                "is_forum": true
            },
            "forum_topic_edited": {
                "name": "AGENTS.md Ready Check"
            }
        }))
        .unwrap()
    }

    fn text_message(thread_id: Option<i32>) -> Message {
        serde_json::from_value(json!({
            "message_id": 43,
            "message_thread_id": thread_id,
            "date": 1_742_342_401,
            "chat": {
                "id": -1001234567890i64,
                "title": "threadBridge",
                "type": "supergroup",
                "is_forum": true
            },
            "from": {
                "id": 7,
                "is_bot": false,
                "first_name": "Ronnie"
            },
            "text": "hello"
        }))
        .unwrap()
    }

    #[test]
    fn command_list_registers_new_but_not_reset_codex_session() {
        let commands = command_list()
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();

        assert!(commands.iter().any(|command| command == "/new"));
        assert!(commands.iter().any(|command| command == "/new_thread"));
        assert!(commands.iter().any(|command| command == "/thread_info"));
        assert!(
            !commands
                .iter()
                .any(|command| command == "/reset_codex_session")
        );
    }

    #[test]
    fn command_parser_distinguishes_new_from_new_thread() {
        assert!(matches!(Command::parse("/new", ""), Ok(Command::New)));
        assert!(matches!(
            Command::parse("/new_thread", ""),
            Ok(Command::NewThread)
        ));
        assert!(matches!(
            Command::parse("/thread_info", ""),
            Ok(Command::ThreadInfo)
        ));
        assert!(Command::parse("/reset_codex_session", "").is_err());
    }

    #[test]
    fn topic_rename_service_message_extracts_thread_id() {
        let msg = forum_topic_edited_message(Some(321));
        assert_eq!(topic_rename_service_message_thread_id(&msg), Some(321));
    }

    #[test]
    fn topic_rename_service_message_ignores_regular_text() {
        let msg = text_message(Some(321));
        assert_eq!(topic_rename_service_message_thread_id(&msg), None);
    }

    #[test]
    fn topic_rename_service_message_ignores_missing_thread_id() {
        let msg = forum_topic_edited_message(None);
        assert_eq!(topic_rename_service_message_thread_id(&msg), None);
    }

    #[test]
    fn topic_rename_service_message_does_not_require_authorized_sender() {
        let msg = forum_topic_edited_message(Some(321));
        assert!(msg.from.is_none());
        assert_eq!(topic_rename_service_message_thread_id(&msg), Some(321));
    }

    #[tokio::test]
    async fn cleanup_only_applies_to_managed_threads() {
        let root = temp_path();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let managed = forum_topic_edited_message(Some(321));
        let unmanaged = forum_topic_edited_message(Some(999));

        assert!(
            !should_cleanup_topic_rename_service_message(&repo, &managed)
                .await
                .unwrap()
        );

        repo.create_thread(managed.chat.id.0, 321, "Managed".to_owned())
            .await
            .unwrap();

        assert!(
            should_cleanup_topic_rename_service_message(&repo, &managed)
                .await
                .unwrap()
        );
        assert!(
            !should_cleanup_topic_rename_service_message(&repo, &unmanaged)
                .await
                .unwrap()
        );
    }
}
