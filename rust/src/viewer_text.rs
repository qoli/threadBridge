use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::repository::{TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorRole};

const LABEL_TELEGRAM: &str = "Telegram";
const LABEL_CLI: &str = "CLI";
const LABEL_CODEX: &str = "Codex";
const BODY_SEPARATOR: &str = " │ ";

pub fn parse_transcript_mirror_jsonl(content: &str) -> Result<Vec<TranscriptMirrorEntry>> {
    let mut entries = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if let Some(entry) = parse_transcript_mirror_line(line)
            .with_context(|| format!("invalid transcript mirror JSONL at line {}", index + 1))?
        {
            entries.push(entry);
        }
    }
    Ok(entries)
}

pub fn parse_transcript_mirror_line(line: &str) -> Result<Option<TranscriptMirrorEntry>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(trimmed)?))
}

pub fn read_transcript_mirror_jsonl(path: &Path) -> Result<Vec<TranscriptMirrorEntry>> {
    match fs::read_to_string(path) {
        Ok(content) => parse_transcript_mirror_jsonl(&content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn filter_transcript_entries(
    entries: &[TranscriptMirrorEntry],
    since: Option<&str>,
) -> Vec<TranscriptMirrorEntry> {
    entries
        .iter()
        .filter(|entry| since.is_none_or(|threshold| entry.timestamp.as_str() >= threshold))
        .cloned()
        .collect()
}

pub fn render_transcript_lines(entries: &[TranscriptMirrorEntry], width: u16) -> Vec<String> {
    if entries.is_empty() {
        return vec!["No mirror entries yet.".to_owned()];
    }

    let content_width = content_width(width);
    let label_width = [LABEL_TELEGRAM, LABEL_CLI, LABEL_CODEX]
        .into_iter()
        .map(display_width)
        .max()
        .unwrap_or_else(|| display_width(LABEL_TELEGRAM));
    let text_width = content_width
        .saturating_sub(label_width + display_width(BODY_SEPARATOR))
        .max(8);

    let mut lines = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(render_entry_lines(entry, label_width, text_width));
    }
    lines
}

fn label_for(entry: &TranscriptMirrorEntry) -> &'static str {
    match (&entry.origin, &entry.role) {
        (TranscriptMirrorOrigin::Telegram, TranscriptMirrorRole::User) => LABEL_TELEGRAM,
        (TranscriptMirrorOrigin::Cli, TranscriptMirrorRole::User) => LABEL_CLI,
        (_, TranscriptMirrorRole::Assistant) => LABEL_CODEX,
    }
}

fn render_entry_lines(
    entry: &TranscriptMirrorEntry,
    label_width: usize,
    text_width: usize,
) -> Vec<String> {
    let label = pad_display(label_for(entry), label_width);
    let continuation = format!("{}{}", " ".repeat(label_width), BODY_SEPARATOR);
    let wrapped = wrap_multiline_text(&entry.text, text_width);

    let mut lines = Vec::new();
    for (index, line) in wrapped.iter().enumerate() {
        if index == 0 {
            lines.push(format!("{label}{BODY_SEPARATOR}{line}"));
        } else {
            lines.push(format!("{continuation}{line}"));
        }
    }

    if lines.is_empty() {
        lines.push(format!("{label}{BODY_SEPARATOR}"));
    }

    lines
}

fn wrap_multiline_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for (paragraph_index, paragraph) in text.split('\n').enumerate() {
        if paragraph_index > 0 {
            lines.push(String::new());
        }
        if paragraph.is_empty() {
            continue;
        }
        lines.extend(wrap_display_line(paragraph, width));
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn wrap_display_line(input: &str, width: usize) -> Vec<String> {
    if input.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in input.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width + ch_width > width {
            lines.push(current.trim_end().to_owned());
            current.clear();
            current_width = 0;
            if ch == ' ' {
                continue;
            }
        }
        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        lines.push(current.trim_end().to_owned());
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn pad_display(text: &str, width: usize) -> String {
    let pad = width.saturating_sub(display_width(text));
    format!("{text}{}", " ".repeat(pad))
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn content_width(total_width: u16) -> usize {
    usize::from(total_width).max(24)
}

#[cfg(test)]
mod tests {
    use super::{
        filter_transcript_entries, parse_transcript_mirror_jsonl, parse_transcript_mirror_line,
        render_transcript_lines,
    };
    use crate::repository::{
        TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin,
        TranscriptMirrorRole,
    };

    fn sample_entry(text: &str) -> TranscriptMirrorEntry {
        TranscriptMirrorEntry {
            timestamp: "2026-03-19T00:00:00.000Z".to_owned(),
            session_id: "session-1".to_owned(),
            origin: TranscriptMirrorOrigin::Telegram,
            role: TranscriptMirrorRole::User,
            delivery: TranscriptMirrorDelivery::Final,
            text: text.to_owned(),
        }
    }

    #[test]
    fn parses_jsonl_and_filters_since() {
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&sample_entry("first")).unwrap(),
            serde_json::to_string(&TranscriptMirrorEntry {
                timestamp: "2026-03-19T00:00:01.000Z".to_owned(),
                text: "second".to_owned(),
                ..sample_entry("unused")
            })
            .unwrap()
        );
        let entries = parse_transcript_mirror_jsonl(&content).unwrap();
        let filtered = filter_transcript_entries(&entries, Some("2026-03-19T00:00:01.000Z"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].text, "second");
    }

    #[test]
    fn parse_line_skips_blank_input() {
        assert!(parse_transcript_mirror_line("   ").unwrap().is_none());
    }

    #[test]
    fn continuation_lines_stay_under_body_column() {
        let rendered = render_transcript_lines(
            &[sample_entry(
                "這是一段很長很長的中文內容，用來確認 continuation line 會留在正文列，而不是跑回 Telegram 前綴那一列。",
            )],
            50,
        );

        assert!(rendered.iter().any(|line| line.contains("Telegram │")));
        assert!(rendered.iter().any(|line| line.contains("         │")));
    }
}
