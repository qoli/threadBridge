use std::path::PathBuf;

use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{BotCommand, CallbackQuery, ThreadId};
use teloxide::utils::command::BotCommands;
use tracing::error;

pub(crate) use crate::codex::{CodexInputItem, CodexRunner, CodexThreadEvent, CodexWorkspace};
pub(crate) use crate::codex_home::{CodexHome, CodexSessionSummary};
pub(crate) use crate::config::AppConfig;
pub(crate) use crate::image_artifacts::{
    ImageAnalysisArtifact, ImageAnalysisImage, build_image_analysis_prompt,
    render_pending_image_batch,
};
pub(crate) use crate::repository::{
    AppendPendingImageInput, LogDirection, SessionBinding, ThreadRecord, ThreadRepository,
    ThreadStatus,
};
pub(crate) use crate::tool_results::{TelegramOutboxItem, parse_telegram_outbox};
pub(crate) use crate::workspace::{ensure_linked_workspace_runtime, validate_seed_template};

mod media;
pub mod preview;
mod restore;
mod thread_flow;

#[derive(Clone, BotCommands)]
#[command(rename_rule = "snake_case")]
pub enum Command {
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
    #[command(description = "Archive the current thread")]
    ArchiveThread,
    #[command(description = "Show archived threads and restore one interactively")]
    RestoreThread,
    #[command(description = "Revalidate the current bound Codex session for this thread")]
    ReconnectCodex,
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) config: AppConfig,
    pub(crate) repository: ThreadRepository,
    pub(crate) codex: CodexRunner,
    pub(crate) codex_home: CodexHome,
    pub(crate) seed_template_path: PathBuf,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let repository = ThreadRepository::open(&config.runtime.data_root_path).await?;
        let codex_home = CodexHome::discover()?;
        let seed_template_path = validate_seed_template(
            &config
                .runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        Ok(Self {
            codex: CodexRunner::new(config.runtime.codex_model.clone()),
            codex_home,
            repository,
            seed_template_path,
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
    let request = bot.send_message(chat_id, text);
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await,
        None => request.await,
    }
}

pub(crate) fn usable_bound_session_id(session: Option<&SessionBinding>) -> Option<&str> {
    session
        .filter(|session| !session.session_broken)
        .and_then(|session| session.codex_session_id.as_deref())
}

pub(crate) fn session_binding_hint(session: Option<&SessionBinding>) -> &'static str {
    match session {
        Some(session) if session.session_broken => {
            "This thread's bound Codex session is invalid. Use /reconnect_codex to revalidate it or /bind_session <session_id> to attach another one."
        }
        _ => {
            "This thread is not bound to a Codex session. Use /list_sessions, then /bind_session <session_id>."
        }
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

pub(crate) fn format_session_list_text(sessions: &[CodexSessionSummary]) -> String {
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

pub(crate) async fn ensure_bound_workspace_runtime(
    state: &AppState,
    record: &ThreadRecord,
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
        &record.folder_path,
        &record.linked_workspace_path(),
        &resolved.cwd,
    )
    .await?;
    Ok(record.linked_workspace_path())
}
