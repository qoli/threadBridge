use std::collections::HashSet;
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub data_root_path: PathBuf,
    pub debug_log_path: PathBuf,
    pub codex_working_directory: PathBuf,
    pub codex_model: Option<String>,
    pub management_bind_addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub telegram_token: String,
    pub authorized_user_ids: HashSet<i64>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub runtime: RuntimeConfig,
    pub telegram: TelegramConfig,
    pub stream_edit_interval_ms: u64,
    pub stream_message_max_chars: usize,
    pub command_output_tail_chars: usize,
    pub workspace_status_poll_interval_ms: u64,
}

fn load_dotenv() {
    let local = Path::new(".env.local");
    if local.exists() {
        let _ = dotenvy::from_path(local);
    }
    let _ = dotenvy::dotenv();
}

fn parse_positive_u64(name: &str, fallback: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn parse_positive_usize(name: &str, fallback: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn parse_socket_addr(name: &str, fallback: &str) -> Result<SocketAddr> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_owned());
    raw.parse::<SocketAddr>()
        .with_context(|| format!("Invalid {name}: {raw}"))
}

fn parse_authorized_users(raw: &str) -> Result<HashSet<i64>> {
    let ids = raw
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(trimmed)
        })
        .map(|trimmed| {
            trimmed
                .parse::<i64>()
                .with_context(|| format!("Invalid AUTHORIZED_TELEGRAM_USER_IDS entry: {trimmed}"))
        })
        .collect::<Result<HashSet<_>>>()?;

    if ids.is_empty() {
        bail!("AUTHORIZED_TELEGRAM_USER_IDS must contain at least one Telegram user ID.");
    }

    Ok(ids)
}

fn resolve_from_cwd(input: Option<String>, fallback: &str) -> Result<PathBuf> {
    let cwd = env::current_dir().context("failed to read current working directory")?;
    let value = input.unwrap_or_else(|| fallback.to_owned());
    let joined = cwd.join(value);
    Ok(joined.canonicalize().unwrap_or(joined))
}

pub fn load_runtime_config() -> Result<RuntimeConfig> {
    load_dotenv();

    let bot_data_path = resolve_from_cwd(env::var("BOT_DATA_PATH").ok(), "./data/state.json")?;
    let data_root_path = if let Ok(root) = env::var("DATA_ROOT") {
        resolve_from_cwd(Some(root), "./data")?
    } else {
        bot_data_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data"))
    };

    Ok(RuntimeConfig {
        data_root_path,
        debug_log_path: resolve_from_cwd(
            env::var("DEBUG_LOG_PATH").ok(),
            "./data/debug/events.jsonl",
        )?,
        codex_working_directory: resolve_from_cwd(env::var("CODEX_WORKING_DIRECTORY").ok(), ".")?,
        codex_model: env::var("CODEX_MODEL")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        management_bind_addr: parse_socket_addr(
            "THREADBRIDGE_MANAGEMENT_BIND_ADDR",
            "127.0.0.1:38420",
        )?,
    })
}

pub fn load_telegram_config() -> Result<TelegramConfig> {
    load_dotenv();

    let telegram_token = env::var("TELEGRAM_BOT_TOKEN")
        .context("Missing TELEGRAM_BOT_TOKEN.")?
        .trim()
        .to_owned();
    if telegram_token.is_empty() {
        bail!("Missing TELEGRAM_BOT_TOKEN.");
    }

    let authorized_user_ids = parse_authorized_users(
        &env::var("AUTHORIZED_TELEGRAM_USER_IDS")
            .context("Missing AUTHORIZED_TELEGRAM_USER_IDS.")?,
    )?;

    Ok(TelegramConfig {
        telegram_token,
        authorized_user_ids,
    })
}

pub fn load_optional_telegram_config() -> Result<Option<TelegramConfig>> {
    match load_telegram_config() {
        Ok(config) => Ok(Some(config)),
        Err(error) => {
            let message = error.to_string();
            if message.contains("Missing TELEGRAM_BOT_TOKEN")
                || message.contains("Missing AUTHORIZED_TELEGRAM_USER_IDS")
            {
                return Ok(None);
            }
            Err(error)
        }
    }
}

pub fn load_app_config() -> Result<AppConfig> {
    let runtime = load_runtime_config()?;
    let telegram = load_telegram_config()?;

    Ok(AppConfig {
        runtime,
        telegram,
        stream_edit_interval_ms: parse_positive_u64("STREAM_EDIT_INTERVAL_MS", 750),
        stream_message_max_chars: parse_positive_usize("STREAM_MESSAGE_MAX_CHARS", 3500),
        command_output_tail_chars: parse_positive_usize("COMMAND_OUTPUT_TAIL_CHARS", 800),
        workspace_status_poll_interval_ms: parse_positive_u64(
            "WORKSPACE_STATUS_POLL_INTERVAL_MS",
            1500,
        ),
    })
}
