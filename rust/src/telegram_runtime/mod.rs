use std::path::PathBuf;

use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{BotCommand, CallbackQuery, LinkPreviewOptions, ThreadId};
use teloxide::utils::command::BotCommands;
use tokio::net::TcpStream;
use tracing::{error, info, warn};

use crate::app_server_runtime::WorkspaceRuntimeManager;
pub(crate) use crate::codex::{CodexInputItem, CodexRunner, CodexThreadEvent, CodexWorkspace};
use crate::collaboration_mode::CollaborationMode;
pub(crate) use crate::config::AppConfig;
pub(crate) use crate::image_artifacts::{
    ImageAnalysisArtifact, ImageAnalysisImage, build_image_analysis_prompt,
    render_pending_image_batch,
};
use crate::interactive::{
    CompletedInteractiveRequest, InteractiveAdvance, InteractivePromptSnapshot,
    InteractiveRequestRegistry, ToolRequestUserInputQuestion,
};
pub(crate) use crate::repository::{
    AppendPendingImageInput, LogDirection, SessionBinding, ThreadRecord, ThreadRepository,
    TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorRole,
};
use crate::thread_state::{
    BindingStatus, ResolvedThreadState, cached_effective_busy_snapshot_for_binding,
    resolve_thread_state_with_cache,
};
pub(crate) use crate::tool_results::{TelegramOutboxItem, parse_telegram_outbox};
use crate::tui_proxy::TuiProxyManager;
pub(crate) use crate::workspace::{ensure_workspace_runtime, validate_seed_template};
pub(crate) use crate::workspace_status::{
    SessionCurrentStatus, WorkspaceStatusCache, read_local_session_claim, read_session_status,
    record_bot_status_event,
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
    #[command(description = "Show commands for the control chat and managed workspaces")]
    Start,
    #[command(description = "Add a workspace and create or reuse its Telegram thread")]
    AddWorkspace,
    #[command(description = "Start a fresh Codex session for this workspace")]
    NewSession,
    #[command(description = "Rename the current workspace from chat history")]
    RenameWorkspace,
    #[command(description = "Archive the current workspace")]
    ArchiveWorkspace,
    #[command(description = "Show archived workspaces and restore one interactively")]
    RestoreWorkspace,
    #[command(description = "Repair the current workspace's Codex session continuity")]
    RepairSession,
    #[command(description = "Show this workspace's key, path, session, and local session state")]
    WorkspaceInfo,
    #[command(description = "Set this workspace thread to Plan collaboration mode")]
    PlanMode,
    #[command(description = "Set this workspace thread to Default collaboration mode")]
    DefaultMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TelegramTextRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TelegramSystemIntent {
    Info,
    Question,
    Warning,
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) config: AppConfig,
    pub(crate) repository: ThreadRepository,
    pub(crate) codex: CodexRunner,
    pub(crate) app_server_runtime: WorkspaceRuntimeManager,
    pub(crate) tui_proxy: TuiProxyManager,
    pub(crate) interactive_requests: InteractiveRequestRegistry,
    pub(crate) seed_template_path: PathBuf,
    pub(crate) workspace_status_cache: WorkspaceStatusCache,
    pub(crate) runtime_ownership_mode: RuntimeOwnershipMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeOwnershipMode {
    SelfManaged,
    DesktopOwner,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let repository = ThreadRepository::open(&config.runtime.data_root_path).await?;
        let app_server_runtime = WorkspaceRuntimeManager::new();
        let tui_proxy = TuiProxyManager::new(repository.clone());
        Self::new_with_runtimes(config, app_server_runtime, tui_proxy).await
    }

    pub async fn new_with_runtimes(
        config: AppConfig,
        app_server_runtime: WorkspaceRuntimeManager,
        tui_proxy: TuiProxyManager,
    ) -> Result<Self> {
        Self::new_with_runtimes_and_mode(
            config,
            app_server_runtime,
            tui_proxy,
            RuntimeOwnershipMode::SelfManaged,
        )
        .await
    }

    pub(crate) async fn new_with_runtimes_and_mode(
        config: AppConfig,
        app_server_runtime: WorkspaceRuntimeManager,
        tui_proxy: TuiProxyManager,
        runtime_ownership_mode: RuntimeOwnershipMode,
    ) -> Result<Self> {
        let repository = ThreadRepository::open(&config.runtime.data_root_path).await?;
        let seed_template_path = validate_seed_template(
            &config
                .runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        let interactive_requests = InteractiveRequestRegistry::new();
        tui_proxy
            .configure_telegram_bridge(
                config.telegram.telegram_token.clone(),
                interactive_requests.clone(),
            )
            .await;
        Ok(Self {
            codex: CodexRunner::new(config.runtime.codex_model.clone()),
            app_server_runtime,
            tui_proxy,
            interactive_requests,
            repository,
            seed_template_path,
            workspace_status_cache: WorkspaceStatusCache::new(),
            runtime_ownership_mode,
            config,
        })
    }

    pub(crate) fn runtime_is_owner_managed(&self) -> bool {
        self.runtime_ownership_mode == RuntimeOwnershipMode::DesktopOwner
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
        let _ = send_scoped_warning_message(
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
            let _ = send_scoped_warning_message(
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
        let _ = send_scoped_warning_message(
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
    Ok(repository
        .find_thread(msg.chat.id.0, thread_id)
        .await?
        .is_some())
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
        .telegram
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
        CALLBACK_PLAN_IMPLEMENT => {
            let choice = parts.get(1).copied().unwrap_or_default();
            match choice {
                "yes" => {
                    thread_flow::launch_plan_implementation_turn(bot, state, message).await?;
                    bot.edit_message_text(
                        message.chat.id,
                        message.id,
                        format_system_text(
                            TelegramSystemIntent::Info,
                            "Implementing the plan in Default mode.",
                        ),
                    )
                    .await?;
                }
                "no" => {
                    bot.edit_message_text(
                        message.chat.id,
                        message.id,
                        format_system_text(TelegramSystemIntent::Info, "Staying in Plan mode."),
                    )
                    .await?;
                }
                _ => {}
            }
            bot.answer_callback_query(query.id.clone()).await?;
        }
        CALLBACK_REQUEST_USER_INPUT_OPTION => {
            let Some(thread_id) = message.thread_id else {
                bot.answer_callback_query(query.id.clone())
                    .text("This button only works inside a thread.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            let request_id = parts
                .get(1)
                .and_then(|value| value.parse::<i64>().ok())
                .context("invalid request_user_input request id")?;
            let choice = parts.get(2).copied().unwrap_or_default();
            let Some(snapshot) = state
                .interactive_requests
                .prompt_for(message.chat.id.0, thread_id_to_i32(thread_id))
                .await
            else {
                bot.answer_callback_query(query.id.clone())
                    .text("Question is no longer pending.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            let other_index = snapshot
                .question
                .options
                .as_ref()
                .map(|options| options.len())
                .unwrap_or_default();
            let option_index = if choice == "other" {
                other_index
            } else {
                choice
                    .parse::<usize>()
                    .context("invalid request_user_input option index")?
            };
            let Some(advance) = state
                .interactive_requests
                .choose_option(
                    message.chat.id.0,
                    thread_id_to_i32(thread_id),
                    request_id,
                    option_index,
                )
                .await?
            else {
                bot.answer_callback_query(query.id.clone())
                    .text("Question is no longer pending.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            };
            apply_interactive_advance(bot, state, message.chat.id, thread_id, advance).await?;
            bot.answer_callback_query(query.id.clone()).await?;
        }
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
                format_system_text(
                    TelegramSystemIntent::Info,
                    "已採納 TUI session，後續 Telegram 對話將接續該 session。",
                ),
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
                format_system_text(
                    TelegramSystemIntent::Info,
                    "已保留原對話，TUI session 不會被採納為目前 Telegram session。",
                ),
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
                .telegram
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

pub(crate) fn telegram_role_marker(role: TelegramTextRole) -> &'static str {
    match role {
        TelegramTextRole::User => "□",
        TelegramTextRole::Assistant => "■",
    }
}

pub(crate) fn format_prefixed_text(marker: &str, text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        marker.to_owned()
    } else {
        format!("{marker} {trimmed}")
    }
}

pub(crate) fn format_role_text(role: TelegramTextRole, text: &str) -> String {
    format_prefixed_text(telegram_role_marker(role), text)
}

pub(crate) fn telegram_system_marker(intent: TelegramSystemIntent) -> &'static str {
    match intent {
        TelegramSystemIntent::Info => "Info:",
        TelegramSystemIntent::Question => "Question:",
        TelegramSystemIntent::Warning => "Warning:",
    }
}

pub(crate) fn format_system_text(intent: TelegramSystemIntent, text: &str) -> String {
    format_prefixed_text(telegram_system_marker(intent), text)
}

pub(crate) async fn send_scoped_role_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    role: TelegramTextRole,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    let text = format_role_text(role, &text.into());
    let request = bot
        .send_message(chat_id, text)
        .link_preview_options(disabled_link_preview_options());
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await,
        None => request.await,
    }
}

pub(crate) async fn send_scoped_system_message_with_intent(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    intent: TelegramSystemIntent,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    let text = format_system_text(intent, &text.into());
    let request = bot
        .send_message(chat_id, text)
        .link_preview_options(disabled_link_preview_options());
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await,
        None => request.await,
    }
}

pub(crate) async fn send_scoped_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    send_scoped_system_message_with_intent(
        bot,
        chat_id,
        thread_id,
        TelegramSystemIntent::Info,
        text,
    )
    .await
}

pub(crate) async fn send_scoped_warning_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    send_scoped_system_message_with_intent(
        bot,
        chat_id,
        thread_id,
        TelegramSystemIntent::Warning,
        text,
    )
    .await
}

pub(crate) const CALLBACK_REQUEST_USER_INPUT_OPTION: &str = "request_user_input_option";
pub(crate) const CALLBACK_PLAN_IMPLEMENT: &str = "plan_implement";
pub(crate) const PLAN_IMPLEMENTATION_TEXT: &str = "Implement this plan?";
pub(crate) const PLAN_IMPLEMENTATION_MESSAGE: &str = "Implement the plan.";

pub(crate) fn collaboration_mode_for_session(
    session: Option<&SessionBinding>,
) -> CollaborationMode {
    session
        .and_then(|binding| binding.current_collaboration_mode)
        .unwrap_or(CollaborationMode::Default)
}

pub(crate) fn render_request_user_input_prompt(snapshot: &InteractivePromptSnapshot) -> String {
    let question = &snapshot.question;
    let mut lines = vec![format_system_text(
        TelegramSystemIntent::Question,
        &format!("{}: {}", question.header, question.question),
    )];
    if question.is_secret {
        lines.push("Secret input is not supported in Telegram v1.".to_owned());
    } else if snapshot.awaiting_freeform_text || question.options.is_none() || question.is_other {
        lines.push("Reply with your next text message in this thread.".to_owned());
    } else if let Some(options) = question.options.as_ref() {
        for option in options {
            lines.push(format!("- {}: {}", option.label, option.description));
        }
        lines.push("- Other: reply with your own text".to_owned());
    }
    lines.join("\n")
}

pub(crate) async fn upsert_request_user_input_prompt(
    bot: &Bot,
    state: &AppState,
    chat_id: ChatId,
    thread_id: ThreadId,
    snapshot: &InteractivePromptSnapshot,
) -> Result<()> {
    let text = render_request_user_input_prompt(snapshot);
    let markup = request_user_input_markup(snapshot.request_id, &snapshot.question);
    if let Some(message_id) = snapshot.prompt_message_id {
        let request = bot.edit_message_text(chat_id, teloxide::types::MessageId(message_id), text);
        if let Some(markup) = markup {
            request.reply_markup(markup).await?;
        } else {
            request.await?;
        }
        return Ok(());
    }
    let request = bot.send_message(chat_id, text).message_thread_id(thread_id);
    let sent = if let Some(markup) = markup {
        request.reply_markup(markup).await?
    } else {
        request.await?
    };
    state
        .interactive_requests
        .set_prompt_message_id(chat_id.0, thread_id_to_i32(thread_id), sent.id.0)
        .await;
    Ok(())
}

pub(crate) fn request_user_input_markup(
    request_id: i64,
    question: &ToolRequestUserInputQuestion,
) -> Option<teloxide::types::InlineKeyboardMarkup> {
    let options = question.options.as_ref()?;
    let mut buttons = options
        .iter()
        .enumerate()
        .map(|(option_index, option)| {
            vec![teloxide::types::InlineKeyboardButton::callback(
                option.label.clone(),
                format!("{CALLBACK_REQUEST_USER_INPUT_OPTION}:{request_id}:{option_index}"),
            )]
        })
        .collect::<Vec<_>>();
    buttons.push(vec![teloxide::types::InlineKeyboardButton::callback(
        "Other",
        format!("{CALLBACK_REQUEST_USER_INPUT_OPTION}:{request_id}:other"),
    )]);
    Some(teloxide::types::InlineKeyboardMarkup::new(buttons))
}

pub(crate) async fn apply_interactive_advance(
    bot: &Bot,
    state: &AppState,
    chat_id: ChatId,
    thread_id: ThreadId,
    advance: InteractiveAdvance,
) -> Result<()> {
    match advance {
        InteractiveAdvance::Updated(snapshot) => {
            upsert_request_user_input_prompt(bot, state, chat_id, thread_id, &snapshot).await?;
        }
        InteractiveAdvance::Completed(completed) => match completed {
            CompletedInteractiveRequest::Direct {
                prompt_message_id,
                response,
                responder,
            } => {
                let _ = responder.send(response);
                if let Some(message_id) = prompt_message_id {
                    bot.edit_message_text(
                        chat_id,
                        teloxide::types::MessageId(message_id),
                        format_system_text(TelegramSystemIntent::Info, "Questions completed."),
                    )
                    .await?;
                }
            }
            CompletedInteractiveRequest::Tui {
                thread_key,
                request_id,
                prompt_message_id,
                response,
            } => {
                state
                    .tui_proxy
                    .submit_request_user_input_response(&thread_key, request_id, &response)
                    .await?;
                if let Some(message_id) = prompt_message_id {
                    bot.edit_message_text(
                        chat_id,
                        teloxide::types::MessageId(message_id),
                        format_system_text(TelegramSystemIntent::Info, "Questions completed."),
                    )
                    .await?;
                }
            }
        },
    }
    Ok(())
}

pub(crate) async fn send_plan_implementation_prompt(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: ThreadId,
) -> Result<()> {
    bot.send_message(
        chat_id,
        format_system_text(TelegramSystemIntent::Question, PLAN_IMPLEMENTATION_TEXT),
    )
    .message_thread_id(thread_id)
    .reply_markup(teloxide::types::InlineKeyboardMarkup::new(vec![
        vec![teloxide::types::InlineKeyboardButton::callback(
            "Yes, implement this plan",
            format!("{CALLBACK_PLAN_IMPLEMENT}:yes"),
        )],
        vec![teloxide::types::InlineKeyboardButton::callback(
            "No, stay in Plan mode",
            format!("{CALLBACK_PLAN_IMPLEMENT}:no"),
        )],
    ]))
    .await?;
    Ok(())
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

pub(crate) fn current_bound_session_id(session: Option<&SessionBinding>) -> Option<&str> {
    session
        .and_then(|session| session.current_codex_thread_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn usable_bound_session_id(
    state: ResolvedThreadState,
    session: Option<&SessionBinding>,
) -> Option<&str> {
    if state.binding_status != BindingStatus::Healthy {
        return None;
    }
    current_bound_session_id(session)
}

fn healthy_binding_hint(session: Option<&SessionBinding>) -> &'static str {
    if current_bound_session_id(session).is_some() {
        "This workspace already has a usable Codex session."
    } else {
        "This workspace is missing a usable Codex session id. Use /new_session to start a fresh one."
    }
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
        Some(_) => healthy_binding_hint(session),
        None => {
            "This workspace thread is not bound yet. Archive it and re-add the workspace from the control chat with /add_workspace <absolute-path>."
        }
    }
}

pub(crate) fn session_binding_hint_for_state(
    state: ResolvedThreadState,
    session: Option<&SessionBinding>,
) -> &'static str {
    match state.binding_status {
        BindingStatus::Broken => {
            "This workspace's Codex session is invalid. Use /repair_session to verify it again or /new_session to start a fresh one for the same workspace."
        }
        BindingStatus::Unbound => {
            "This workspace thread is not bound yet. Archive it and re-add the workspace from the control chat with /add_workspace <absolute-path>."
        }
        BindingStatus::Healthy if usable_bound_session_id(state, session).is_none() => {
            healthy_binding_hint(session)
        }
        BindingStatus::Healthy => session_binding_hint(session),
    }
}

pub(crate) async fn resolve_busy_gate_state(
    state: &AppState,
    record: &ThreadRecord,
    session: Option<&SessionBinding>,
) -> Result<(ResolvedThreadState, Option<SessionCurrentStatus>)> {
    let resolved_state =
        resolve_thread_state_with_cache(&record.metadata, session, &state.workspace_status_cache)
            .await?;
    let blocking_snapshot = if resolved_state.is_running() {
        cached_effective_busy_snapshot_for_binding(&state.workspace_status_cache, session).await?
    } else {
        None
    };
    Ok((resolved_state, blocking_snapshot))
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
    info!(
        event = "telegram_runtime.workspace.ensure_bound_runtime",
        workspace = %workspace.display(),
        owner_managed = state.runtime_is_owner_managed(),
        "telegram runtime ensured bound workspace surface"
    );
    if state.runtime_is_owner_managed() {
        let _ = read_owner_managed_workspace_runtime(&workspace).await?;
    } else {
        let _ = state
            .app_server_runtime
            .ensure_workspace_daemon(&workspace)
            .await?;
    }
    Ok(workspace)
}

pub(crate) async fn prepare_workspace_runtime_for_control(
    state: &AppState,
    workspace: PathBuf,
) -> Result<CodexWorkspace> {
    info!(
        event = "telegram_runtime.workspace.prepare_control_runtime",
        workspace = %workspace.display(),
        owner_managed = state.runtime_is_owner_managed(),
        "telegram runtime requested control-path workspace runtime"
    );
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

async fn should_route_telegram_input_to_live_tui_session(
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<bool> {
    let Some(tui_session_id) = binding.tui_active_codex_thread_id.as_deref() else {
        return Ok(false);
    };
    if Some(tui_session_id) == current_bound_session_id(Some(binding)) {
        return Ok(false);
    }
    let workspace_path = workspace_path_from_binding(binding)?;
    let Some(owner_claim) = read_local_session_claim(&workspace_path).await? else {
        return Ok(false);
    };
    if owner_claim.thread_key != record.metadata.thread_key
        || owner_claim.session_id.as_deref() != Some(tui_session_id)
    {
        return Ok(false);
    }
    let snapshot = read_session_status(&workspace_path, tui_session_id).await?;
    Ok(snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.owner == crate::workspace_status::SessionStatusOwner::Local && snapshot.live
    }))
}

pub(crate) async fn maybe_route_telegram_input_to_tui_session(
    state: &AppState,
    record: ThreadRecord,
    session: Option<SessionBinding>,
) -> Result<(ThreadRecord, Option<SessionBinding>)> {
    let Some(binding) = session.as_ref() else {
        return Ok((record, session));
    };
    let Some(tui_session_id) = binding.tui_active_codex_thread_id.clone() else {
        return Ok((record, session));
    };

    let log_message = if binding.tui_session_adoption_pending {
        Some(format!(
            "Auto-adopted pending TUI session `{}` on the next Telegram input.",
            tui_session_id
        ))
    } else if should_route_telegram_input_to_live_tui_session(&record, binding).await? {
        Some(format!(
            "Auto-adopted live TUI session `{}` for Telegram input routing.",
            tui_session_id
        ))
    } else {
        None
    };

    let Some(log_message) = log_message else {
        return Ok((record, session));
    };

    let workspace_path = workspace_path_from_binding(binding)?;
    let workspace = shared_codex_workspace(state, workspace_path).await?;
    if let Err(error) = state
        .codex
        .reconnect_session(&workspace, &tui_session_id)
        .await
    {
        let reason = format!(
            "TUI session adoption verification failed for `{}`: {}",
            tui_session_id, error
        );
        let updated = state.repository.clear_tui_adoption_state(record).await?;
        state
            .repository
            .append_log(&updated, LogDirection::System, reason.clone(), None)
            .await?;
        let updated = state
            .repository
            .mark_session_binding_broken(updated, reason)
            .await?;
        let session = state.repository.read_session_binding(&updated).await?;
        return Ok((updated, session));
    }

    let updated = state.repository.adopt_tui_active_session(record).await?;
    let session = state.repository.read_session_binding(&updated).await?;
    state
        .repository
        .append_log(&updated, LogDirection::System, log_message, None)
        .await?;
    Ok((updated, session))
}

pub(crate) async fn shared_codex_workspace(
    state: &AppState,
    workspace: PathBuf,
) -> Result<CodexWorkspace> {
    info!(
        event = "telegram_runtime.workspace.shared_runtime",
        workspace = %workspace.display(),
        owner_managed = state.runtime_is_owner_managed(),
        "telegram runtime requested shared workspace runtime"
    );
    let runtime = if state.runtime_is_owner_managed() {
        read_owner_managed_workspace_runtime(&workspace).await?
    } else {
        state
            .app_server_runtime
            .ensure_workspace_daemon(&workspace)
            .await?
    };
    Ok(CodexWorkspace {
        working_directory: workspace,
        app_server_url: Some(runtime.daemon_ws_url),
    })
}

async fn read_owner_managed_workspace_runtime(
    workspace: &std::path::Path,
) -> Result<crate::app_server_runtime::WorkspaceRuntimeState> {
    let state_path = workspace
        .join(".threadbridge")
        .join("state")
        .join("app-server")
        .join("current.json");
    let contents = tokio::fs::read_to_string(&state_path)
        .await
        .with_context(|| {
            format!(
                "missing owner-managed runtime state: {}",
                state_path.display()
            )
        })?;
    let state: crate::app_server_runtime::WorkspaceRuntimeState = serde_json::from_str(&contents)
        .with_context(|| {
        format!(
            "invalid owner-managed runtime state: {}",
            state_path.display()
        )
    })?;
    let Some(socket_addr) = state.daemon_ws_url.strip_prefix("ws://") else {
        anyhow::bail!("owner-managed daemon url must start with ws://");
    };
    if TcpStream::connect(socket_addr).await.is_err() {
        anyhow::bail!(
            "workspace runtime is not ready for {}. Start threadbridge_desktop and repair the workspace runtime first.",
            workspace.display()
        );
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::{
        AppState, Command, RuntimeOwnershipMode, TelegramSystemIntent, TelegramTextRole,
        command_list, current_bound_session_id, format_role_text, format_system_text,
        maybe_route_telegram_input_to_tui_session, session_binding_hint_for_state,
        should_cleanup_topic_rename_service_message, topic_rename_service_message_thread_id,
        usable_bound_session_id,
    };
    use crate::app_server_runtime::WorkspaceRuntimeManager;
    use crate::app_server_runtime::WorkspaceRuntimeState;
    use crate::codex::CodexRunner;
    use crate::config::{AppConfig, RuntimeConfig, TelegramConfig};
    use crate::interactive::InteractiveRequestRegistry;
    use crate::repository::{SessionBinding, ThreadRepository};
    use crate::thread_state::{BindingStatus, LifecycleStatus, ResolvedThreadState, RunStatus};
    use crate::tui_proxy::TuiProxyManager;
    use crate::workspace_status::{
        SessionStatusOwner, WorkspaceStatusCache, WorkspaceStatusPhase, read_session_status,
        record_hcodex_launcher_started, record_tui_proxy_connected,
    };
    use anyhow::Context;
    use serde_json::json;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::Duration;
    use teloxide::types::Message;
    use teloxide::utils::command::BotCommands;
    use tokio::fs;
    use tokio::net::TcpListener;
    use tokio::time::timeout;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
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

    fn temp_workspace() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-live-tui-{}", Uuid::new_v4()))
    }

    async fn start_mock_app_server(workspace: PathBuf) -> anyhow::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn({
                    let workspace = workspace.clone();
                    async move {
                        let Ok(mut ws) = accept_async(stream).await else {
                            return;
                        };
                        while let Some(message) = futures_util::StreamExt::next(&mut ws).await {
                            let Ok(message) = message else {
                                break;
                            };
                            let WsMessage::Text(text) = message else {
                                continue;
                            };
                            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text)
                            else {
                                continue;
                            };
                            let Some(id) = payload.get("id").and_then(serde_json::Value::as_i64)
                            else {
                                continue;
                            };
                            let method = payload
                                .get("method")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            let response = match method {
                                "initialize" => json!({
                                    "id": id,
                                    "result": { "protocolVersion": "2" },
                                }),
                                "thread/read" => json!({
                                    "id": id,
                                    "result": {
                                        "thread": {
                                            "id": "thr_tui",
                                            "cwd": workspace.display().to_string(),
                                        },
                                        "cwd": workspace.display().to_string(),
                                        "model": "gpt-test",
                                        "reasoningEffort": "medium",
                                        "approvalPolicy": "on-request",
                                        "sandbox": "workspace-write",
                                    },
                                }),
                                _ => json!({
                                    "id": id,
                                    "error": {
                                        "message": format!("unsupported method: {method}"),
                                    },
                                }),
                            };
                            let _ = futures_util::SinkExt::send(
                                &mut ws,
                                WsMessage::Text(response.to_string().into()),
                            )
                            .await;
                        }
                    }
                });
            }
        });
        Ok(format!("ws://127.0.0.1:{}", addr.port()))
    }

    async fn write_owner_managed_runtime_state(
        workspace: &std::path::Path,
        daemon_ws_url: &str,
    ) -> anyhow::Result<()> {
        let state_dir = workspace.join(".threadbridge/state/app-server");
        fs::create_dir_all(&state_dir).await?;
        let state = WorkspaceRuntimeState {
            schema_version: 1,
            workspace_cwd: workspace.display().to_string(),
            daemon_ws_url: daemon_ws_url.to_owned(),
            tui_proxy_base_ws_url: None,
        };
        fs::write(
            state_dir.join("current.json"),
            format!("{}\n", serde_json::to_string_pretty(&state)?),
        )
        .await?;
        timeout(Duration::from_secs(1), async {
            loop {
                if tokio::net::TcpStream::connect(
                    daemon_ws_url.strip_prefix("ws://").unwrap_or_default(),
                )
                .await
                .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("timed out waiting for mock app-server to listen")?;
        Ok(())
    }

    #[test]
    fn command_list_registers_workspace_first_commands() {
        let commands = command_list()
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();

        assert!(commands.iter().any(|command| command == "/new_session"));
        assert!(commands.iter().any(|command| command == "/add_workspace"));
        assert!(commands.iter().any(|command| command == "/workspace_info"));
        assert!(
            commands
                .iter()
                .any(|command| command == "/archive_workspace")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "/restore_workspace")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "/rename_workspace")
        );
        assert!(commands.iter().any(|command| command == "/repair_session"));
        assert!(
            !commands
                .iter()
                .any(|command| command == "/reset_codex_session")
        );
    }

    #[test]
    fn role_formatter_uses_symbol_headers() {
        assert_eq!(format_role_text(TelegramTextRole::User, "hello"), "□ hello");
        assert_eq!(
            format_role_text(TelegramTextRole::Assistant, "hello"),
            "■ hello"
        );
    }

    #[test]
    fn system_formatter_uses_intent_specific_headers() {
        assert_eq!(
            format_system_text(TelegramSystemIntent::Info, "ready"),
            "Info: ready"
        );
        assert_eq!(
            format_system_text(TelegramSystemIntent::Question, "continue?"),
            "Question: continue?"
        );
        assert_eq!(
            format_system_text(TelegramSystemIntent::Warning, "failed"),
            "Warning: failed"
        );
    }

    #[test]
    fn binding_hint_prefers_resolved_broken_state() {
        let state = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Active,
            binding_status: BindingStatus::Broken,
            run_status: RunStatus::Idle,
        };
        let hint = session_binding_hint_for_state(state, None);
        assert!(hint.contains("/repair_session"));
        assert!(hint.contains("/new_session"));
    }

    #[test]
    fn usable_bound_session_id_requires_healthy_binding_status() {
        let binding = SessionBinding::fresh(
            Some("/tmp/workspace".to_owned()),
            Some("thr_current".to_owned()),
            crate::execution_mode::SessionExecutionSnapshot::from_mode(
                crate::execution_mode::ExecutionMode::FullAuto,
            ),
        );
        let healthy = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Active,
            binding_status: BindingStatus::Healthy,
            run_status: RunStatus::Idle,
        };
        let broken = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Active,
            binding_status: BindingStatus::Broken,
            run_status: RunStatus::Idle,
        };

        assert_eq!(
            usable_bound_session_id(healthy, Some(&binding)),
            Some("thr_current")
        );
        assert_eq!(usable_bound_session_id(broken, Some(&binding)), None);
        assert_eq!(
            current_bound_session_id(Some(&binding)),
            Some("thr_current")
        );
    }

    #[tokio::test]
    async fn telegram_input_auto_adopts_live_tui_session() {
        let root = temp_path();
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).await.unwrap();
        let daemon_ws_url = start_mock_app_server(workspace.clone()).await.unwrap();
        write_owner_managed_runtime_state(&workspace, &daemon_ws_url)
            .await
            .unwrap();
        let repository = ThreadRepository::open(&root).await.unwrap();
        let record = repository
            .create_thread(1, 7, "Title".to_owned())
            .await
            .unwrap();
        let record = repository
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                crate::execution_mode::SessionExecutionSnapshot::from_mode(
                    crate::execution_mode::ExecutionMode::FullAuto,
                ),
            )
            .await
            .unwrap();
        let _ = repository
            .set_tui_active_session_for_thread_key(&record.metadata.thread_key, "thr_tui")
            .await
            .unwrap();
        record_hcodex_launcher_started(
            &workspace,
            &record.metadata.thread_key,
            42,
            77,
            "codex --remote",
        )
        .await
        .unwrap();
        record_tui_proxy_connected(&workspace, &record.metadata.thread_key, "thr_tui")
            .await
            .unwrap();

        let state = AppState {
            config: AppConfig {
                telegram: TelegramConfig {
                    telegram_token: "test".to_owned(),
                    authorized_user_ids: HashSet::from([7_i64]),
                },
                stream_edit_interval_ms: 10,
                stream_message_max_chars: 1000,
                command_output_tail_chars: 1000,
                workspace_status_poll_interval_ms: 1000,
                runtime: RuntimeConfig {
                    data_root_path: root.clone(),
                    codex_working_directory: root.clone(),
                    codex_model: None,
                    debug_log_path: root.join("debug.jsonl"),
                    management_bind_addr: "127.0.0.1:38420".parse().unwrap(),
                },
            },
            repository: repository.clone(),
            codex: CodexRunner::new(None),
            app_server_runtime: WorkspaceRuntimeManager::new(),
            tui_proxy: TuiProxyManager::new(repository.clone()),
            interactive_requests: InteractiveRequestRegistry::new(),
            seed_template_path: root.join("seed.md"),
            workspace_status_cache: WorkspaceStatusCache::new(),
            runtime_ownership_mode: RuntimeOwnershipMode::DesktopOwner,
        };

        let session: Option<SessionBinding> =
            repository.read_session_binding(&record).await.unwrap();
        let (updated, updated_session) =
            maybe_route_telegram_input_to_tui_session(&state, record, session)
                .await
                .unwrap();
        let binding = repository
            .read_session_binding(&updated)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_tui"));
        assert_eq!(binding.tui_active_codex_thread_id, None);
        assert!(!binding.tui_session_adoption_pending);
        assert_eq!(
            updated_session.unwrap().current_codex_thread_id.as_deref(),
            Some("thr_tui")
        );
        let live_snapshot = read_session_status(&workspace, "thr_tui")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live_snapshot.owner, SessionStatusOwner::Local);
        assert_eq!(live_snapshot.phase, WorkspaceStatusPhase::ShellActive);
    }

    #[test]
    fn command_parser_uses_workspace_first_names() {
        assert!(matches!(
            Command::parse("/new_session", ""),
            Ok(Command::NewSession)
        ));
        assert!(matches!(
            Command::parse("/add_workspace", ""),
            Ok(Command::AddWorkspace)
        ));
        assert!(matches!(
            Command::parse("/workspace_info", ""),
            Ok(Command::WorkspaceInfo)
        ));
        assert!(matches!(
            Command::parse("/archive_workspace", ""),
            Ok(Command::ArchiveWorkspace)
        ));
        assert!(matches!(
            Command::parse("/restore_workspace", ""),
            Ok(Command::RestoreWorkspace)
        ));
        assert!(matches!(
            Command::parse("/rename_workspace", ""),
            Ok(Command::RenameWorkspace)
        ));
        assert!(matches!(
            Command::parse("/repair_session", ""),
            Ok(Command::RepairSession)
        ));
        assert!(Command::parse("/new", "").is_err());
        assert!(Command::parse("/generate_title", "").is_err());
        assert!(Command::parse("/reconnect_codex", "").is_err());
        assert!(Command::parse("/thread_info", "").is_err());
        assert!(Command::parse("/archive_thread", "").is_err());
        assert!(Command::parse("/restore_thread", "").is_err());
        assert!(Command::parse("/new_thread", "").is_err());
        assert!(Command::parse("/bind_workspace", "").is_err());
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
