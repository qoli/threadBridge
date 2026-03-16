use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, error, info};

const WORKSPACE_READY_PROMPT: &str = "You are initializing a Telegram thread workspace. Read the current workspace AGENTS.md and reply with exactly READY. Do not ask follow-up questions. Do not run tools.";
const WORKSPACE_RECONNECT_PROMPT: &str = "You are reconnecting an existing Telegram thread workspace session. Read the current workspace AGENTS.md and reply with exactly READY. Do not ask follow-up questions. Do not run tools.";

#[derive(Debug, Clone, Serialize)]
pub struct CodexWorkspace {
    pub working_directory: PathBuf,
}

#[derive(Debug, Clone)]
pub enum CodexInputItem {
    Text { text: String },
    LocalImage { path: String },
}

#[derive(Debug, Clone)]
pub struct CodexRunner {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexThreadEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted,
    #[serde(rename = "turn.completed")]
    TurnCompleted { usage: Option<serde_json::Value> },
    #[serde(rename = "turn.failed")]
    TurnFailed { error: serde_json::Value },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "item.started")]
    ItemStarted { item: serde_json::Value },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: serde_json::Value },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: serde_json::Value },
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexRunResult {
    pub final_response: String,
    pub selected_factory: String,
    pub thread_id: String,
    pub thread_id_changed: bool,
}

fn log_item_event(lifecycle: &str, item: &serde_json::Value) {
    let Some(item_type) = item.get("type").and_then(|value| value.as_str()) else {
        return;
    };

    match item_type {
        "command_execution" | "mcp_tool_call" | "web_search" => {
            info!(
                event = "codex.item",
                lifecycle,
                item_type,
                item = %item,
                "codex tool activity"
            );
        }
        _ => {}
    }
}

impl CodexRunner {
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    fn build_fresh_args(
        &self,
        workspace: &Path,
        prompt: &str,
        image_paths: &[String],
    ) -> Vec<String> {
        let mut args = vec![
            "exec".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--full-auto".to_owned(),
            "--cd".to_owned(),
            workspace.display().to_string(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        for image_path in image_paths {
            args.push("--image".to_owned());
            args.push(image_path.clone());
        }
        args.push("--".to_owned());
        args.push(prompt.to_owned());
        args
    }

    fn build_resume_args(
        &self,
        thread_id: &str,
        prompt: &str,
        image_paths: &[String],
    ) -> Vec<String> {
        let mut args = vec![
            "exec".to_owned(),
            "resume".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--full-auto".to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        for image_path in image_paths {
            args.push("--image".to_owned());
            args.push(image_path.clone());
        }
        args.push(thread_id.to_owned());
        args.push("--".to_owned());
        args.push(prompt.to_owned());
        args
    }

    fn normalize_input(input: &[CodexInputItem]) -> (String, Vec<String>) {
        let mut prompt_parts = Vec::new();
        let mut image_paths = Vec::new();
        for item in input {
            match item {
                CodexInputItem::Text { text } => prompt_parts.push(text.clone()),
                CodexInputItem::LocalImage { path } => image_paths.push(path.clone()),
            }
        }
        (prompt_parts.join("\n\n"), image_paths)
    }

    pub async fn run_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        input: Vec<CodexInputItem>,
        mut on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let (prompt, image_paths) = Self::normalize_input(&input);
        let selected_factory = if existing_thread_id.is_some() {
            "resumeThread"
        } else {
            "startThread"
        };
        let args = match existing_thread_id {
            Some(thread_id) => self.build_resume_args(thread_id, &prompt, &image_paths),
            None => self.build_fresh_args(&workspace.working_directory, &prompt, &image_paths),
        };

        info!(
            event = "codex.cli.spawn",
            selected_factory,
            cwd = %workspace.working_directory.display(),
            existing_thread_id = existing_thread_id.unwrap_or(""),
            command_args = ?args,
            "spawning codex cli"
        );

        let mut child = Command::new("codex")
            .args(&args)
            .current_dir(&workspace.working_directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn codex cli")?;

        let stdout = child.stdout.take().context("missing codex stdout")?;
        let stderr = child.stderr.take().context("missing codex stderr")?;
        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut latest_thread_id: Option<String> = None;
        let mut final_response = String::new();
        let mut stderr_lines = Vec::new();

        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line? {
                        Some(line) => {
                            debug!(event = "codex.cli.stdout.line", line = %line);
                            if let Ok(event) = serde_json::from_str::<CodexThreadEvent>(&line) {
                                if let CodexThreadEvent::ThreadStarted { thread_id } = &event {
                                    latest_thread_id = Some(thread_id.clone());
                                }
                                match &event {
                                    CodexThreadEvent::ItemStarted { item } => log_item_event("started", item),
                                    CodexThreadEvent::ItemCompleted { item } => log_item_event("completed", item),
                                    _ => {}
                                }
                                if let CodexThreadEvent::ItemCompleted { item } = &event {
                                    if item.get("type").and_then(|v| v.as_str()) == Some("agent_message") {
                                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                            final_response = text.to_owned();
                                        }
                                    }
                                }
                                on_event(event).await;
                            }
                        }
                        None => break,
                    }
                }
                line = stderr_reader.next_line() => {
                    match line? {
                        Some(line) => {
                            debug!(event = "codex.cli.stderr.line", line = %line);
                            stderr_lines.push(line);
                        }
                        None => {}
                    }
                }
            }
        }

        let status = child
            .wait()
            .await
            .context("failed waiting for codex process")?;
        if !status.success() {
            error!(event = "codex.cli.exit", status = ?status, stderr = ?stderr_lines);
            bail!(
                "Codex CLI exited unsuccessfully: {}",
                stderr_lines.join("\n")
            );
        }

        let resolved = latest_thread_id.context("codex did not emit thread.started")?;
        Ok(CodexRunResult {
            final_response,
            selected_factory: selected_factory.to_owned(),
            thread_id_changed: existing_thread_id.is_some_and(|id| id != resolved),
            thread_id: resolved,
        })
    }

    pub async fn run(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        input: Vec<CodexInputItem>,
    ) -> Result<CodexRunResult> {
        self.run_with_events(workspace, existing_thread_id, input, |_| async {})
            .await
    }

    pub async fn run_locked(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        input: Vec<CodexInputItem>,
    ) -> Result<CodexRunResult> {
        let result = self.run(workspace, Some(locked_thread_id), input).await?;
        self.ensure_locked_thread_id(locked_thread_id, result)
    }

    pub async fn run_locked_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        input: Vec<CodexInputItem>,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let result = self
            .run_with_events(workspace, Some(locked_thread_id), input, on_event)
            .await?;
        self.ensure_locked_thread_id(locked_thread_id, result)
    }

    pub async fn run_prompt(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        prompt: &str,
    ) -> Result<CodexRunResult> {
        self.run(
            workspace,
            existing_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
        )
        .await
    }

    pub async fn run_locked_prompt(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        prompt: &str,
    ) -> Result<CodexRunResult> {
        self.run_locked(
            workspace,
            locked_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
        )
        .await
    }

    pub async fn run_prompt_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        prompt: &str,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_with_events(
            workspace,
            existing_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
            on_event,
        )
        .await
    }

    pub async fn run_locked_prompt_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        prompt: &str,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_locked_with_events(
            workspace,
            locked_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
            on_event,
        )
        .await
    }

    fn ensure_locked_thread_id(
        &self,
        locked_thread_id: &str,
        result: CodexRunResult,
    ) -> Result<CodexRunResult> {
        if result.thread_id != locked_thread_id || result.thread_id_changed {
            bail!(
                "Codex session continuity changed: expected thread {}, got {}",
                locked_thread_id,
                result.thread_id
            );
        }
        Ok(result)
    }

    fn ensure_ready_response(
        &self,
        result: CodexRunResult,
        context: &str,
    ) -> Result<CodexRunResult> {
        if result.final_response.trim() != "READY" {
            bail!(
                "{} did not return READY: {}",
                context,
                result.final_response.trim()
            );
        }
        Ok(result)
    }

    pub async fn initialize_workspace_session(
        &self,
        workspace: &CodexWorkspace,
    ) -> Result<CodexRunResult> {
        let result = self
            .run_prompt(workspace, None, WORKSPACE_READY_PROMPT)
            .await?;
        self.ensure_ready_response(result, "workspace initialization")
    }

    pub async fn generate_thread_title_from_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Generate a concise Telegram thread title for our conversation so far.",
            "Rules:",
            "- Return only the title.",
            "- Use the same language as the conversation when possible.",
            "- Keep it under 48 characters.",
            "- No quotes, no markdown, no emojis unless the conversation clearly needs one.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn summarize_thread_from_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Summarize our conversation so far.",
            "Rules:",
            "- Focus on user goals, key decisions, unfinished work, and important facts.",
            "- Keep the summary concise but useful.",
            "- Use the same language as the conversation when possible.",
            "- Return plain text only.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn reconnect_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let result = self
            .run_locked_prompt(workspace, existing_thread_id, WORKSPACE_RECONNECT_PROMPT)
            .await?;
        self.ensure_ready_response(result, "workspace reconnect")
    }

    pub async fn build_prompt_config(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Use this workspace runtime to build the next prompt config artifacts.",
            "Read and follow the current workspace AGENTS.md.",
            "Use the local wrapper command ./bin/build_prompt_config for any file materialization work.",
            "Base all semantic decisions on the current Codex session context.",
            "If the session still lacks enough information, ask follow-up questions in this thread and do not run the tool.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn generate_image(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Use this workspace runtime to generate images for the current thread.",
            "Read and follow the current workspace AGENTS.md.",
            "Use the local wrapper command ./bin/generate_image for API execution and output materialization.",
            "By default, use the latest prompt config unless the current session clearly requires a different one.",
            "If the workspace is missing a usable prompt config or other required inputs, ask follow-up questions in this thread and do not run the tool.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn update_agents_md(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
        target_path: &Path,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Based on our Telegram thread so far, update the child AGENTS.md for this exact thread workspace.".to_owned(),
            format!(
                "Read the existing AGENTS.md at {} first if it already exists, then preserve any still-valid stable rules while rewriting it from the latest session context.",
                target_path.display()
            ),
            format!("Write or rewrite the thread-local AGENTS.md at: {}", target_path.display()),
            "Preserve the workspace runtime contract section and the wrapper commands ./bin/build_prompt_config and ./bin/generate_image.".to_owned(),
            "Keep the embedded build_prompt_config prompt guide inside the workspace runtime contract so future turns stay fully workspace-local.".to_owned(),
            "This file should capture stable thread-specific instructions for future turns, not a transcript recap.".to_owned(),
            "Write the Markdown file directly in the workspace.".to_owned(),
            "If the current session still lacks enough stable information, ask the user follow-up questions in this thread and do not write or modify the target file.".to_owned(),
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn generate_restore_recap_from_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Write a concise restore recap for this Telegram thread.",
            "Rules:",
            "- Base the recap on our session so far.",
            "- Focus on what we already explored, key decisions, existing artifacts, and the most useful next step.",
            "- Write for the human user who is reopening the thread after archiving it.",
            "- Keep it plain text.",
            "- Do not ask follow-up questions.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{CodexInputItem, CodexRunResult, CodexRunner};
    use std::path::Path;

    fn run_result(thread_id: &str, changed: bool) -> CodexRunResult {
        CodexRunResult {
            final_response: "ok".to_owned(),
            selected_factory: "resumeThread".to_owned(),
            thread_id: thread_id.to_owned(),
            thread_id_changed: changed,
        }
    }

    #[test]
    fn fresh_args_include_cd_and_full_auto() {
        let runner = CodexRunner::new(Some("gpt-test".to_owned()));
        let args = runner.build_fresh_args(Path::new("/tmp/workspace"), "hello", &[]);
        assert_eq!(
            args,
            vec![
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--full-auto",
                "--cd",
                "/tmp/workspace",
                "--model",
                "gpt-test",
                "--",
                "hello",
            ]
        );
    }

    #[test]
    fn resume_args_include_full_auto_thread_id_and_images() {
        let runner = CodexRunner::new(None);
        let args = runner.build_resume_args(
            "thread-123",
            "prompt",
            &["/a.png".to_owned(), "/b.png".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "resume",
                "--json",
                "--skip-git-repo-check",
                "--full-auto",
                "--image",
                "/a.png",
                "--image",
                "/b.png",
                "thread-123",
                "--",
                "prompt",
            ]
        );
    }

    #[test]
    fn fresh_args_insert_double_dash_before_dash_prefixed_prompt() {
        let runner = CodexRunner::new(None);
        let args = runner.build_fresh_args(Path::new("/tmp/workspace"), "- explain this", &[]);
        assert_eq!(args.last().map(String::as_str), Some("- explain this"));
        assert_eq!(args.get(args.len() - 2).map(String::as_str), Some("--"));
    }

    #[test]
    fn resume_args_insert_double_dash_before_dash_prefixed_prompt() {
        let runner = CodexRunner::new(None);
        let args = runner.build_resume_args("thread-123", "- explain this", &[]);
        assert_eq!(args.last().map(String::as_str), Some("- explain this"));
        assert_eq!(args.get(args.len() - 2).map(String::as_str), Some("--"));
    }

    #[test]
    fn normalize_input_splits_prompt_and_images() {
        let (prompt, image_paths) = CodexRunner::normalize_input(&[
            CodexInputItem::Text {
                text: "one".to_owned(),
            },
            CodexInputItem::LocalImage {
                path: "/tmp/1.png".to_owned(),
            },
            CodexInputItem::Text {
                text: "two".to_owned(),
            },
        ]);
        assert_eq!(prompt, "one\n\ntwo");
        assert_eq!(image_paths, vec!["/tmp/1.png".to_owned()]);
    }

    #[test]
    fn locked_thread_id_accepts_matching_resume_result() {
        let runner = CodexRunner::new(None);
        let result = runner.ensure_locked_thread_id("thread-123", run_result("thread-123", false));
        assert!(result.is_ok());
    }

    #[test]
    fn locked_thread_id_rejects_thread_drift() {
        let runner = CodexRunner::new(None);
        let result = runner.ensure_locked_thread_id("thread-123", run_result("thread-999", true));
        assert!(result.is_err());
    }
}
