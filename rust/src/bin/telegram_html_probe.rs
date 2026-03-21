use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;
use teloxide::types::{ChatId, LinkPreviewOptions, MessageId, ParseMode, ThreadId};
use threadbridge_rust::config::load_app_config;
use threadbridge_rust::telegram_runtime::final_reply::{
    INLINE_MESSAGE_CHAR_LIMIT, TelegramReplyPlan, plan_final_assistant_reply,
};

#[derive(Debug)]
struct ProbeArgs {
    sample: Option<String>,
    thread_key: Option<String>,
    line: Option<usize>,
    chat_id: Option<i64>,
    message_thread_id: Option<i32>,
    no_send: bool,
}

#[derive(Debug, Deserialize)]
struct ConversationEntry {
    direction: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ThreadMetadata {
    chat_id: i64,
    message_thread_id: Option<i32>,
    updated_at: String,
}

#[derive(Debug)]
struct SelectedSample {
    raw_text: String,
    html: String,
    source_path: PathBuf,
    line_no: usize,
    chat_id: i64,
    message_thread_id: Option<i32>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let config = load_app_config()?;
    let selected = select_sample(&config.runtime.data_root_path, &args)?;

    println!(
        "Sample: {}:{}",
        selected.source_path.display(),
        selected.line_no
    );
    println!("Chat: {}", args.chat_id.unwrap_or(selected.chat_id));
    println!(
        "Thread: {}",
        args.message_thread_id
            .or(selected.message_thread_id)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned())
    );
    println!("\n== Raw Markdown ==\n{}\n", selected.raw_text);
    println!("== Telegram HTML ==\n{}\n", selected.html);

    if args.no_send {
        return Ok(());
    }

    if selected.html.trim().is_empty() {
        bail!("rendered html is empty");
    }
    if selected.html.chars().count() > 4096 {
        bail!(
            "rendered html is too long for a single Telegram message: {} chars",
            selected.html.chars().count()
        );
    }

    let bot = teloxide::Bot::new(config.telegram.telegram_token);
    let chat_id = ChatId(args.chat_id.unwrap_or(selected.chat_id));
    let request = bot
        .send_message(chat_id, selected.html.clone())
        .parse_mode(ParseMode::Html)
        .link_preview_options(LinkPreviewOptions {
            is_disabled: true,
            url: None,
            prefer_small_media: false,
            prefer_large_media: false,
            show_above_text: false,
        });
    let message = match args.message_thread_id.or(selected.message_thread_id) {
        Some(thread_id) => {
            request
                .message_thread_id(ThreadId(MessageId(thread_id)))
                .await?
        }
        None => request.await?,
    };

    println!("Sent message_id {}", message.id.0);
    Ok(())
}

fn parse_args() -> Result<ProbeArgs> {
    let mut sample = None;
    let mut thread_key = None;
    let mut line = None;
    let mut chat_id = None;
    let mut message_thread_id = None;
    let mut no_send = false;

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--sample" => {
                sample = Some(iter.next().context(
                    "missing value for --sample, expected path/to/conversations.jsonl:line",
                )?);
            }
            "--thread-key" => {
                thread_key = Some(iter.next().context("missing value for --thread-key")?);
            }
            "--line" => {
                let value = iter.next().context("missing value for --line")?;
                line = Some(
                    value
                        .parse::<usize>()
                        .with_context(|| format!("invalid --line value: {value}"))?,
                );
            }
            "--chat-id" => {
                let value = iter.next().context("missing value for --chat-id")?;
                chat_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --chat-id value: {value}"))?,
                );
            }
            "--message-thread-id" => {
                let value = iter
                    .next()
                    .context("missing value for --message-thread-id")?;
                message_thread_id = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("invalid --message-thread-id value: {value}"))?,
                );
            }
            "--no-send" => {
                no_send = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    if sample.is_some() && (thread_key.is_some() || line.is_some()) {
        bail!("use either --sample or --thread-key/--line");
    }

    if thread_key.is_some() && line.is_none() {
        bail!("--thread-key requires --line");
    }

    Ok(ProbeArgs {
        sample,
        thread_key,
        line,
        chat_id,
        message_thread_id,
        no_send,
    })
}

fn print_usage() {
    eprintln!(
        "Usage:\n  cargo run --bin telegram_html_probe -- --sample data/<thread-key>/conversations.jsonl:<line>\n  cargo run --bin telegram_html_probe -- --thread-key <thread-key> --line <line>\n\nOptional:\n  --chat-id <id>\n  --message-thread-id <id>\n  --no-send"
    );
}

fn select_sample(data_root: &Path, args: &ProbeArgs) -> Result<SelectedSample> {
    if let Some(spec) = &args.sample {
        return load_sample_from_spec(spec, data_root);
    }
    if let Some(thread_key) = &args.thread_key {
        let line = args.line.context("missing --line")?;
        let path = data_root.join(thread_key).join("conversations.jsonl");
        return load_sample_from_path(&path, line);
    }
    load_latest_assistant_sample(data_root)
}

fn load_sample_from_spec(spec: &str, _data_root: &Path) -> Result<SelectedSample> {
    let (path_part, line_part) = spec
        .rsplit_once(':')
        .with_context(|| format!("invalid sample spec: {spec}"))?;
    let line_no = line_part
        .parse::<usize>()
        .with_context(|| format!("invalid line number in sample spec: {spec}"))?;
    let path = PathBuf::from(path_part);
    let resolved = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .context("failed to read current working directory")?
            .join(path)
    };
    load_sample_from_path(&resolved, line_no)
}

fn load_latest_assistant_sample(data_root: &Path) -> Result<SelectedSample> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(data_root)
        .with_context(|| format!("failed to read {}", data_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let metadata_path = path.join("metadata.json");
        let conversation_path = path.join("conversations.jsonl");
        if !metadata_path.exists() || !conversation_path.exists() {
            continue;
        }
        let metadata_text = fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?;
        let metadata: ThreadMetadata = serde_json::from_str(&metadata_text)
            .with_context(|| format!("invalid json in {}", metadata_path.display()))?;
        candidates.push((metadata.updated_at, conversation_path));
    }

    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((_, path)) = candidates.pop() else {
        bail!("no conversations found under {}", data_root.display());
    };

    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    for (idx, line) in lines.iter().enumerate().rev() {
        let entry: ConversationEntry = serde_json::from_str(line)
            .with_context(|| format!("invalid json in {}:{}", path.display(), idx + 1))?;
        if entry.direction == "assistant" && !entry.text.trim().is_empty() {
            return load_sample_from_path(&path, idx + 1);
        }
    }

    bail!("no assistant sample found in {}", path.display())
}

fn load_sample_from_path(path: &Path, line_no: usize) -> Result<SelectedSample> {
    if line_no == 0 {
        bail!("line numbers are 1-based");
    }

    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let line = text
        .lines()
        .nth(line_no - 1)
        .with_context(|| format!("missing line {} in {}", line_no, path.display()))?;
    let entry: ConversationEntry = serde_json::from_str(line)
        .with_context(|| format!("invalid json in {}:{}", path.display(), line_no))?;
    if entry.direction != "assistant" {
        bail!("{}:{} is not an assistant message", path.display(), line_no);
    }

    let raw_text = entry.text.trim().to_owned();
    if raw_text.is_empty() {
        bail!("{}:{} is empty after trim", path.display(), line_no);
    }

    let thread_dir = path
        .parent()
        .with_context(|| format!("missing parent directory for {}", path.display()))?;
    let metadata_path = thread_dir.join("metadata.json");
    let metadata_text = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata: ThreadMetadata = serde_json::from_str(&metadata_text)
        .with_context(|| format!("invalid json in {}", metadata_path.display()))?;

    Ok(SelectedSample {
        html: match plan_final_assistant_reply(&raw_text, INLINE_MESSAGE_CHAR_LIMIT) {
            TelegramReplyPlan::InlineHtml { text } => text,
            TelegramReplyPlan::InlinePlainText { text, .. } => text,
            TelegramReplyPlan::MarkdownAttachment { notice_text, .. } => notice_text,
        },
        raw_text,
        source_path: path.to_path_buf(),
        line_no,
        chat_id: metadata.chat_id,
        message_thread_id: metadata.message_thread_id,
    })
}
