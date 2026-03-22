use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use teloxide::payloads::setters::*;
use teloxide::types::ChatAction;
use tokio::sync::{Mutex, oneshot};
use tracing::warn;

use crate::repository::{TranscriptMirrorDelivery, TranscriptMirrorEntry};

use super::final_reply::render_markdown_to_telegram_html;
use super::{Bot, ChatId, CodexThreadEvent, Requester, Result, ThreadId, thread_id_to_i32};

const TYPING_HEARTBEAT_SECONDS: u64 = 4;
const PREVIEW_HEARTBEAT_SECONDS: u64 = 4;
static NEXT_PREVIEW_DRAFT_ID: AtomicI32 = AtomicI32::new(1);

pub(crate) struct TypingHeartbeat {
    stop_tx: Option<oneshot::Sender<()>>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl TypingHeartbeat {
    pub(crate) fn start(bot: Bot, chat_id: ChatId, thread_id: Option<ThreadId>) -> Self {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            loop {
                let request = bot.send_chat_action(chat_id, ChatAction::Typing);
                let _ = match thread_id {
                    Some(thread_id) => request.message_thread_id(thread_id).await,
                    None => request.await,
                };
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(TYPING_HEARTBEAT_SECONDS)) => {}
                    _ = &mut stop_rx => break,
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            join_handle,
        }
    }

    pub(crate) async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        let _ = self.join_handle.await;
    }
}

pub(crate) struct PreviewHeartbeat {
    stop_tx: Option<oneshot::Sender<()>>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl PreviewHeartbeat {
    pub(crate) fn start(preview: Arc<Mutex<TurnPreviewController>>) -> Self {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(PREVIEW_HEARTBEAT_SECONDS)) => {
                        preview.lock().await.heartbeat().await;
                    }
                    _ = &mut stop_rx => break,
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            join_handle,
        }
    }

    pub(crate) async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        let _ = self.join_handle.await;
    }
}

fn summarize_text(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let truncated: String = compact.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}

fn truncate_preserving_layout(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_owned();
    }
    chars
        .into_iter()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn next_preview_draft_id() -> i32 {
    let draft_id = NEXT_PREVIEW_DRAFT_ID.fetch_add(1, Ordering::Relaxed);
    if draft_id <= 0 {
        NEXT_PREVIEW_DRAFT_ID.store(2, Ordering::Relaxed);
        return 1;
    }
    draft_id
}

#[derive(Debug, Serialize)]
pub(crate) struct SendMessageDraftRequest {
    pub(crate) chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_thread_id: Option<i32>,
    pub(crate) draft_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parse_mode: Option<&'static str>,
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
struct TelegramMethodEnvelope {
    ok: bool,
    description: Option<String>,
}

async fn send_message_draft(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    draft_id: i32,
    text: &str,
    parse_mode: Option<&'static str>,
) -> Result<()> {
    let mut url = bot.api_url();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("telegram api url cannot be a base url"))?;
        segments.push(&format!("bot{}", bot.token()));
        segments.push("sendMessageDraft");
    }

    let payload = SendMessageDraftRequest {
        chat_id: chat_id.0,
        message_thread_id: thread_id.map(thread_id_to_i32),
        draft_id,
        parse_mode,
        text: text.to_owned(),
    };
    let response = bot
        .client()
        .post(url)
        .json(&payload)
        .send()
        .await
        .context("failed to call Telegram sendMessageDraft")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read Telegram sendMessageDraft response")?;
    let envelope: TelegramMethodEnvelope = serde_json::from_str(&body)
        .with_context(|| format!("invalid Telegram sendMessageDraft response: {body}"))?;
    if !status.is_success() || !envelope.ok {
        anyhow::bail!(
            "Telegram sendMessageDraft failed (HTTP {}): {}",
            status,
            envelope
                .description
                .unwrap_or_else(|| "unknown Telegram error".to_owned())
        );
    }
    Ok(())
}

fn preview_status(label: &str) -> String {
    label.to_owned()
}

fn preview_heartbeat_marker(frame: usize) -> &'static str {
    const FRAMES: [&str; 2] = ["○", "●"];
    FRAMES[frame % FRAMES.len()]
}

fn render_draft_html(text: &str) -> String {
    let (marker, body) = text
        .split_once(' ')
        .map(|(marker, body)| (marker.trim(), body.trim()))
        .unwrap_or((text.trim(), ""));
    if body.is_empty() {
        marker.to_owned()
    } else {
        format!("{marker} {}", render_markdown_to_telegram_html(body))
    }
}

pub(crate) struct PreviewRenderer {
    status: String,
    status_frame: usize,
    draft_text: String,
    final_response: String,
    latest_render: String,
    max_chars: usize,
    in_progress: bool,
}

impl PreviewRenderer {
    pub(crate) fn new(max_chars: usize, _command_output_tail_chars: usize) -> Self {
        Self {
            status: preview_status("Preparing reply..."),
            status_frame: 0,
            draft_text: String::new(),
            final_response: String::new(),
            latest_render: String::new(),
            max_chars,
            in_progress: true,
        }
    }

    pub(crate) fn consume(&mut self, event: &CodexThreadEvent) -> bool {
        match event {
            CodexThreadEvent::ThreadStarted { .. } => {
                self.in_progress = true;
                self.status = preview_status("Preparing reply...")
            }
            CodexThreadEvent::TurnStarted => {
                self.in_progress = true;
                self.status = preview_status("Reading context...");
            }
            CodexThreadEvent::TurnCompleted { .. } => {
                self.in_progress = false;
                self.status = preview_status("Finalizing...");
            }
            CodexThreadEvent::TurnFailed { error } => {
                self.in_progress = false;
                self.status = format!(
                    "Preview unavailable\n{}",
                    summarize_text(&error.to_string(), 120)
                );
            }
            CodexThreadEvent::Error { message } => {
                self.in_progress = false;
                self.status = format!("Preview unavailable\n{}", summarize_text(message, 120));
            }
            CodexThreadEvent::ItemStarted { item }
            | CodexThreadEvent::ItemUpdated { item }
            | CodexThreadEvent::ItemCompleted { item } => {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("agent_message") => {
                        let text = item
                            .get("text")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_owned();
                        if !text.is_empty() {
                            self.draft_text = text.clone();
                            self.final_response = text;
                            self.status = preview_status("Drafting...");
                        }
                    }
                    Some("reasoning") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Thinking...");
                        }
                    }
                    Some("command_execution") | Some("mcp_tool_call") | Some("web_search") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Using tools...");
                        }
                    }
                    Some("todo_list") => {
                        if self.draft_text.is_empty() {
                            self.status = preview_status("Planning...");
                        }
                    }
                    _ => {}
                }
            }
        }
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    pub(crate) fn consume_process_entry(&mut self, entry: &TranscriptMirrorEntry) -> bool {
        if entry.delivery != TranscriptMirrorDelivery::Process {
            return false;
        }
        let text = entry.text.trim();
        if text.is_empty() {
            return false;
        }
        self.status = preview_status(text);
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    pub(crate) fn consume_preview_text(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        self.in_progress = true;
        self.status = preview_status("Drafting...");
        self.draft_text = text.to_owned();
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    pub(crate) fn heartbeat(&mut self) -> bool {
        if !self.in_progress {
            return false;
        }
        self.status_frame = self.status_frame.wrapping_add(1);
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }

    pub(crate) fn reset_for_new_turn(&mut self) {
        self.status = preview_status("Preparing reply...");
        self.status_frame = 0;
        self.draft_text.clear();
        self.final_response.clear();
        self.latest_render.clear();
        self.in_progress = true;
    }

    fn render_text(&self) -> String {
        let marker = preview_heartbeat_marker(self.status_frame);
        let body = if !self.draft_text.is_empty() {
            self.draft_text.as_str()
        } else {
            self.status.as_str()
        };
        let text = if body.is_empty() {
            marker.to_owned()
        } else {
            format!("{marker} {body}")
        };
        truncate_preserving_layout(&text, self.max_chars)
    }

    pub(crate) fn get_render_text(&self) -> &str {
        &self.latest_render
    }

    pub(crate) fn get_final_response(&self) -> &str {
        &self.final_response
    }

    pub(crate) fn complete_with_final_text(&mut self, final_text: &str) -> bool {
        let final_text = final_text.trim();
        if final_text.is_empty() {
            return false;
        }
        self.in_progress = false;
        self.status = preview_status("Finalized");
        self.draft_text = final_text.to_owned();
        self.final_response = final_text.to_owned();
        let next_render = self.render_text();
        let changed = next_render != self.latest_render;
        self.latest_render = next_render;
        changed
    }
}

pub(crate) struct TurnPreviewController {
    bot: Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    renderer: PreviewRenderer,
    draft_id: i32,
    last_sent_text: String,
    last_update_at: Option<Instant>,
    edit_interval: Duration,
}

impl TurnPreviewController {
    pub(crate) fn new(
        bot: Bot,
        chat_id: ChatId,
        thread_id: Option<ThreadId>,
        max_chars: usize,
        command_output_tail_chars: usize,
        edit_interval_ms: u64,
    ) -> Self {
        Self {
            bot,
            chat_id,
            thread_id,
            renderer: PreviewRenderer::new(max_chars, command_output_tail_chars),
            draft_id: next_preview_draft_id(),
            last_sent_text: String::new(),
            last_update_at: None,
            edit_interval: Duration::from_millis(edit_interval_ms),
        }
    }

    pub(crate) async fn consume(&mut self, event: &CodexThreadEvent) {
        if !self.renderer.consume(event) {
            return;
        }
        self.flush_render().await;
    }

    pub(crate) async fn consume_process_entry(&mut self, entry: &TranscriptMirrorEntry) {
        if !self.renderer.consume_process_entry(entry) {
            return;
        }
        self.flush_render().await;
    }

    pub(crate) async fn consume_preview_text(&mut self, text: &str) {
        if !self.renderer.consume_preview_text(text) {
            return;
        }
        self.flush_render().await;
    }

    pub(crate) async fn heartbeat(&mut self) {
        if !self.renderer.heartbeat() {
            return;
        }
        self.flush_render().await;
    }

    pub(crate) fn reset_for_new_turn(&mut self) {
        self.renderer.reset_for_new_turn();
        self.last_sent_text.clear();
        self.last_update_at = None;
    }

    async fn flush_render(&mut self) {
        let render_text = self.renderer.get_render_text().trim();
        if render_text.is_empty() || render_text == self.last_sent_text {
            return;
        }
        if let Some(last_update_at) = self.last_update_at {
            if last_update_at.elapsed() < self.edit_interval {
                return;
            }
        }
        let capped = truncate_preserving_layout(render_text, 4096);
        let html_text = render_draft_html(&capped);
        match send_message_draft(
            &self.bot,
            self.chat_id,
            self.thread_id,
            self.draft_id,
            &html_text,
            Some("HTML"),
        )
        .await
        {
            Ok(()) => {
                self.last_sent_text = capped;
                self.last_update_at = Some(Instant::now());
            }
            Err(error) => {
                warn!(
                    event = "telegram.preview.draft.html_failed",
                    chat_id = self.chat_id.0,
                    draft_id = self.draft_id,
                    error = %error,
                    "failed to send preview draft with html parse mode; retrying as plain text"
                );
                match send_message_draft(
                    &self.bot,
                    self.chat_id,
                    self.thread_id,
                    self.draft_id,
                    &capped,
                    None,
                )
                .await
                {
                    Ok(()) => {
                        self.last_sent_text = capped;
                        self.last_update_at = Some(Instant::now());
                    }
                    Err(error) => {
                        warn!(
                            event = "telegram.preview.draft.failed",
                            chat_id = self.chat_id.0,
                            draft_id = self.draft_id,
                            error = %error,
                            "failed to send preview draft"
                        );
                    }
                }
            }
        }
    }

    pub(crate) async fn complete(&mut self, _final_text: &str) -> bool {
        if !self.renderer.complete_with_final_text(_final_text) {
            return false;
        }
        self.last_update_at = None;
        self.flush_render().await;
        true
    }

    pub(crate) fn fallback_final_response(&self) -> &str {
        self.renderer.get_final_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{PreviewRenderer, SendMessageDraftRequest};
    use serde_json::json;
    use teloxide::types::ChatId;

    use crate::codex::CodexThreadEvent;
    use crate::repository::{
        TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
        TranscriptMirrorPhase, TranscriptMirrorRole,
    };

    #[test]
    fn preview_renderer_applies_heartbeat_to_draft_text() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume(&CodexThreadEvent::TurnStarted);
        assert_eq!(renderer.get_render_text(), "○ Reading context...");

        renderer.consume(&CodexThreadEvent::ItemUpdated {
            item: json!({
                "type": "agent_message",
                "text": "First draft paragraph"
            }),
        });
        assert_eq!(renderer.get_render_text(), "○ First draft paragraph");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "● First draft paragraph");
    }

    #[test]
    fn preview_renderer_heartbeat_rotates_prefix_marker() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume(&CodexThreadEvent::TurnStarted);
        assert_eq!(renderer.get_render_text(), "○ Reading context...");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "● Reading context...");

        renderer.heartbeat();
        assert_eq!(renderer.get_render_text(), "○ Reading context...");
    }

    #[test]
    fn send_message_draft_payload_serializes_thread_id_only_when_present() {
        let with_thread = SendMessageDraftRequest {
            chat_id: ChatId(42).0,
            message_thread_id: Some(7),
            draft_id: 11,
            parse_mode: Some("HTML"),
            text: "draft".to_owned(),
        };
        let without_thread = SendMessageDraftRequest {
            chat_id: ChatId(42).0,
            message_thread_id: None,
            draft_id: 11,
            parse_mode: None,
            text: "draft".to_owned(),
        };

        let with_thread = serde_json::to_value(with_thread).unwrap();
        let without_thread = serde_json::to_value(without_thread).unwrap();

        assert_eq!(with_thread["message_thread_id"], 7);
        assert!(without_thread.get("message_thread_id").is_none());
    }

    #[test]
    fn preview_renderer_uses_process_entry_summary() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume(&CodexThreadEvent::TurnStarted);
        let changed = renderer.consume_process_entry(&TranscriptMirrorEntry {
            timestamp: "2026-03-22T00:00:00.000Z".to_owned(),
            session_id: "session-1".to_owned(),
            origin: TranscriptMirrorOrigin::Telegram,
            role: TranscriptMirrorRole::Assistant,
            delivery: TranscriptMirrorDelivery::Process,
            phase: Some(TranscriptMirrorPhase::Tool),
            text: "Command: cargo test".to_owned(),
        });
        assert!(changed);
        assert_eq!(renderer.get_render_text(), "○ Command: cargo test");
    }

    #[test]
    fn preview_renderer_reset_allows_same_process_summary_next_turn() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        assert!(renderer.consume_process_entry(&TranscriptMirrorEntry {
            timestamp: "2026-03-22T00:00:00.000Z".to_owned(),
            session_id: "session-1".to_owned(),
            origin: TranscriptMirrorOrigin::Telegram,
            role: TranscriptMirrorRole::Assistant,
            delivery: TranscriptMirrorDelivery::Process,
            phase: Some(TranscriptMirrorPhase::Tool),
            text: "Command: cargo test".to_owned(),
        }));
        renderer.reset_for_new_turn();
        assert!(renderer.consume_process_entry(&TranscriptMirrorEntry {
            timestamp: "2026-03-22T00:00:01.000Z".to_owned(),
            session_id: "session-1".to_owned(),
            origin: TranscriptMirrorOrigin::Telegram,
            role: TranscriptMirrorRole::Assistant,
            delivery: TranscriptMirrorDelivery::Process,
            phase: Some(TranscriptMirrorPhase::Tool),
            text: "Command: cargo test".to_owned(),
        }));
        assert_eq!(renderer.get_render_text(), "○ Command: cargo test");
    }

    #[test]
    fn preview_renderer_uses_preview_text_without_touching_final_response() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        assert!(
            renderer
                .consume_preview_text("我先直接查看這個 repo 最近 2 個 commit，整理出摘要給你。")
        );
        assert_eq!(
            renderer.get_render_text(),
            "○ 我先直接查看這個 repo 最近 2 個 commit，整理出摘要給你。"
        );
        assert_eq!(renderer.get_final_response(), "");
    }

    #[test]
    fn preview_renderer_complete_replaces_draft_with_last_marker() {
        let mut renderer = PreviewRenderer::new(3500, 800);
        renderer.consume_preview_text("我先查一下这个仓库最近 2 个 commit。");
        assert!(renderer.complete_with_final_text("最近 2 個 commit 都在修 redraw。"));
        assert_eq!(
            renderer.get_render_text(),
            "○ 最近 2 個 commit 都在修 redraw。"
        );
        assert_eq!(
            renderer.get_final_response(),
            "最近 2 個 commit 都在修 redraw。"
        );
    }
}
