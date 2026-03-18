use anyhow::Result;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::dptree;
use teloxide::prelude::*;
use tracing::info;

use threadbridge_rust::config::load_app_config;
use threadbridge_rust::logging::init_json_logs;
use threadbridge_rust::telegram_runtime::{
    AppState, Command, command_list, handle_callback_query, handle_command, handle_message,
    status_sync::spawn_workspace_status_watcher,
};

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_app_config()?;
    let _guard = init_json_logs(&config.runtime.debug_log_path)?;
    let state = AppState::new(config.clone()).await?;
    let bot = Bot::new(config.telegram_token.clone());
    spawn_workspace_status_watcher(bot.clone(), state.clone()).await;

    bot.set_my_commands(command_list()).await?;
    info!(
        event = "bot.started",
        commands = ?command_list().into_iter().map(|command| command.command).collect::<Vec<_>>(),
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
