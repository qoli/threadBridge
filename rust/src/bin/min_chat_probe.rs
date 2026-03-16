use anyhow::Result;
use serde::Serialize;
use threadbridge_rust::codex::{CodexRunner, CodexWorkspace};
use threadbridge_rust::config::load_runtime_config;
use threadbridge_rust::logging::init_json_logs;
use threadbridge_rust::workspace::{ensure_workspace_runtime, validate_seed_template};
use tokio::fs;
use uuid::Uuid;

const PROMPTS: [&str; 3] = [
    "笛卡尔指南讲的大概内容，帮我简短撰写一下和艺术相关的摘要和篇章给我",
    "卢梭的内容，也按照以上结果帮我解读和摘取一些摘要",
    "我們產生了多少條對話呢？",
];

#[derive(Serialize)]
struct TurnReport {
    index: usize,
    prompt: String,
    thread_id: String,
    thread_id_changed_from_previous: bool,
    final_response: String,
}

#[derive(Serialize)]
struct ProbeReport {
    probe_id: String,
    workspace_path: String,
    turns: Vec<TurnReport>,
    thread_id_stable_across_all_turns: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = load_runtime_config()?;
    let probe_id = format!(
        "min-chat-{}-{}",
        chrono_like_now(),
        &Uuid::new_v4().to_string()[..8]
    );
    let probe_root = runtime
        .data_root_path
        .join("min-chat-probes")
        .join(&probe_id);
    let workspace_path = probe_root.join("workspace");
    fs::create_dir_all(&workspace_path).await?;

    let _guard = init_json_logs(&probe_root.join("events.jsonl"))?;
    let template =
        validate_seed_template(&runtime.codex_working_directory.join("templates/AGENTS.md"))?;
    ensure_workspace_runtime(&runtime.codex_working_directory, &template, &workspace_path).await?;

    let runner = CodexRunner::new(runtime.codex_model.clone());
    let workspace = CodexWorkspace {
        working_directory: workspace_path.clone(),
    };

    let mut turns = Vec::new();
    let mut existing_thread_id: Option<String> = None;

    for (index, prompt) in PROMPTS.iter().enumerate() {
        let result = runner
            .run_prompt(&workspace, existing_thread_id.as_deref(), prompt)
            .await?;
        let thread_id_changed = existing_thread_id
            .as_deref()
            .is_some_and(|previous| previous != result.thread_id);
        existing_thread_id = Some(result.thread_id.clone());
        turns.push(TurnReport {
            index: index + 1,
            prompt: (*prompt).to_owned(),
            thread_id: result.thread_id,
            thread_id_changed_from_previous: thread_id_changed,
            final_response: result.final_response,
        });
    }

    let stable = turns
        .first()
        .map(|first| turns.iter().all(|turn| turn.thread_id == first.thread_id))
        .unwrap_or(true);

    let report = ProbeReport {
        probe_id,
        workspace_path: workspace_path.display().to_string(),
        turns,
        thread_id_stable_across_all_turns: stable,
    };

    let report_path = probe_root.join("report.json");
    fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);

    Ok(())
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
}
