use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramOutbox {
    pub items: Vec<TelegramOutboxItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelegramOutboxItem {
    Text {
        text: String,
    },
    Photo {
        path: String,
        caption: Option<String>,
    },
    Document {
        path: String,
        caption: Option<String>,
    },
}

pub fn parse_telegram_outbox(text: &str) -> Result<TelegramOutbox> {
    let parsed: TelegramOutbox = serde_json::from_str(text)?;
    for item in &parsed.items {
        match item {
            TelegramOutboxItem::Text { text } if text.trim().is_empty() => {
                bail!("Invalid telegram outbox text item.");
            }
            TelegramOutboxItem::Photo { path, .. } | TelegramOutboxItem::Document { path, .. }
                if path.trim().is_empty() =>
            {
                bail!("Invalid telegram outbox file item.");
            }
            _ => {}
        }
    }
    Ok(parsed)
}
