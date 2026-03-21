use anyhow::Result;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::dptree;
use teloxide::prelude::*;
use tracing::{info, warn};

use crate::config::{AppConfig, load_app_config, load_optional_telegram_config};
use crate::local_control::LocalControlHandle;
use crate::management_api::{ManagementApiHandle, TelegramPollingState};
use crate::telegram_runtime::{
    AppState, Command, command_list, handle_callback_query, handle_command, handle_message,
    status_sync::{reconcile_stale_bot_busy_sessions, spawn_workspace_status_watcher},
};

#[derive(Clone)]
pub struct BotRuntimeHandle {
    pub bot: Bot,
    pub state: AppState,
}

pub async fn spawn_bot_runtime(
    config: AppConfig,
    management_api: ManagementApiHandle,
) -> Result<BotRuntimeHandle> {
    let state = AppState::new(config.clone()).await?;
    let bot = Bot::new(config.telegram.telegram_token.clone());
    management_api
        .set_telegram_polling_state(TelegramPollingState::Active)
        .await;
    management_api
        .set_local_control(Some(LocalControlHandle::new(bot.clone(), state.clone())))
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
        dispatcher_management_api.set_local_control(None).await;
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
