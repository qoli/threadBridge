use std::collections::HashSet;
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::runtime_paths::{RuntimePathOverrides, resolve_runtime_paths};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub data_root_path: PathBuf,
    pub debug_log_path: PathBuf,
    pub runtime_support_root_path: PathBuf,
    pub runtime_support_seed_root_path: PathBuf,
    pub codex_model: Option<String>,
    pub management_bind_addr: SocketAddr,
}

impl RuntimeConfig {
    pub fn config_env_path(&self) -> PathBuf {
        self.data_root_path.join("config.env.local")
    }

    pub fn managed_codex_binary_path(&self) -> PathBuf {
        self.data_root_path.join(".threadbridge/codex/codex")
    }

    pub fn managed_codex_root_path(&self) -> PathBuf {
        self.data_root_path.join(".threadbridge/codex")
    }

    pub fn runtime_skill_template_path(&self) -> PathBuf {
        self.runtime_support_root_path
            .join("templates")
            .join("threadbridge-runtime-skill")
            .join("SKILL.md")
    }

    pub fn runtime_telemetry_path(&self) -> PathBuf {
        self.debug_log_path
            .parent()
            .unwrap_or(self.data_root_path.as_path())
            .join("runtime-telemetry.jsonl")
    }

    pub fn supports_runtime_support_rebuild(&self) -> bool {
        self.runtime_support_root_path != self.runtime_support_seed_root_path
    }
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
    if let Ok(paths) = resolve_runtime_paths(RuntimePathOverrides::default()) {
        let local = paths.data_root_path.join("config.env.local");
        if local.exists() {
            let _ = dotenvy::from_path(local);
        }
    }
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

fn load_telegram_config_from_path(path: &Path) -> Result<TelegramConfig> {
    if !path.exists() {
        bail!("Missing TELEGRAM_BOT_TOKEN.");
    }
    let mut telegram_token = None;
    let mut authorized_user_ids = None;
    for item in dotenvy::from_path_iter(path)
        .with_context(|| format!("failed to read {}", path.display()))?
    {
        let (key, value) = item?;
        let trimmed = value.trim().to_owned();
        match key.as_str() {
            "TELEGRAM_BOT_TOKEN" if !trimmed.is_empty() => telegram_token = Some(trimmed),
            "AUTHORIZED_TELEGRAM_USER_IDS" if !trimmed.is_empty() => {
                authorized_user_ids = Some(parse_authorized_users(&trimmed)?);
            }
            _ => {}
        }
    }

    let telegram_token = telegram_token.context("Missing TELEGRAM_BOT_TOKEN.")?;
    let authorized_user_ids =
        authorized_user_ids.context("Missing AUTHORIZED_TELEGRAM_USER_IDS.")?;

    Ok(TelegramConfig {
        telegram_token,
        authorized_user_ids,
    })
}

pub fn load_runtime_config() -> Result<RuntimeConfig> {
    load_dotenv();

    let runtime_paths = resolve_runtime_paths(RuntimePathOverrides {
        data_root: env::var("DATA_ROOT").ok(),
        bot_data_path: env::var("BOT_DATA_PATH").ok(),
        debug_log_path: env::var("DEBUG_LOG_PATH").ok(),
    })?;

    Ok(RuntimeConfig {
        data_root_path: runtime_paths.data_root_path,
        debug_log_path: runtime_paths.debug_log_path,
        runtime_support_root_path: runtime_paths.runtime_support_root_path,
        runtime_support_seed_root_path: runtime_paths.runtime_support_seed_root_path,
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
    let runtime = load_runtime_config()?;
    load_optional_telegram_config_from_path(&runtime.config_env_path())
}

pub fn load_optional_telegram_config_from_path(path: &Path) -> Result<Option<TelegramConfig>> {
    match load_telegram_config_from_path(path) {
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
