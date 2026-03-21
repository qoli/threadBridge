use anyhow::Result;
use tracing::info;

use threadbridge_rust::bot_runner::spawn_bot_runtime;
use threadbridge_rust::config::{
    load_app_config, load_optional_telegram_config, load_runtime_config,
};
use threadbridge_rust::hcodex_runtime;
use threadbridge_rust::hcodex_ws_bridge;
use threadbridge_rust::logging::init_json_logs;
use threadbridge_rust::management_api::{TelegramPollingState, spawn_management_api};

#[tokio::main]
async fn main() -> Result<()> {
    if hcodex_runtime::maybe_run_from_args(std::env::args_os().skip(1).collect()).await? {
        return Ok(());
    }
    if hcodex_ws_bridge::maybe_run_from_args(std::env::args_os().skip(1).collect()).await? {
        return Ok(());
    }
    let runtime_config = load_runtime_config()?;
    let _guard = init_json_logs(&runtime_config.debug_log_path)?;
    let management_api = spawn_management_api(runtime_config.clone()).await?;
    info!(
        event = "management_api.ready",
        base_url = %management_api.base_url,
        "local management API is ready"
    );

    if load_optional_telegram_config()?.is_none() {
        management_api
            .set_telegram_polling_state(TelegramPollingState::Disconnected)
            .await;
        info!(
            event = "bot.startup.deferred",
            "Telegram credentials are missing; running local management API only."
        );
        tokio::signal::ctrl_c().await?;
        return Ok(());
    }

    let config = load_app_config()?;
    let _bot_runtime = spawn_bot_runtime(config.clone(), management_api.clone()).await?;
    info!(
        event = "bot.started",
        data_root_path = %config.runtime.data_root_path.display(),
        debug_log_path = %config.runtime.debug_log_path.display(),
        working_directory = %config.runtime.codex_working_directory.display(),
        management_api_base_url = %management_api.base_url,
        "Telegram bot is running."
    );
    tokio::signal::ctrl_c().await?;
    management_api
        .set_telegram_polling_state(TelegramPollingState::Disconnected)
        .await;
    Ok(())
}
