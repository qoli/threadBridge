use anyhow::Result;
use chrono::Utc;
use teloxide::Bot;
use teloxide::payloads::SendMessageSetters;
use teloxide::requests::Requester;
use teloxide::types::{ChatId, MessageId, ThreadId};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::approval::ApprovalRequestRegistry;
use crate::delivery_bus::{
    ClaimStatus, DeliveryAttempt, DeliveryBusCoordinator, DeliveryChannel, DeliveryClaim,
    DeliveryKind, provisional_key_for_request, provisional_key_for_text,
};
use crate::interactive::InteractiveRequestRegistry;
use crate::repository::ThreadRepository;
use crate::runtime_interaction::{
    RuntimeApprovalRequest, RuntimeInteractionEvent, RuntimeInteractionRequest,
    RuntimeInteractionResolved, RuntimeInteractionSender, TurnCompletionSummary,
};

use super::{
    TelegramSystemIntent, approval_markup, format_system_text, render_request_user_input_prompt,
    render_pending_approval_prompt, request_user_input_markup, send_plan_implementation_prompt,
};

pub(crate) fn spawn_telegram_interaction_bridge(
    bot_token: String,
    repository: ThreadRepository,
    delivery_bus: DeliveryBusCoordinator,
    registry: InteractiveRequestRegistry,
    approval_registry: ApprovalRequestRegistry,
) -> RuntimeInteractionSender {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let bot = Bot::new(bot_token);
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if let Err(error) =
                handle_runtime_interaction(
                    &bot,
                    &repository,
                    &delivery_bus,
                    &registry,
                    &approval_registry,
                    event,
                )
                .await
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
    delivery_bus: &DeliveryBusCoordinator,
    registry: &InteractiveRequestRegistry,
    approval_registry: &ApprovalRequestRegistry,
    event: RuntimeInteractionEvent,
) -> Result<()> {
    let kind = event.kind();
    debug!(
        event = "telegram.interaction_bridge.event",
        interaction_kind = kind.as_str()
    );
    match event {
        RuntimeInteractionEvent::ApprovalRequested(request) => {
            handle_approval_requested(bot, repository, delivery_bus, approval_registry, request)
                .await
        }
        RuntimeInteractionEvent::RequestUserInput(request) => {
            handle_request_user_input(bot, repository, delivery_bus, registry, request).await
        }
        RuntimeInteractionEvent::RequestResolved(resolved) => {
            handle_request_resolved(bot, repository, registry, resolved).await
        }
        RuntimeInteractionEvent::TurnCompleted(summary) => {
            handle_turn_completed(bot, repository, delivery_bus, summary).await
        }
    }
}

async fn handle_request_user_input(
    bot: &Bot,
    repository: &ThreadRepository,
    delivery_bus: &DeliveryBusCoordinator,
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
    let thread_key = request.thread_key.clone();
    let snapshot = registry
        .register_tui(
            record.metadata.chat_id,
            telegram_thread_id,
            thread_key.clone(),
            request.request_id,
            request.params,
        )
        .await?;
    let provisional_key =
        provisional_key_for_request(&snapshot.thread_id, snapshot.request_id, &snapshot.item_id);
    let claim = delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: thread_key.clone(),
            session_id: snapshot.thread_id.clone(),
            turn_id: Some(snapshot.turn_id.clone()),
            provisional_key: Some(provisional_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::RequestUserInputPrompt,
            owner: "interaction_bridge".to_owned(),
        })
        .await?;
    if matches!(claim, ClaimStatus::Existing(_)) {
        return Ok(());
    }
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
    let _ = delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key,
            session_id: snapshot.thread_id,
            turn_id: Some(snapshot.turn_id),
            provisional_key: Some(provisional_key),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::RequestUserInputPrompt,
            executor: "telegram_interaction_bridge".to_owned(),
            transport_ref: Some(format!("message:{}", sent.id.0)),
            report_json: serde_json::json!({
                "targets": [{
                    "type": "telegram_message",
                    "target_ref": format!("chat:{}/thread:{}", record.metadata.chat_id, telegram_thread_id),
                    "state": "committed",
                    "transport_ref": format!("message:{}", sent.id.0),
                }]
            }),
        })
        .await;
    Ok(())
}

async fn handle_approval_requested(
    bot: &Bot,
    repository: &ThreadRepository,
    delivery_bus: &DeliveryBusCoordinator,
    approval_registry: &ApprovalRequestRegistry,
    request: RuntimeApprovalRequest,
) -> Result<()> {
    let Some(record) = repository
        .find_active_thread_by_key(&request.approval.thread_key)
        .await?
    else {
        return Ok(());
    };
    let Some(telegram_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    let chat_id = ChatId(record.metadata.chat_id);
    let thread_id = ThreadId(MessageId(telegram_thread_id));
    let provisional_key = provisional_key_for_request(
        &request.approval.thread_id,
        request.approval.request_id,
        &request.approval.item_id,
    );
    let claim = delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: request.approval.thread_key.clone(),
            session_id: request.approval.thread_id.clone(),
            turn_id: Some(request.approval.turn_id.clone()),
            provisional_key: Some(provisional_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::ApprovalPrompt,
            owner: "interaction_bridge".to_owned(),
        })
        .await?;
    if matches!(claim, ClaimStatus::Existing(_)) {
        return Ok(());
    }
    let request_builder = bot
        .send_message(chat_id, render_pending_approval_prompt(&request.approval))
        .message_thread_id(thread_id);
    let sent = if let Some(markup) = approval_markup(&request.approval) {
        request_builder.reply_markup(markup).await?
    } else {
        request_builder.await?
    };
    approval_registry
        .set_prompt_message_id(&request.approval.approval_key, sent.id.0)
        .await;
    let _ = delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key: request.approval.thread_key.clone(),
            session_id: request.approval.thread_id.clone(),
            turn_id: Some(request.approval.turn_id.clone()),
            provisional_key: Some(provisional_key),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::ApprovalPrompt,
            executor: "telegram_interaction_bridge".to_owned(),
            transport_ref: Some(format!("message:{}", sent.id.0)),
            report_json: serde_json::json!({
                "targets": [{
                    "type": "telegram_approval_prompt",
                    "target_ref": format!("chat:{}/thread:{}", record.metadata.chat_id, telegram_thread_id),
                    "state": "committed",
                    "transport_ref": format!("message:{}", sent.id.0),
                    "approval_key": request.approval.approval_key,
                }]
            }),
        })
        .await;
    Ok(())
}

async fn handle_request_resolved(
    bot: &Bot,
    repository: &ThreadRepository,
    registry: &InteractiveRequestRegistry,
    resolved: RuntimeInteractionResolved,
) -> Result<()> {
    if let Some(resolved_request) = registry
        .resolve_request_id(&resolved.thread_id, &resolved.request_id)
        .await
    {
        if let Some(message_id) = resolved_request.prompt_message_id {
            let _ = bot
                .edit_message_text(
                    ChatId(resolved_request.chat_id),
                    MessageId(message_id),
                    format_system_text(TelegramSystemIntent::Info, "Questions resolved."),
                )
                .await;
        }
    }
    if let (Some(thread_key), Some(message_id)) = (
        resolved.approval_thread_key.as_deref(),
        resolved.approval_prompt_message_id,
    ) {
        if let Some(record) = repository.find_active_thread_by_key(thread_key).await? {
            let _ = bot
                .edit_message_text(
                    ChatId(record.metadata.chat_id),
                    MessageId(message_id),
                    format_system_text(TelegramSystemIntent::Info, "Approval resolved."),
                )
                .await;
        }
    }
    Ok(())
}

async fn handle_turn_completed(
    bot: &Bot,
    repository: &ThreadRepository,
    delivery_bus: &DeliveryBusCoordinator,
    summary: TurnCompletionSummary,
) -> Result<()> {
    if !summary.plan_follow_up_requested() {
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
    let session_id = repository
        .read_session_binding(&record)
        .await?
        .and_then(|binding| binding.current_codex_thread_id)
        .unwrap_or_default();
    if session_id.trim().is_empty() {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let provisional_key = provisional_key_for_text(
        &session_id,
        DeliveryKind::SystemNotice,
        summary
            .final_text
            .as_deref()
            .unwrap_or("plan_implementation_prompt"),
        &now,
    );
    let claim = delivery_bus
        .claim_delivery(DeliveryClaim {
            thread_key: summary.thread_key.clone(),
            session_id: session_id.clone(),
            turn_id: None,
            provisional_key: Some(provisional_key.clone()),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::SystemNotice,
            owner: "interaction_bridge".to_owned(),
        })
        .await?;
    if matches!(claim, ClaimStatus::Existing(_)) {
        return Ok(());
    }
    send_plan_implementation_prompt(
        bot,
        ChatId(record.metadata.chat_id),
        ThreadId(MessageId(telegram_thread_id)),
    )
    .await?;
    let _ = delivery_bus
        .commit_delivery(DeliveryAttempt {
            thread_key: summary.thread_key,
            session_id,
            turn_id: None,
            provisional_key: Some(provisional_key),
            channel: DeliveryChannel::Telegram,
            kind: DeliveryKind::SystemNotice,
            executor: "telegram_interaction_bridge".to_owned(),
            transport_ref: None,
            report_json: serde_json::json!({
                "targets": [{
                    "type": "telegram_plan_prompt",
                    "target_ref": format!("chat:{}/thread:{}", record.metadata.chat_id, telegram_thread_id),
                    "state": "committed",
                }]
            }),
        })
        .await;
    Ok(())
}
