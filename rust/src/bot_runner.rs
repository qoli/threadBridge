use anyhow::Result;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::dptree;
use teloxide::prelude::*;
use tracing::{info, warn};

use crate::app_server_runtime::WorkspaceRuntimeManager;
use crate::config::{AppConfig, load_app_config, load_optional_telegram_config};
use crate::local_control::TelegramControlBridgeHandle;
use crate::management_api::{ManagementApiHandle, TelegramPollingState};
use crate::runtime_control::RuntimeOwnershipMode;
use crate::telegram_runtime::{
    AppState, Command, command_list, handle_callback_query, handle_command, handle_message,
    status_sync::{reconcile_stale_bot_busy_sessions, spawn_workspace_status_watcher},
};

#[derive(Clone)]
pub struct BotRuntimeHandle {
    pub bot: Bot,
    pub state: AppState,
}

impl BotRuntimeHandle {
    pub fn runtime_interaction_sender(
        &self,
    ) -> crate::runtime_interaction::RuntimeInteractionSender {
        self.state.runtime_interaction_sender.clone()
    }
}

pub async fn spawn_bot_runtime(
    config: AppConfig,
    management_api: ManagementApiHandle,
) -> Result<BotRuntimeHandle> {
    let state = AppState::new(config.clone()).await?;
    spawn_bot_runtime_from_state(config, management_api, state).await
}

pub async fn spawn_bot_runtime_with_runtimes(
    config: AppConfig,
    management_api: ManagementApiHandle,
    app_server_runtime: WorkspaceRuntimeManager,
) -> Result<BotRuntimeHandle> {
    let state = AppState::new_with_runtimes_and_mode(
        config.clone(),
        app_server_runtime,
        None,
        RuntimeOwnershipMode::DesktopOwner,
    )
    .await?;
    spawn_bot_runtime_from_state(config, management_api, state).await
}

async fn spawn_bot_runtime_from_state(
    config: AppConfig,
    management_api: ManagementApiHandle,
    state: AppState,
) -> Result<BotRuntimeHandle> {
    let bot = Bot::new(config.telegram.telegram_token.clone());
    management_api
        .set_telegram_polling_state(TelegramPollingState::Active)
        .await;
    management_api
        .set_telegram_bridge(Some(TelegramControlBridgeHandle::new(
            bot.clone(),
            state.repository.clone(),
        )))
        .await;

    match reconcile_stale_bot_busy_sessions(&state).await {
        Ok(report) => {
            info!(
                event = "workspace_status.reconcile_stale_bot_busy.completed",
                scanned_threads = report.scanned_threads,
                unique_sessions = report.unique_sessions,
                recovered_sessions = report.recovered_sessions,
                recovered_threads = report.recovered_threads,
                skipped_threads = report.skipped_threads,
                "startup stale busy reconciliation completed"
            );
        }
        Err(error) => {
            warn!(
                event = "workspace_status.reconcile_stale_bot_busy.failed",
                error = %error,
                "startup stale busy reconciliation failed"
            );
        }
    }
    spawn_workspace_status_watcher(bot.clone(), state.clone()).await;
    bot.set_my_commands(command_list()).await?;

    let dispatcher_bot = bot.clone();
    let dispatcher_state = state.clone();
    let dispatcher_management_api = management_api.clone();
    tokio::spawn(async move {
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

        Dispatcher::builder(dispatcher_bot, handler)
            .dependencies(dptree::deps![dispatcher_state])
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;

        dispatcher_management_api
            .set_telegram_polling_state(TelegramPollingState::Disconnected)
            .await;
        dispatcher_management_api.set_telegram_bridge(None).await;
    });

    Ok(BotRuntimeHandle { bot, state })
}

pub async fn spawn_bot_runtime_from_env(
    management_api: ManagementApiHandle,
) -> Result<Option<BotRuntimeHandle>> {
    if load_optional_telegram_config()?.is_none() {
        management_api
            .set_telegram_polling_state(TelegramPollingState::Disconnected)
            .await;
        return Ok(None);
    }
    let config = load_app_config()?;
    spawn_bot_runtime(config, management_api).await.map(Some)
}

pub async fn spawn_bot_runtime_from_env_with_runtimes(
    management_api: ManagementApiHandle,
    app_server_runtime: WorkspaceRuntimeManager,
) -> Result<Option<BotRuntimeHandle>> {
    if load_optional_telegram_config()?.is_none() {
        management_api
            .set_telegram_polling_state(TelegramPollingState::Disconnected)
            .await;
        return Ok(None);
    }
    let config = load_app_config()?;
    spawn_bot_runtime_with_runtimes(config, management_api, app_server_runtime)
        .await
        .map(Some)
}
