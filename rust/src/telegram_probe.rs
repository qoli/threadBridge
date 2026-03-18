use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

pub fn render_markdown_to_telegram_html(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut renderer = ProbeHtmlRenderer::default();

    for event in parser {
        renderer.handle_event(event);
    }

    apply_probe_layout_adjustments(&renderer.finish())
}

#[derive(Debug, Clone)]
struct ListState {
    next_index: u64,
    ordered: bool,
}

#[derive(Default)]
struct ProbeHtmlRenderer {
    html: String,
    list_stack: Vec<ListState>,
}

impl ProbeHtmlRenderer {
    fn handle_event(&mut self, event: Event<'_>) {
        match event {
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
                self.push_block_break();
                self.list_stack.push(ListState {
                    ordered: start.is_some(),
                    next_index: start.unwrap_or(1),
                });
            }
            Event::End(Tag::List(_)) => {
                self.list_stack.pop();
                self.push_block_break();
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
                self.html
                    .push_str(&format!("<code>{}</code>", escape_html(&text)));
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

    fn finish(self) -> String {
        self.html.trim().to_owned()
    }
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

fn apply_probe_layout_adjustments(html: &str) -> String {
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
        } else if let Some(reflowed) = reflow_file_reference_bullet(line) {
            reflowed
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

fn reflow_file_reference_bullet(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let indent = line.len().saturating_sub(trimmed.len());
    let marker_len = if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        2
    } else {
        ordered_marker_len(trimmed)?
    };
    let marker = &trimmed[..marker_len];
    let content = &trimmed[marker_len..];

    if let Some(close) = content.find("</a>") {
        let reference = rewrite_anchor_reference_for_probe(&content[..close + 4]);
        let description = content[close + 4..].trim_start();
        if !description.is_empty() {
            return Some(format!(
                "{}{}\n{}{}",
                " ".repeat(indent),
                format!("{marker}{reference}"),
                continuation_indent(indent, marker_len),
                description
            ));
        }
    }

    if let Some(close) = content.find("</code>") {
        let reference = &content[..close + 7];
        let description = content[close + 7..].trim_start();
        if !description.is_empty() {
            return Some(format!(
                "{}{}\n{}{}",
                " ".repeat(indent),
                format!("{marker}{reference}"),
                continuation_indent(indent, marker_len),
                description
            ));
        }
    }

    None
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

fn continuation_indent(indent: usize, marker_len: usize) -> String {
    let width = if marker_len == 0 { 0 } else { 1 };
    format!("{}{}", " ".repeat(indent), "　".repeat(width))
}

fn rewrite_anchor_reference_for_probe(anchor_html: &str) -> String {
    let Some(start) = anchor_html.find('>') else {
        return anchor_html.to_owned();
    };
    let Some(end) = anchor_html.rfind("</a>") else {
        return anchor_html.to_owned();
    };

    let open = &anchor_html[..=start];
    let label = &anchor_html[start + 1..end];
    let close = &anchor_html[end..];
    let href = extract_anchor_href(open);

    if href
        .as_deref()
        .is_some_and(|value| is_local_reference_href(value))
        && !label.contains(' ')
    {
        return format!("<code>{}</code>", label);
    }

    if label.contains(' ') {
        return anchor_html.to_owned();
    }

    format!("{open}{label}{close}")
}

fn extract_anchor_href(open_tag: &str) -> Option<String> {
    let (_, rest) = open_tag.split_once("href=\"")?;
    let (href, _) = rest.split_once('"')?;
    Some(href.to_owned())
}

fn is_local_reference_href(href: &str) -> bool {
    href.starts_with('/') || href.starts_with("./") || href.starts_with("../")
}

#[cfg(test)]
mod tests {
    use super::render_markdown_to_telegram_html;

    #[test]
    fn renders_supported_markdown_subset() {
        let html = render_markdown_to_telegram_html(
            "# Heading\n\n- [README.md](/tmp/README.md) use `cargo test`\n- **Bold** item",
        );

        assert!(html.contains("<b>Heading</b>"));
        assert!(html.contains("- <code>README.md</code>\n　use <code>cargo test</code>"));
        assert!(html.contains("- <b>Bold</b> item"));
    }

    #[test]
    fn escapes_raw_html() {
        let html = render_markdown_to_telegram_html("<script>alert(1)</script>");

        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn reflows_file_reference_bullet_into_two_lines() {
        let html = render_markdown_to_telegram_html(
            "- [README.md](/tmp/README.md) 說明 `threadBridge` 是 bot。",
        );

        assert!(
            html.contains("- <code>README.md</code>\n　說明 <code>threadBridge</code> 是 bot。")
        );
    }

    #[test]
    fn bolds_short_section_labels_before_lists() {
        let html =
            render_markdown_to_telegram_html("項目總覽：\n- [README.md](/tmp/README.md) 說明");

        assert!(html.contains("<b>項目總覽：</b>"));
    }

    #[test]
    fn rewrites_slashy_anchor_labels_as_code() {
        let html = render_markdown_to_telegram_html(
            "- [docs/callNanobanana.md](/tmp/docs/callNanobanana.md) 說明",
        );

        assert!(html.contains("- <code>docs/callNanobanana.md</code>\n　說明"));
    }

    #[test]
    fn rewrites_local_filename_anchor_labels_as_code() {
        let html =
            render_markdown_to_telegram_html("- [AGENTS.md](/tmp/AGENTS.md) 說明 maintainer guide");

        assert!(html.contains("- <code>AGENTS.md</code>\n　說明 maintainer guide"));
    }

    #[test]
    fn keeps_external_links_as_links() {
        let html = render_markdown_to_telegram_html("- [OpenAI](https://openai.com) 說明 external");

        assert!(html.contains("- <a href=\"https://openai.com\">OpenAI</a>\n　說明 external"));
    }
}
