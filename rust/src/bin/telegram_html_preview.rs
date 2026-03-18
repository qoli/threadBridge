use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use threadbridge_rust::config::load_runtime_config;
use threadbridge_rust::telegram_runtime::final_reply::{
    INLINE_MESSAGE_CHAR_LIMIT, TelegramReplyPlan, plan_final_assistant_reply,
};

const OUTPUT_PATH: &str = "docs/telegram-html-preview.html";
const MAX_REAL_SAMPLES: usize = 8;
const SYNTHETIC_CODE_BLOCK: &str = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
const SYNTHETIC_BLOCKQUOTE: &str = "> nested\n> quote\n>\n> - list item\n> - another item";

#[derive(Debug, Deserialize)]
struct ConversationEntry {
    timestamp: String,
    direction: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ThreadMetadata {
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct Sample {
    title: String,
    source_label: String,
    source_detail: String,
    timestamp: String,
    raw_text: String,
    tags: Vec<String>,
    plan: TelegramReplyPlan,
    synthetic: bool,
}

fn main() -> Result<()> {
    let runtime = load_runtime_config()?;
    let samples = collect_samples(&runtime.data_root_path)?;
    let html = render_page(&samples);
    let output_path = runtime.codex_working_directory.join(OUTPUT_PATH);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&output_path, html).with_context(|| format!("failed to write {}", output_path.display()))?;
    println!("Wrote {}", output_path.display());
    Ok(())
}

fn collect_samples(data_root: &Path) -> Result<Vec<Sample>> {
    let titles = read_thread_titles(data_root)?;
    let mut candidates = Vec::new();
    for entry in fs::read_dir(data_root).with_context(|| format!("failed to read {}", data_root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().to_string();
        let conversation_path = path.join("conversations.jsonl");
        if !conversation_path.exists() {
            continue;
        }
        let text = fs::read_to_string(&conversation_path)
            .with_context(|| format!("failed to read {}", conversation_path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            let item: ConversationEntry = serde_json::from_str(line)
                .with_context(|| format!("invalid json in {}:{}", conversation_path.display(), line_no + 1))?;
            if item.direction != "assistant" {
                continue;
            }
            let raw_text = item.text.trim().to_owned();
            if raw_text.is_empty() {
                continue;
            }
            let tags = classify_tags(&raw_text);
            let thread_title = titles
                .get(&folder)
                .and_then(|title| title.clone())
                .unwrap_or_else(|| format!("thread {}", &folder[..folder.len().min(8)]));
            candidates.push(Sample {
                title: sample_title(&thread_title, &tags, candidates.len() + 1),
                source_label: thread_title,
                source_detail: format!("{folder}/conversations.jsonl:{}", line_no + 1),
                timestamp: item.timestamp,
                raw_text: raw_text.clone(),
                tags,
                plan: plan_final_assistant_reply(&raw_text, INLINE_MESSAGE_CHAR_LIMIT),
                synthetic: false,
            });
        }
    }

    let real_samples = select_real_samples(candidates);
    let mut all_samples = real_samples.clone();
    let existing_tags: HashSet<String> = all_samples
        .iter()
        .flat_map(|sample| sample.tags.iter().cloned())
        .collect();
    if !existing_tags.contains("code-block") {
        all_samples.push(make_synthetic_sample(
            "Synthetic fenced code block",
            "supplemental coverage",
            SYNTHETIC_CODE_BLOCK,
            vec!["synthetic", "code-block"],
        ));
    }
    if !existing_tags.contains("blockquote") {
        all_samples.push(make_synthetic_sample(
            "Synthetic blockquote fallback",
            "supplemental coverage",
            SYNTHETIC_BLOCKQUOTE,
            vec!["synthetic", "blockquote", "fallback"],
        ));
    }
    if !all_samples
        .iter()
        .any(|sample| matches!(sample.plan, TelegramReplyPlan::MarkdownAttachment { .. }))
    {
        let overflow = format!(
            "## Overflow sample\n\n{}\n",
            "This sample exists only to hit MarkdownAttachment mode. ".repeat(120)
        );
        all_samples.push(make_synthetic_sample(
            "Synthetic overflow attachment",
            "supplemental coverage",
            &overflow,
            vec!["synthetic", "overflow", "attachment"],
        ));
    }

    Ok(all_samples)
}

fn read_thread_titles(data_root: &Path) -> Result<BTreeMap<String, Option<String>>> {
    let mut titles = BTreeMap::new();
    for entry in fs::read_dir(data_root).with_context(|| format!("failed to read {}", data_root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let metadata_path = path.join("metadata.json");
        if !metadata_path.exists() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let text = fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?;
        let metadata: ThreadMetadata = serde_json::from_str(&text)
            .with_context(|| format!("invalid json in {}", metadata_path.display()))?;
        titles.insert(file_name, metadata.title);
    }
    Ok(titles)
}

fn classify_tags(text: &str) -> Vec<String> {
    let mut tags = BTreeSet::new();
    if text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
                && trimmed.contains(". ")
    }) {
        tags.insert("lists");
    }
    if text.contains("```") {
        tags.insert("code-block");
    }
    if text.contains('`') {
        tags.insert("inline-code");
    }
    if text.contains("](") || text.contains("https://") || text.contains("http://") {
        tags.insert("links");
    }
    if text.contains("/Volumes/") {
        tags.insert("paths");
    }
    if text.lines().any(|line| line.trim_start().starts_with('>')) {
        tags.insert("blockquote");
    }
    let char_count = text.chars().count();
    if char_count > 900 {
        tags.insert("long");
    }
    if text.lines().count() > 12 {
        tags.insert("dense");
    }
    if tags.is_empty() {
        tags.insert("plain");
    }
    tags.into_iter().map(str::to_owned).collect()
}

fn sample_title(thread_title: &str, tags: &[String], ordinal: usize) -> String {
    let primary = tags.first().cloned().unwrap_or_else(|| "plain".to_owned());
    format!("{thread_title} · sample {ordinal} · {primary}")
}

fn select_real_samples(mut candidates: Vec<Sample>) -> Vec<Sample> {
    let mut chosen = Vec::new();

    if let Some(simple) = candidates
        .iter()
        .filter(|sample| sample.raw_text.chars().count() <= 80)
        .min_by_key(|sample| sample.raw_text.chars().count())
        .cloned()
    {
        chosen.push(simple.clone());
        candidates.retain(|sample| sample.source_detail != simple.source_detail);
    }

    let mut covered: HashSet<String> = HashSet::new();
    while !candidates.is_empty() && chosen.len() < MAX_REAL_SAMPLES {
        let mut best_index = None;
        let mut best_score = isize::MIN;
        for (idx, sample) in candidates.iter().enumerate() {
            let uncovered = sample
                .tags
                .iter()
                .filter(|tag| !covered.contains(*tag))
                .count() as isize;
            let folder_repeat_penalty = if chosen
                .iter()
                .filter(|item| item.source_label == sample.source_label)
                .count()
                >= 2
            {
                5
            } else {
                0
            };
            let length_bonus = (sample.raw_text.chars().count().min(1200) / 200) as isize;
            let score = uncovered * 20 + length_bonus - folder_repeat_penalty;
            if score > best_score {
                best_score = score;
                best_index = Some(idx);
            }
        }
        let Some(best_index) = best_index else { break };
        let sample = candidates.remove(best_index);
        for tag in &sample.tags {
            covered.insert(tag.clone());
        }
        chosen.push(sample);
    }

    chosen
}

fn make_synthetic_sample(title: &str, source_label: &str, raw_text: &str, tags: Vec<&str>) -> Sample {
    Sample {
        title: title.to_owned(),
        source_label: source_label.to_owned(),
        source_detail: "synthetic".to_owned(),
        timestamp: "synthetic".to_owned(),
        raw_text: raw_text.to_owned(),
        tags: tags.into_iter().map(str::to_owned).collect(),
        plan: plan_final_assistant_reply(raw_text, INLINE_MESSAGE_CHAR_LIMIT),
        synthetic: true,
    }
}

fn render_page(samples: &[Sample]) -> String {
    let real_count = samples.iter().filter(|sample| !sample.synthetic).count();
    let synthetic_count = samples.len().saturating_sub(real_count);
    let cards = samples.iter().map(render_sample_card).collect::<Vec<_>>().join("\n");
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-Hant">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Telegram HTML Preview</title>
  <style>
    :root {{
      --bg: #eef3f7;
      --panel: #ffffff;
      --panel-muted: #f7f9fb;
      --border: #d7e1ea;
      --text: #173042;
      --muted: #60778a;
      --accent: #2a8ed9;
      --bubble: #ffffff;
      --bubble-out: #dff4ff;
      --code-bg: #0f2231;
      --code-text: #dbeaf4;
      --shadow: 0 18px 40px rgba(21, 53, 79, 0.08);
      --radius: 18px;
      --mono: ui-monospace, "SFMono-Regular", Menlo, Monaco, Consolas, monospace;
      --sans: "SF Pro Text", "Segoe UI", "Helvetica Neue", Helvetica, Arial, sans-serif;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: var(--sans);
      color: var(--text);
      background:
        radial-gradient(circle at top left, rgba(42, 142, 217, 0.08), transparent 32%),
        linear-gradient(180deg, #f7fbff 0%, var(--bg) 100%);
    }}
    .page {{
      width: min(1400px, calc(100vw - 32px));
      margin: 24px auto 80px;
    }}
    .hero {{
      background: rgba(255,255,255,0.88);
      backdrop-filter: blur(14px);
      border: 1px solid rgba(215, 225, 234, 0.9);
      border-radius: 24px;
      box-shadow: var(--shadow);
      padding: 28px;
      margin-bottom: 24px;
    }}
    .hero h1 {{
      margin: 0 0 10px;
      font-size: clamp(28px, 4vw, 42px);
      line-height: 1.05;
      letter-spacing: -0.03em;
    }}
    .hero p {{
      margin: 0 0 14px;
      max-width: 980px;
      color: var(--muted);
      font-size: 15px;
      line-height: 1.65;
    }}
    .summary {{
      display: flex;
      flex-wrap: wrap;
      gap: 10px;
      margin-top: 18px;
    }}
    .summary span {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      border: 1px solid var(--border);
      background: var(--panel-muted);
      border-radius: 999px;
      padding: 8px 12px;
      font-size: 12px;
      color: var(--muted);
    }}
    .sample {{
      background: rgba(255,255,255,0.92);
      border: 1px solid rgba(215, 225, 234, 0.92);
      border-radius: 24px;
      box-shadow: var(--shadow);
      padding: 20px;
      margin-bottom: 20px;
    }}
    .sample-header {{
      display: flex;
      flex-wrap: wrap;
      align-items: baseline;
      justify-content: space-between;
      gap: 12px;
      margin-bottom: 16px;
    }}
    .sample h2 {{
      margin: 0;
      font-size: 22px;
      letter-spacing: -0.02em;
    }}
    .meta {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      font-size: 12px;
      color: var(--muted);
      margin-top: 10px;
    }}
    .pill {{
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 6px 10px;
      border-radius: 999px;
      background: var(--panel-muted);
      border: 1px solid var(--border);
    }}
    .pill.mode-html {{ background: #eaf6ff; color: #0f5a92; border-color: #b8ddf8; }}
    .pill.mode-plain {{ background: #fff4de; color: #8f5c00; border-color: #ead1a0; }}
    .pill.mode-attachment {{ background: #f3ebff; color: #6a3ab2; border-color: #d8c4ff; }}
    .pill.synthetic {{ background: #fff1ec; color: #9a4a22; border-color: #f0c6b4; }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 16px;
    }}
    .panel {{
      min-width: 0;
      border: 1px solid var(--border);
      background: var(--panel);
      border-radius: 18px;
      overflow: hidden;
    }}
    .panel-head {{
      padding: 12px 14px;
      border-bottom: 1px solid var(--border);
      background: var(--panel-muted);
      font-size: 12px;
      font-weight: 700;
      letter-spacing: 0.04em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .panel-body {{
      padding: 14px;
    }}
    pre {{
      margin: 0;
      white-space: pre-wrap;
      word-break: break-word;
      font: 12px/1.58 var(--mono);
    }}
    .code-frame {{
      background: var(--code-bg);
      color: var(--code-text);
    }}
    .tg-shell {{
      background: linear-gradient(180deg, #dfeefa 0%, #edf6ff 100%);
      min-height: 100%;
      padding: 22px;
    }}
    .tg-chat {{
      max-width: 420px;
      margin-left: auto;
      display: flex;
      flex-direction: column;
      gap: 10px;
    }}
    .tg-bubble {{
      align-self: flex-end;
      background: var(--bubble-out);
      border-radius: 20px 20px 6px 20px;
      padding: 14px 16px;
      box-shadow: 0 10px 24px rgba(37, 92, 132, 0.08);
      color: #143043;
      line-height: 1.55;
      font-size: 15px;
      white-space: pre-wrap;
      word-break: break-word;
    }}
    .tg-bubble pre {{
      margin-top: 10px;
      padding: 12px;
      background: rgba(10, 24, 35, 0.92);
      color: #e8f4fb;
      border-radius: 12px;
      overflow-x: auto;
      white-space: pre-wrap;
    }}
    .tg-bubble code {{
      font-family: var(--mono);
      font-size: 0.92em;
      background: rgba(18, 42, 60, 0.08);
      border-radius: 6px;
      padding: 1px 5px;
    }}
    .tg-bubble a {{
      color: #1167a8;
      text-decoration: none;
      font-weight: 600;
    }}
    .tg-bubble p:first-child {{ margin-top: 0; }}
    .tg-bubble p:last-child {{ margin-bottom: 0; }}
    .attachment {{
      margin-top: 10px;
      background: rgba(255,255,255,0.75);
      border: 1px solid rgba(17, 103, 168, 0.15);
      border-radius: 14px;
      padding: 12px 14px;
      display: flex;
      flex-direction: column;
      gap: 4px;
    }}
    .attachment strong {{ font-size: 14px; }}
    .attachment small {{ color: var(--muted); }}
    .note {{
      margin-top: 8px;
      color: var(--muted);
      font-size: 12px;
    }}
    @media (max-width: 1100px) {{
      .grid {{ grid-template-columns: 1fr; }}
      .tg-chat {{ max-width: none; }}
    }}
  </style>
</head>
<body>
  <main class="page">
    <section class="hero">
      <h1>Telegram HTML Preview</h1>
      <p>
        This page uses the current Rust reply planner and renderer to compare real assistant outputs
        from <code>data/*/conversations.jsonl</code> against the exact Telegram HTML they produce now.
        The right-most panel is a browser approximation of Telegram presentation, not an authoritative client rendering.
      </p>
      <div class="summary">
        <span>{real_count} real assistant samples</span>
        <span>{synthetic_count} synthetic coverage samples</span>
        <span>compare raw text, generated HTML, and Telegram-style browser rendering</span>
        <span>attachment and fallback branches are labeled explicitly</span>
      </div>
    </section>
    {cards}
  </main>
</body>
</html>
"#
    )
}

fn render_sample_card(sample: &Sample) -> String {
    let mode = match &sample.plan {
        TelegramReplyPlan::InlineHtml { .. } => "InlineHtml",
        TelegramReplyPlan::InlinePlainText { .. } => "InlinePlainText",
        TelegramReplyPlan::MarkdownAttachment { .. } => "MarkdownAttachment",
    };
    let mode_class = match &sample.plan {
        TelegramReplyPlan::InlineHtml { .. } => "mode-html",
        TelegramReplyPlan::InlinePlainText { .. } => "mode-plain",
        TelegramReplyPlan::MarkdownAttachment { .. } => "mode-attachment",
    };
    let tags = sample
        .tags
        .iter()
        .map(|tag| format!(r#"<span class="pill">{}</span>"#, escape_html(tag)))
        .collect::<Vec<_>>()
        .join(" ");
    let synthetic_badge = if sample.synthetic {
        r#"<span class="pill synthetic">synthetic coverage</span>"#
    } else {
        ""
    };
    format!(
        r#"<section class="sample">
  <div class="sample-header">
    <div>
      <h2>{title}</h2>
      <div class="meta">
        <span class="pill {mode_class}">{mode}</span>
        {synthetic_badge}
        <span class="pill">{source_label}</span>
        <span class="pill">{source_detail}</span>
        <span class="pill">{timestamp}</span>
        {tags}
      </div>
    </div>
  </div>
  <div class="grid">
    <article class="panel">
      <div class="panel-head">Raw Markdown / Text</div>
      <div class="panel-body code-frame"><pre>{raw}</pre></div>
    </article>
    <article class="panel">
      <div class="panel-head">Generated Telegram HTML / Mode</div>
      <div class="panel-body code-frame"><pre>{generated}</pre></div>
    </article>
    <article class="panel">
      <div class="panel-head">Telegram-style Browser Preview</div>
      <div class="panel-body tg-shell">
        <div class="tg-chat">
          {preview}
        </div>
      </div>
    </article>
  </div>
</section>"#,
        title = escape_html(&sample.title),
        mode_class = mode_class,
        mode = escape_html(mode),
        synthetic_badge = synthetic_badge,
        source_label = escape_html(&sample.source_label),
        source_detail = escape_html(&sample.source_detail),
        timestamp = escape_html(&sample.timestamp),
        tags = tags,
        raw = escape_html(&sample.raw_text),
        generated = generated_panel_text(&sample.plan),
        preview = preview_panel(&sample.plan),
    )
}

fn generated_panel_text(plan: &TelegramReplyPlan) -> String {
    let text = match plan {
        TelegramReplyPlan::InlineHtml { text } => text.clone(),
        TelegramReplyPlan::InlinePlainText { text, reason } => {
            format!("mode: InlinePlainText\nreason: {reason}\n\n{text}")
        }
        TelegramReplyPlan::MarkdownAttachment {
            notice_text,
            markdown,
        } => format!(
            "mode: MarkdownAttachment\nnotice_text:\n{notice_text}\n\nattachment: reply.md\nmarkdown_bytes: {}\n\n{}",
            markdown.len(),
            truncate_for_panel(markdown, 1200)
        ),
    };
    escape_html(&text)
}

fn preview_panel(plan: &TelegramReplyPlan) -> String {
    match plan {
        TelegramReplyPlan::InlineHtml { text } => format!(r#"<div class="tg-bubble">{text}</div>"#),
        TelegramReplyPlan::InlinePlainText { text, reason } => format!(
            r#"<div class="tg-bubble"><pre>{}</pre><div class="note">plain text mode: {}</div></div>"#,
            escape_html(text),
            escape_html(reason)
        ),
        TelegramReplyPlan::MarkdownAttachment {
            notice_text,
            markdown,
        } => format!(
            r#"<div class="tg-bubble">
  <div>{}</div>
  <div class="attachment">
    <strong>reply.md</strong>
    <small>Telegram document attachment preview</small>
  </div>
  <div class="note">attachment payload preview: {} chars of raw markdown</div>
</div>"#,
            escape_html(notice_text),
            markdown.chars().count()
        ),
    }
}

fn truncate_for_panel(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
