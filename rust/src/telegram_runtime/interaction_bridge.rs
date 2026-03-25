use anyhow::Result;
use teloxide::Bot;
use teloxide::payloads::SendMessageSetters;
use teloxide::requests::Requester;
use teloxide::types::{ChatId, MessageId, ThreadId};
use tokio::sync::mpsc;
use tracing::warn;

use crate::collaboration_mode::CollaborationMode;
use crate::interactive::InteractiveRequestRegistry;
use crate::repository::ThreadRepository;
use crate::runtime_interaction::{
    RuntimeInteractionEvent, RuntimeInteractionRequest, RuntimeInteractionResolved,
    RuntimeInteractionSender, TurnCompletionSummary,
};

use super::{
    TelegramSystemIntent, format_system_text, render_request_user_input_prompt,
    request_user_input_markup, send_plan_implementation_prompt,
};

pub(crate) fn spawn_telegram_interaction_bridge(
    bot_token: String,
    repository: ThreadRepository,
    registry: InteractiveRequestRegistry,
) -> RuntimeInteractionSender {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let bot = Bot::new(bot_token);
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if let Err(error) =
                handle_runtime_interaction(&bot, &repository, &registry, event).await
            {
                warn!(event = "telegram.interaction_bridge.failed", error = %error);
            }
        }
    });
    sender
}

async fn handle_runtime_interaction(
    bot: &Bot,
    repository: &ThreadRepository,
    registry: &InteractiveRequestRegistry,
    event: RuntimeInteractionEvent,
) -> Result<()> {
    match event {
        RuntimeInteractionEvent::RequestUserInput(request) => {
            handle_request_user_input(bot, repository, registry, request).await
        }
        RuntimeInteractionEvent::RequestResolved(resolved) => {
            handle_request_resolved(bot, registry, resolved).await
        }
        RuntimeInteractionEvent::TurnCompleted(summary) => {
            handle_turn_completed(bot, repository, summary).await
        }
    }
}

async fn handle_request_user_input(
    bot: &Bot,
    repository: &ThreadRepository,
    registry: &InteractiveRequestRegistry,
    request: RuntimeInteractionRequest,
) -> Result<()> {
    if request
        .params
        .questions
        .iter()
        .any(|question| question.is_secret)
    {
        return Ok(());
    }
    let Some(record) = repository
        .find_active_thread_by_key(&request.thread_key)
        .await?
    else {
        return Ok(());
    };
    let Some(telegram_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    let snapshot = registry
        .register_tui(
            record.metadata.chat_id,
            telegram_thread_id,
            request.thread_key,
            request.request_id,
            request.params,
        )
        .await?;
    let text = render_request_user_input_prompt(&snapshot);
    let chat_id = ChatId(record.metadata.chat_id);
    let thread_id = ThreadId(MessageId(telegram_thread_id));
    let request = bot.send_message(chat_id, text).message_thread_id(thread_id);
    let sent =
        if let Some(markup) = request_user_input_markup(snapshot.request_id, &snapshot.question) {
            request.reply_markup(markup).await?
        } else {
            request.await?
        };
    registry
        .set_prompt_message_id(record.metadata.chat_id, telegram_thread_id, sent.id.0)
        .await;
    Ok(())
}

async fn handle_request_resolved(
    bot: &Bot,
    registry: &InteractiveRequestRegistry,
    resolved: RuntimeInteractionResolved,
) -> Result<()> {
    let Some(resolved_request) = registry
        .resolve_request_id(&resolved.thread_id, &resolved.request_id)
        .await
    else {
        return Ok(());
    };
    if let Some(message_id) = resolved_request.prompt_message_id {
        let _ = bot
            .edit_message_text(
                ChatId(resolved_request.chat_id),
                MessageId(message_id),
                format_system_text(TelegramSystemIntent::Info, "Questions resolved."),
            )
            .await;
    }
    Ok(())
}

async fn handle_turn_completed(
    bot: &Bot,
    repository: &ThreadRepository,
    summary: TurnCompletionSummary,
) -> Result<()> {
    let _ = summary.final_text.as_deref();
    if summary.collaboration_mode != CollaborationMode::Plan || !summary.has_plan {
        return Ok(());
    }
    let Some(record) = repository
        .find_active_thread_by_key(&summary.thread_key)
        .await?
    else {
        return Ok(());
    };
    let Some(telegram_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    send_plan_implementation_prompt(
        bot,
        ChatId(record.metadata.chat_id),
        ThreadId(MessageId(telegram_thread_id)),
    )
    .await?;
    Ok(())
}
