use std::path::PathBuf;

use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use teloxide::payloads::setters::*;
use teloxide::types::{ChatId, InputFile, Message, ParseMode, ThreadId};
use tokio::fs;
use tracing::{info, warn};

use super::{
    Bot, Requester, TelegramSystemIntent, TelegramTextRole, ThreadRecord, format_prefixed_text,
    format_role_text, format_system_text, telegram_role_marker,
};

pub const INLINE_MESSAGE_CHAR_LIMIT: usize = 4096;
const PREVIEW_CHAR_LIMIT: usize = 800;
const OVERFLOW_FILE_NAME: &str = "reply.md";
const OVERFLOW_NOTICE: &str =
    "Reply too long for inline Telegram delivery. Full response attached.";
const OVERFLOW_ATTACHMENT_LIMIT_NOTICE: &str =
    "Reply attachment exceeded Telegram's bot upload limit and wasn't delivered.";
const DEBUG_REPLY_MARKDOWN_FILE: &str = "final-reply-last.md";
const DEBUG_REPLY_HTML_FILE: &str = "final-reply-last.html";
const TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramReplyPlan {
    InlineHtml {
        text: String,
    },
    InlinePlainText {
        text: String,
        reason: &'static str,
    },
    MarkdownAttachment {
        notice_text: String,
        markdown: String,
    },
}

#[derive(Debug, Clone)]
struct ListState {
    next_index: u64,
    ordered: bool,
}

pub fn plan_final_assistant_reply(raw_text: &str, inline_limit: usize) -> TelegramReplyPlan {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return TelegramReplyPlan::InlinePlainText {
            text: String::new(),
            reason: "empty_reply",
        };
    }

    let html = render_markdown_to_telegram_html(trimmed);
    plan_final_assistant_reply_from_rendered(trimmed, html, inline_limit)
}

fn plan_final_assistant_reply_from_rendered(
    trimmed: &str,
    html: String,
    inline_limit: usize,
) -> TelegramReplyPlan {
    if html.trim().is_empty() {
        return TelegramReplyPlan::InlinePlainText {
            text: trimmed.to_owned(),
            reason: "html_render_empty",
        };
    }

    if html.chars().count() <= inline_limit {
        TelegramReplyPlan::InlineHtml { text: html }
    } else {
        TelegramReplyPlan::MarkdownAttachment {
            notice_text: build_overflow_notice(trimmed),
            markdown: trimmed.to_owned(),
        }
    }
}

pub(crate) async fn send_final_assistant_reply(
    bot: &Bot,
    record: &ThreadRecord,
    thread_id: Option<ThreadId>,
    raw_text: &str,
) -> Result<()> {
    let trimmed = raw_text.trim();
    let rendered_html = if trimmed.is_empty() {
        String::new()
    } else {
        render_role_markdown_to_telegram_html(TelegramTextRole::Assistant, trimmed)
    };
    write_debug_reply_dump(raw_text, &rendered_html).await;

    let plan = if trimmed.is_empty() {
        TelegramReplyPlan::InlinePlainText {
            text: String::new(),
            reason: "empty_reply",
        }
    } else {
        plan_final_assistant_reply_from_rendered(trimmed, rendered_html, INLINE_MESSAGE_CHAR_LIMIT)
    };

    match plan {
        TelegramReplyPlan::InlineHtml { text } => {
            match send_html_message(
                bot,
                ChatId(record.metadata.chat_id),
                thread_id,
                text.clone(),
            )
            .await
            {
                Ok(_) => {
                    info!(
                        event = "telegram.reply.rendered_html",
                        thread_key = %record.metadata.thread_key,
                        chat_id = record.metadata.chat_id,
                        message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                        "sent final assistant reply with telegram html renderer"
                    );
                }
                Err(error) => {
                    warn!(
                        event = "telegram.reply.fallback_plaintext",
                        thread_key = %record.metadata.thread_key,
                        chat_id = record.metadata.chat_id,
                        message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                        error = %error,
                        "telegram html send failed; retrying with plain text"
                    );
                    send_plain_text_message(
                        bot,
                        ChatId(record.metadata.chat_id),
                        thread_id,
                        format_role_text(TelegramTextRole::Assistant, raw_text.trim()),
                    )
                    .await?;
                }
            }
        }
        TelegramReplyPlan::InlinePlainText { text, reason } => {
            info!(
                event = "telegram.reply.fallback_plaintext",
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                reason = reason,
                "sending final assistant reply as plain text"
            );
            send_plain_text_message(
                bot,
                ChatId(record.metadata.chat_id),
                thread_id,
                format_role_text(TelegramTextRole::Assistant, &text),
            )
            .await?;
        }
        TelegramReplyPlan::MarkdownAttachment {
            notice_text,
            markdown,
        } => {
            info!(
                event = "telegram.reply.overflow_attachment",
                thread_key = %record.metadata.thread_key,
                chat_id = record.metadata.chat_id,
                message_thread_id = record.metadata.message_thread_id.unwrap_or_default(),
                "sending final assistant reply as markdown attachment"
            );
            send_plain_text_message(
                bot,
                ChatId(record.metadata.chat_id),
                thread_id,
                format_role_text(TelegramTextRole::Assistant, &notice_text),
            )
            .await?;
            if !send_markdown_attachment(bot, record, thread_id, markdown).await? {
                send_plain_text_message(
                    bot,
                    ChatId(record.metadata.chat_id),
                    thread_id,
                    format_system_text(
                        TelegramSystemIntent::Warning,
                        OVERFLOW_ATTACHMENT_LIMIT_NOTICE,
                    ),
                )
                .await?;
            }
        }
    }
    Ok(())
}

pub(crate) fn wrap_rendered_html_with_role(role: TelegramTextRole, rendered_html: &str) -> String {
    format_prefixed_text(telegram_role_marker(role), rendered_html)
}

pub(crate) fn render_role_markdown_to_telegram_html(
    role: TelegramTextRole,
    markdown: &str,
) -> String {
    let rendered = render_markdown_to_telegram_html(markdown);
    wrap_rendered_html_with_role(role, &rendered)
}

async fn send_html_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: String,
) -> Result<Message> {
    let request = bot
        .send_message(chat_id, text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(super::disabled_link_preview_options());
    let message = match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await?,
        None => request.await?,
    };
    Ok(message)
}

async fn send_plain_text_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    text: String,
) -> Result<Message> {
    let request = bot
        .send_message(chat_id, text)
        .link_preview_options(super::disabled_link_preview_options());
    let message = match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await?,
        None => request.await?,
    };
    Ok(message)
}

async fn send_markdown_attachment(
    bot: &Bot,
    record: &ThreadRecord,
    thread_id: Option<ThreadId>,
    markdown: String,
) -> Result<bool> {
    let attachment_path = overflow_attachment_path(record);
    if let Some(parent) = attachment_path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&attachment_path, markdown.as_bytes())
        .await
        .with_context(|| format!("failed to write {}", attachment_path.display()))?;

    let attachment_size = fs::metadata(&attachment_path)
        .await
        .with_context(|| format!("failed to stat {}", attachment_path.display()))?
        .len();
    if attachment_size > TELEGRAM_BOT_DOCUMENT_LIMIT_BYTES {
        if let Err(error) = fs::remove_file(&attachment_path).await {
            warn!(
                event = "telegram.reply.overflow_attachment.cleanup_failed",
                thread_key = %record.metadata.thread_key,
                path = %attachment_path.display(),
                error = %error,
                "failed to remove oversized overflow attachment"
            );
        }
        return Ok(false);
    }

    let request = bot.send_document(
        ChatId(record.metadata.chat_id),
        InputFile::file(attachment_path.clone()).file_name(OVERFLOW_FILE_NAME),
    );
    match thread_id {
        Some(thread_id) => request.message_thread_id(thread_id).await?,
        None => request.await?,
    };

    if let Err(error) = fs::remove_file(&attachment_path).await {
        warn!(
            event = "telegram.reply.overflow_attachment.cleanup_failed",
            thread_key = %record.metadata.thread_key,
            path = %attachment_path.display(),
            error = %error,
            "failed to remove overflow attachment after successful send"
        );
    }

    Ok(true)
}

fn overflow_attachment_path(record: &ThreadRecord) -> PathBuf {
    let timestamp = chrono::Utc::now().timestamp_millis();
    record
        .state_path()
        .join("telegram")
        .join(format!("overflow-reply-{timestamp}.md"))
}

fn build_overflow_notice(raw_text: &str) -> String {
    let preview = first_preview_snippet(raw_text);
    if preview.is_empty() {
        OVERFLOW_NOTICE.to_owned()
    } else {
        format!("{OVERFLOW_NOTICE}\n\n{preview}")
    }
}

fn first_preview_snippet(raw_text: &str) -> String {
    let paragraph = raw_text
        .split("\n\n")
        .map(str::trim)
        .find(|segment| !segment.is_empty())
        .unwrap_or(raw_text.trim());
    let compact = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&compact, PREVIEW_CHAR_LIMIT)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}

async fn write_debug_reply_dump(raw_markdown: &str, rendered_html: &str) {
    let dump_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp");
    if let Err(error) = fs::create_dir_all(&dump_dir).await {
        warn!(
            event = "telegram.reply.debug_dump.create_dir_failed",
            path = %dump_dir.display(),
            error = %error,
            "failed to create debug dump directory for final reply"
        );
        return;
    }

    let markdown_path = dump_dir.join(DEBUG_REPLY_MARKDOWN_FILE);
    if let Err(error) = fs::write(&markdown_path, raw_markdown.as_bytes()).await {
        warn!(
            event = "telegram.reply.debug_dump.write_markdown_failed",
            path = %markdown_path.display(),
            error = %error,
            "failed to write final reply markdown debug dump"
        );
    }

    let html_path = dump_dir.join(DEBUG_REPLY_HTML_FILE);
    if let Err(error) = fs::write(&html_path, rendered_html.as_bytes()).await {
        warn!(
            event = "telegram.reply.debug_dump.write_html_failed",
            path = %html_path.display(),
            error = %error,
            "failed to write final reply html debug dump"
        );
    }
}

pub(crate) fn render_markdown_to_telegram_html(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut renderer = TelegramHtmlRenderer::default();

    for event in parser {
        renderer.handle_event(event);
    }

    let html = rewrite_markdown_links_as_code(&renderer.finish());
    apply_layout_adjustments(&html)
}

#[derive(Default)]
struct TelegramHtmlRenderer {
    html: String,
    list_stack: Vec<ListState>,
    unsupported_depth: usize,
    unsupported_text: String,
}

impl TelegramHtmlRenderer {
    fn handle_event(&mut self, event: Event<'_>) {
        if self.unsupported_depth > 0 {
            self.handle_unsupported_event(event);
            return;
        }

        match event {
            Event::Start(tag) if is_unsupported_tag(&tag) => {
                self.unsupported_depth = 1;
                self.unsupported_text.clear();
            }
            Event::Start(Tag::Paragraph) => {
                if self.list_stack.is_empty() {
                    self.push_block_break();
                }
            }
            Event::End(Tag::Paragraph) => {
                if self.list_stack.is_empty() {
                    self.push_block_break();
                } else {
                    self.push_line_break();
                }
            }
            Event::Start(Tag::Heading(..)) => {
                self.push_block_break();
                self.html.push_str("<b>");
            }
            Event::End(Tag::Heading(..)) => {
                self.html.push_str("</b>");
                self.push_block_break();
            }
            Event::Start(Tag::Emphasis) => self.html.push_str("<i>"),
            Event::End(Tag::Emphasis) => self.html.push_str("</i>"),
            Event::Start(Tag::Strong) => self.html.push_str("<b>"),
            Event::End(Tag::Strong) => self.html.push_str("</b>"),
            Event::Start(Tag::Strikethrough) => self.html.push_str("<s>"),
            Event::End(Tag::Strikethrough) => self.html.push_str("</s>"),
            Event::Start(Tag::CodeBlock(kind)) => {
                self.push_block_break();
                self.html.push_str("<pre><code>");
                if let CodeBlockKind::Fenced(lang) = kind {
                    let lang = lang.trim();
                    if !lang.is_empty() {
                        self.html.push_str(&escape_html(lang));
                        self.html.push('\n');
                    }
                }
            }
            Event::End(Tag::CodeBlock(_)) => {
                self.html.push_str("</code></pre>");
                self.push_block_break();
            }
            Event::Start(Tag::List(start)) => {
                if self.list_stack.is_empty() {
                    self.push_block_break();
                } else {
                    self.push_line_break();
                }
                self.list_stack.push(ListState {
                    ordered: start.is_some(),
                    next_index: start.unwrap_or(1),
                });
            }
            Event::End(Tag::List(_)) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.push_block_break();
                } else {
                    self.push_line_break();
                }
            }
            Event::Start(Tag::Item) => {
                if !self.html.is_empty() && !self.html.ends_with('\n') {
                    self.html.push('\n');
                }
                let depth = self.list_stack.len().saturating_sub(1);
                self.html.push_str(&"  ".repeat(depth));
                if let Some(state) = self.list_stack.last_mut() {
                    if state.ordered {
                        self.html.push_str(&format!("{}. ", state.next_index));
                        state.next_index += 1;
                    } else {
                        self.html.push_str("- ");
                    }
                } else {
                    self.html.push_str("- ");
                }
            }
            Event::End(Tag::Item) => self.push_line_break(),
            Event::Start(Tag::Link(_, dest_url, _)) => {
                self.html
                    .push_str(&format!("<a href=\"{}\">", escape_html(&dest_url)));
            }
            Event::End(Tag::Link(..)) => {
                self.html.push_str("</a>");
            }
            Event::Start(Tag::Image(_, dest_url, _)) => {
                self.html
                    .push_str(&escape_html(&format!("Image: {}", dest_url)));
            }
            Event::End(Tag::Image(..)) => {}
            Event::Text(text) | Event::Html(text) => self.html.push_str(&escape_html(&text)),
            Event::Code(text) => {
                self.html.push_str(&render_inline_code(&text));
            }
            Event::SoftBreak | Event::HardBreak => self.push_line_break(),
            Event::Rule => {
                self.push_block_break();
                self.html.push_str("----");
                self.push_block_break();
            }
            Event::FootnoteReference(label) => {
                self.html.push_str(&escape_html(&format!("[{label}]")));
            }
            Event::TaskListMarker(checked) => {
                if checked {
                    self.html.push_str("[x] ");
                } else {
                    self.html.push_str("[ ] ");
                }
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }

    fn handle_unsupported_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(_) => {
                self.unsupported_depth += 1;
            }
            Event::End(_) => {
                self.unsupported_depth = self.unsupported_depth.saturating_sub(1);
                if self.unsupported_depth == 0 {
                    self.flush_unsupported_block();
                }
            }
            Event::Text(text) | Event::Html(text) => self.unsupported_text.push_str(&text),
            Event::Code(text) => {
                self.unsupported_text.push('`');
                self.unsupported_text.push_str(&text);
                self.unsupported_text.push('`');
            }
            Event::SoftBreak | Event::HardBreak => self.unsupported_text.push('\n'),
            Event::Rule => {
                if !self.unsupported_text.is_empty() && !self.unsupported_text.ends_with('\n') {
                    self.unsupported_text.push('\n');
                }
                self.unsupported_text.push_str("----\n");
            }
            Event::FootnoteReference(label) => {
                self.unsupported_text.push('[');
                self.unsupported_text.push_str(&label);
                self.unsupported_text.push(']');
            }
            Event::TaskListMarker(checked) => {
                if checked {
                    self.unsupported_text.push_str("[x] ");
                } else {
                    self.unsupported_text.push_str("[ ] ");
                }
            }
        }
    }

    fn flush_unsupported_block(&mut self) {
        let text = self.unsupported_text.trim().to_owned();
        if text.is_empty() {
            self.unsupported_text.clear();
            return;
        }
        self.push_block_break();
        self.html.push_str("<pre><code>");
        self.html.push_str(&escape_html(&text));
        self.html.push_str("</code></pre>");
        self.push_block_break();
        self.unsupported_text.clear();
    }

    fn push_line_break(&mut self) {
        if !self.html.ends_with('\n') {
            self.html.push('\n');
        }
    }

    fn push_block_break(&mut self) {
        if self.html.is_empty() {
            return;
        }
        if self.html.ends_with("\n\n") {
            return;
        }
        if self.html.ends_with('\n') {
            self.html.push('\n');
        } else {
            self.html.push_str("\n\n");
        }
    }

    fn finish(mut self) -> String {
        if self.unsupported_depth > 0 {
            self.unsupported_depth = 0;
            self.flush_unsupported_block();
        }
        self.html.trim().to_owned()
    }
}

fn is_unsupported_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::BlockQuote
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
    )
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn render_inline_code(text: &str) -> String {
    let escaped = escape_html(text);
    if looks_like_directory_label(text) {
        format!("<b>{escaped}</b>")
    } else {
        format!("<code>{escaped}</code>")
    }
}

fn looks_like_directory_label(text: &str) -> bool {
    text.ends_with('/')
        && !text.contains(char::is_whitespace)
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
}

fn apply_layout_adjustments(html: &str) -> String {
    let lines: Vec<&str> = html.lines().collect();
    let mut adjusted = Vec::with_capacity(lines.len());

    for (idx, line) in lines.iter().enumerate() {
        let next_non_empty = lines
            .iter()
            .skip(idx + 1)
            .find(|candidate| !candidate.trim().is_empty())
            .copied();

        let line = if should_bold_section_label(line, next_non_empty) {
            format!("<b>{}</b>", line.trim())
        } else {
            (*line).to_owned()
        };

        adjusted.push(line);
    }

    adjusted.join("\n")
}

fn should_bold_section_label(line: &str, next_non_empty: Option<&str>) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > 40 {
        return false;
    }
    if !(trimmed.ends_with('：') || trimmed.ends_with(':')) {
        return false;
    }
    next_non_empty.is_some_and(looks_like_bullet_line)
}

fn ordered_marker_len(text: &str) -> Option<usize> {
    let digit_count = text.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return None;
    }
    let suffix = text.get(digit_count..digit_count + 2)?;
    if suffix == ". " {
        Some(digit_count + 2)
    } else {
        None
    }
}

fn looks_like_bullet_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || ordered_marker_len(trimmed).is_some()
}

fn rewrite_markdown_links_as_code(html: &str) -> String {
    let mut rewritten = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.find("<a ") {
        rewritten.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(tag_end) = after_start.find('>') else {
            rewritten.push_str(after_start);
            return rewritten;
        };
        let label_start = start + tag_end + 1;
        let after_open = &rest[label_start..];
        let Some(close_start_rel) = after_open.find("</a>") else {
            rewritten.push_str(after_start);
            return rewritten;
        };
        let label = &after_open[..close_start_rel];
        if label.starts_with("<code>") && label.ends_with("</code>") {
            rewritten.push_str(label);
        } else {
            rewritten.push_str(&format!("<code>{}</code>", label));
        }
        rest = &after_open[close_start_rel + 4..];
    }

    rewritten.push_str(rest);
    rewritten
}

#[cfg(test)]
mod tests {
    use super::{
        OVERFLOW_NOTICE, TelegramReplyPlan, first_preview_snippet, plan_final_assistant_reply,
        render_markdown_to_telegram_html, render_role_markdown_to_telegram_html,
    };
    use crate::telegram_runtime::TelegramTextRole;

    #[test]
    fn renders_supported_markdown_to_html() {
        let plan = plan_final_assistant_reply(
            "# Heading\n\nUse `cargo test` in **this** repo.\n\n- one\n- two",
            4096,
        );
        let TelegramReplyPlan::InlineHtml { text } = plan else {
            panic!("expected inline html");
        };

        assert!(text.contains("<b>Heading</b>"));
        assert!(text.contains("<code>cargo test</code>"));
        assert!(text.contains("<b>this</b>"));
        assert!(text.contains("- one"));
        assert!(text.contains("- two"));
    }

    #[test]
    fn assistant_html_render_uses_command_header() {
        let html = render_role_markdown_to_telegram_html(TelegramTextRole::Assistant, "**Hello**");
        assert_eq!(html, "■ <b>Hello</b>");
    }

    #[test]
    fn reflows_local_file_reference_bullets() {
        let html = render_markdown_to_telegram_html(
            "項目總覽：\n- [README.md](/tmp/README.md) 說明 `threadBridge` 是 bot。",
        );

        assert!(html.contains("<b>項目總覽：</b>"));
        assert!(html.contains("- <code>README.md</code> 說明 <code>threadBridge</code> 是 bot。"));
    }

    #[test]
    fn preserves_two_line_file_bullets_without_added_indent() {
        let html = render_markdown_to_telegram_html(
            "- [docs/plan/session-lifecycle.md](/tmp/docs/plan/session-lifecycle.md)\n  重做 session / thread / workspace 的產品模型。",
        );

        assert!(html.contains(
            "- <code>docs/plan/session-lifecycle.md</code>\n重做 session / thread / workspace 的產品模型。"
        ));
    }

    #[test]
    fn preserves_numbered_item_with_prefix_text() {
        let html = render_markdown_to_telegram_html(
            "1. 入口层：[`codex-rs/README.md`](/tmp/codex-rs/README.md)\n2. 下一项",
        );

        assert!(html.contains("1. 入口层：<code>codex-rs/README.md</code>\n2. 下一项"));
    }

    #[test]
    fn rewrites_local_path_labels_as_code() {
        let html = render_markdown_to_telegram_html(
            "- [docs/callNanobanana.md](/tmp/docs/callNanobanana.md) 說明",
        );

        assert!(html.contains("- <code>docs/callNanobanana.md</code> 說明"));
    }

    #[test]
    fn rewrites_local_filename_labels_as_code() {
        let html =
            render_markdown_to_telegram_html("- [AGENTS.md](/tmp/AGENTS.md) 說明 maintainer guide");

        assert!(html.contains("- <code>AGENTS.md</code> 說明 maintainer guide"));
    }

    #[test]
    fn rewrites_external_links_as_code() {
        let html = render_markdown_to_telegram_html("- [OpenAI](https://openai.com) 說明 external");

        assert!(html.contains("- <code>OpenAI</code> 說明 external"));
    }

    #[test]
    fn preserves_bullets_that_only_contain_inline_code_mentions() {
        let html = render_markdown_to_telegram_html(
            "- 對普通用户可配置的 hooks，目前只有 `SessionStart` 和 `Stop`，配置文件是 `hooks.json`。",
        );

        assert_eq!(
            html,
            "- 對普通用户可配置的 hooks，目前只有 <code>SessionStart</code> 和 <code>Stop</code>，配置文件是 <code>hooks.json</code>。"
        );
    }

    #[test]
    fn renders_directory_like_inline_code_as_bold() {
        let html = render_markdown_to_telegram_html("例如 `pathofexile/`、`browser_control/`。");

        assert_eq!(html, "例如 <b>pathofexile/</b>、<b>browser_control/</b>。");
    }

    #[test]
    fn keeps_regular_inline_code_as_code() {
        let html = render_markdown_to_telegram_html("保留 `hooks.json` 和 `SessionStart`。");

        assert_eq!(
            html,
            "保留 <code>hooks.json</code> 和 <code>SessionStart</code>。"
        );
    }

    #[test]
    fn nested_lists_do_not_insert_extra_blank_line() {
        let html = render_markdown_to_telegram_html(
            "- 幫你把零散需求落成具體變更，例如：\n  - 加一個 Telegram helper\n  - 修 PoE 自動化腳本",
        );

        assert!(html.contains(
            "- 幫你把零散需求落成具體變更，例如：\n  - 加一個 Telegram helper\n  - 修 PoE 自動化腳本"
        ));
        assert!(!html.contains("例如：\n\n  - 加一個 Telegram helper"));
    }

    #[test]
    fn escapes_raw_html() {
        let plan = plan_final_assistant_reply("<script>alert(1)</script>", 4096);
        let TelegramReplyPlan::InlineHtml { text } = plan else {
            panic!("expected inline html");
        };

        assert!(text.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!text.contains("<script>"));
    }

    #[test]
    fn unsupported_blocks_fallback_to_code_block() {
        let plan = plan_final_assistant_reply("> nested\n> quote", 4096);
        let TelegramReplyPlan::InlineHtml { text } = plan else {
            panic!("expected inline html");
        };

        assert!(text.contains("<pre><code>nested\nquote</code></pre>"));
    }

    #[test]
    fn oversized_replies_switch_to_markdown_attachment() {
        let raw = "a".repeat(5000);
        let plan = plan_final_assistant_reply(&raw, 4096);
        let TelegramReplyPlan::MarkdownAttachment {
            notice_text,
            markdown,
        } = plan
        else {
            panic!("expected markdown attachment");
        };

        assert!(notice_text.starts_with(OVERFLOW_NOTICE));
        assert_eq!(markdown, raw);
    }

    #[test]
    fn preview_snippet_uses_first_non_empty_paragraph() {
        let snippet = first_preview_snippet("\n\nFirst paragraph here.\n\nSecond paragraph");
        assert_eq!(snippet, "First paragraph here.");
    }

    #[test]
    fn empty_reply_stays_plain_text() {
        let plan = plan_final_assistant_reply("   ", 4096);
        assert_eq!(
            plan,
            TelegramReplyPlan::InlinePlainText {
                text: String::new(),
                reason: "empty_reply",
            }
        );
    }
}
