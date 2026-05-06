use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use tokio::fs;

use crate::execution_mode::ensure_workspace_execution_config;
use crate::telemetry::{RuntimeTelemetryFields, RuntimeTelemetryHandle, RuntimeTelemetryMetrics};
use crate::workspace_status::ensure_workspace_status_surface;

pub const THREADBRIDGE_RUNTIME_DIR: &str = ".threadbridge";
pub const THREADBRIDGE_RUNTIME_START: &str = "<!-- threadbridge:runtime:start -->";
pub const THREADBRIDGE_RUNTIME_END: &str = "<!-- threadbridge:runtime:end -->";
pub const THREADBRIDGE_RUNTIME_SKILL_DIR: &str = "skills/threadbridge-runtime";
const CODEX_REPO_RUNTIME_SKILL_DIR: &str = ".codex/skills/threadbridge-runtime";
const CODEX_REPO_RUNTIME_SKILL_TARGET: &str = "../../.threadbridge/skills/threadbridge-runtime";
const MANAGED_CODEX_CACHE_BINARY: &str = ".threadbridge/codex/codex";
const MANAGED_CODEX_SOURCE_FILE: &str = ".threadbridge/codex/source.txt";
const THREADBRIDGE_RUNTIME_SKILL_FILE: &str = "SKILL.md";
const THREADBRIDGE_RUNTIME_SKILL_REFERENCES_DIR: &str = "references";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexSourcePreference {
    Brew,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceRuntimeEnsureMode {
    ExplicitSync,
    PassiveReconcile,
}

#[derive(Default)]
struct WorkspaceEnsureStats {
    files_written: u64,
    files_skipped: u64,
    mode_changes: u64,
    mode_unchanged: u64,
    managed_codex_copied: u64,
}

impl WorkspaceEnsureStats {
    fn record_write(&mut self, changed: bool) {
        if changed {
            self.files_written += 1;
        } else {
            self.files_skipped += 1;
        }
    }

    fn record_mode_change(&mut self, changed: bool) {
        if changed {
            self.mode_changes += 1;
        } else {
            self.mode_unchanged += 1;
        }
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn build_wrapper_script(
    tool_file_name: &str,
    runtime_support_root: &Path,
    config_env_path: &Path,
) -> String {
    let quoted_runtime_support_root =
        shell_single_quote(&runtime_support_root.display().to_string());
    let quoted_config_env_path = shell_single_quote(&config_env_path.display().to_string());
    [
        "#!/bin/sh",
        "set -eu",
        "SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\"",
        "RUNTIME_DIR=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"",
        "WORKSPACE_DIR=\"$(CDPATH= cd -- \"$RUNTIME_DIR/..\" && pwd)\"",
        &format!("THREADBRIDGE_RUNTIME_SUPPORT_ROOT={quoted_runtime_support_root}"),
        &format!("THREADBRIDGE_CONFIG_ENV={quoted_config_env_path}"),
        "cd \"$WORKSPACE_DIR\"",
        &format!(
            "exec python3 \"$THREADBRIDGE_RUNTIME_SUPPORT_ROOT/tools/{tool_file_name}\" --config-env \"$THREADBRIDGE_CONFIG_ENV\" \"$@\""
        ),
        "",
    ]
    .join("\n")
}

async fn read_codex_source_preference(data_root: &Path) -> Result<CodexSourcePreference> {
    let source_path = data_root.join(MANAGED_CODEX_SOURCE_FILE);
    let value = match fs::read_to_string(&source_path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodexSourcePreference::Brew);
        }
        Err(error) => {
            return Err(anyhow!(
                "failed to read Codex source preference {}: {}",
                source_path.display(),
                error
            ));
        }
    };
    match value.trim() {
        "alpha" | "source" => Ok(CodexSourcePreference::Source),
        "brew" | "" => Ok(CodexSourcePreference::Brew),
        other => Err(anyhow!(
            "unsupported Codex source preference in {}: {}",
            source_path.display(),
            other
        )),
    }
}

fn build_hcodex_launcher_script(
    workspace_path: &Path,
    data_root: &Path,
    threadbridge_executable: &Path,
    codex_source_preference: CodexSourcePreference,
) -> String {
    let workspace = shell_single_quote(&workspace_path.display().to_string());
    let quoted_data_root = shell_single_quote(&data_root.display().to_string());
    let threadbridge_executable =
        shell_single_quote(&threadbridge_executable.display().to_string());
    let managed_codex_path = match codex_source_preference {
        CodexSourcePreference::Brew => workspace_path.join(".threadbridge/bin/codex"),
        // Source-built Codex works reliably from the repo-managed cache, but some
        // workspace-local copies fail after installation. Use the cache directly.
        CodexSourcePreference::Source => data_root.join(MANAGED_CODEX_CACHE_BINARY),
    };
    let managed_codex = shell_single_quote(&managed_codex_path.display().to_string());
    let codex_source = match codex_source_preference {
        CodexSourcePreference::Brew => "brew",
        CodexSourcePreference::Source => "source",
    };
    let mut lines = vec![
        "#!/usr/bin/env bash".to_owned(),
        "set -euo pipefail".to_owned(),
        format!("THREADBRIDGE_WORKSPACE_ROOT={workspace}"),
        format!("THREADBRIDGE_DATA_ROOT={quoted_data_root}"),
        format!("THREADBRIDGE_EXECUTABLE={threadbridge_executable}"),
        format!(
            "THREADBRIDGE_CODEX_SOURCE={}",
            shell_single_quote(codex_source)
        ),
        format!("THREADBRIDGE_MANAGED_CODEX={managed_codex}"),
        "cd \"$THREADBRIDGE_WORKSPACE_ROOT\"".to_owned(),
        "codex_bin=\"\"".to_owned(),
    ];
    match codex_source_preference {
        CodexSourcePreference::Brew => lines.extend([
            "codex_bin=\"$(command -v codex 2>/dev/null || true)\"".to_owned(),
            "if [ -z \"$codex_bin\" ] && [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then".to_owned(),
            "  codex_bin=\"$THREADBRIDGE_MANAGED_CODEX\"".to_owned(),
            "fi".to_owned(),
        ]),
        CodexSourcePreference::Source => lines.extend([
            "if [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then".to_owned(),
            "  codex_bin=\"$THREADBRIDGE_MANAGED_CODEX\"".to_owned(),
            "else".to_owned(),
            "  codex_bin=\"$(command -v codex 2>/dev/null || true)\"".to_owned(),
            "fi".to_owned(),
        ]),
    }
    lines.extend([
        "if [ -z \"$codex_bin\" ]; then".to_owned(),
        "  echo \"hcodex: could not find a codex binary\" >&2".to_owned(),
        "  exit 127".to_owned(),
        "fi".to_owned(),
        "requested_thread_key=\"\"".to_owned(),
        "codex_args=()".to_owned(),
        "while [ \"$#\" -gt 0 ]; do".to_owned(),
        "  case \"$1\" in".to_owned(),
        "    --thread-key)".to_owned(),
        "      shift".to_owned(),
        "      if [ \"$#\" -eq 0 ]; then".to_owned(),
        "        echo \"hcodex: missing value for --thread-key\" >&2".to_owned(),
        "        exit 2".to_owned(),
        "      fi".to_owned(),
        "      requested_thread_key=\"$1\"".to_owned(),
        "      ;;".to_owned(),
        "    *)".to_owned(),
        "      codex_args+=(\"$1\")".to_owned(),
        "      ;;".to_owned(),
        "  esac".to_owned(),
        "  shift".to_owned(),
        "done".to_owned(),
        "ensure_ready_file=\"$(mktemp -t threadbridge-hcodex-runtime-ready.XXXXXX)\"".to_owned(),
        "\"$THREADBRIDGE_EXECUTABLE\" ensure-hcodex-runtime --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\" --data-root \"$THREADBRIDGE_DATA_ROOT\" --parent-pid \"$$\" --ready-file \"$ensure_ready_file\" >/dev/null 2>&1 &".to_owned(),
        "ensure_pid=$!".to_owned(),
        "ensure_ready_json=\"\"".to_owned(),
        "for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40; do".to_owned(),
        "  if [ -s \"$ensure_ready_file\" ]; then".to_owned(),
        "    ensure_ready_json=\"$(cat \"$ensure_ready_file\")\"".to_owned(),
        "    break".to_owned(),
        "  fi".to_owned(),
        "  sleep 0.05".to_owned(),
        "done".to_owned(),
        "rm -f \"$ensure_ready_file\"".to_owned(),
        "if [ -z \"$ensure_ready_json\" ]; then".to_owned(),
        "  kill \"$ensure_pid\" >/dev/null 2>&1 || true".to_owned(),
        "  echo \"hcodex: shared runtime did not become ready\" >&2".to_owned(),
        "  exit 2".to_owned(),
        "fi".to_owned(),
        "launch_info=\"\"".to_owned(),
        "if [ -n \"$requested_thread_key\" ]; then".to_owned(),
        "  launch_info=\"$(\"$THREADBRIDGE_EXECUTABLE\" resolve-hcodex-launch --data-root \"$THREADBRIDGE_DATA_ROOT\" --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\" --thread-key \"$requested_thread_key\")\""
            .to_owned(),
        "else".to_owned(),
        "  launch_info=\"$(\"$THREADBRIDGE_EXECUTABLE\" resolve-hcodex-launch --data-root \"$THREADBRIDGE_DATA_ROOT\" --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\")\""
            .to_owned(),
        "fi".to_owned(),
        "launch_ws_url=\"\"".to_owned(),
        "resolved_thread_key=\"\"".to_owned(),
        "current_thread_id=\"\"".to_owned(),
        "IFS=$'\\t' read -r launch_ws_url resolved_thread_key current_thread_id <<< \"$launch_info\""
            .to_owned(),
        "# resolve-hcodex-launch returns an ingress launch URL, not a Codex-safe".to_owned(),
        "# --remote endpoint. It may include launch_ticket or future sideband".to_owned(),
        "# handshake state. run-hcodex-session must stay as the compatibility".to_owned(),
        "# boundary that bridges launch_ws_url to a bare ws://host:port before".to_owned(),
        "# spawning upstream codex. One hcodex launch maps to one local".to_owned(),
        "# Codex websocket client and one upstream ingress session.".to_owned(),
        "if [ \"${#codex_args[@]}\" -gt 0 ]; then".to_owned(),
        "  \"$THREADBRIDGE_EXECUTABLE\" run-hcodex-session --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\" --data-root \"$THREADBRIDGE_DATA_ROOT\" --thread-key \"$resolved_thread_key\" --codex-bin \"$codex_bin\" --remote-ws-url \"$launch_ws_url\" -- \"${codex_args[@]}\"".to_owned(),
        "else".to_owned(),
        "  \"$THREADBRIDGE_EXECUTABLE\" run-hcodex-session --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\" --data-root \"$THREADBRIDGE_DATA_ROOT\" --thread-key \"$resolved_thread_key\" --codex-bin \"$codex_bin\" --remote-ws-url \"$launch_ws_url\" --".to_owned(),
        "fi".to_owned(),
        "".to_owned(),
    ]);
    lines.join("\n")
}

fn build_hcodex_shell_compat_script(workspace_path: &Path) -> String {
    let launcher = shell_single_quote(
        &workspace_path
            .join(".threadbridge/bin/hcodex")
            .display()
            .to_string(),
    );
    [
        "# threadBridge hcodex compatibility shim",
        &format!(
            "export THREADBRIDGE_WORKSPACE_ROOT={}",
            shell_single_quote(&workspace_path.display().to_string())
        ),
        "hcodex() {",
        &format!("  {launcher} \"$@\""),
        "}",
        "",
    ]
    .join("\n")
}

fn build_runtime_gitignore() -> &'static str {
    "*\n!.gitignore\n"
}

async fn write_text_file_if_changed(path: &Path, contents: &str) -> Result<bool> {
    match fs::read_to_string(path).await {
        Ok(existing) if existing == contents => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(anyhow!("failed to read {}: {}", path.display(), error));
        }
    }

    fs::write(path, contents)
        .await
        .map_err(|error| anyhow!("failed to write {}: {}", path.display(), error))?;
    Ok(true)
}

fn remove_legacy_runtime_agents_appendix(content: &str) -> Result<Option<String>> {
    let Some(start) = content.find(THREADBRIDGE_RUNTIME_START) else {
        return Ok(None);
    };
    let Some(relative_end) = content[start..].find(THREADBRIDGE_RUNTIME_END) else {
        return Err(anyhow!(
            "legacy threadBridge runtime AGENTS.md block is missing end marker"
        ));
    };
    let marker_end = start + relative_end + THREADBRIDGE_RUNTIME_END.len();
    let block_start = content[..start]
        .rfind('\n')
        .map_or(0, |line_break| line_break + 1);
    let block_end = content[marker_end..]
        .find('\n')
        .map_or(content.len(), |line_break| marker_end + line_break + 1);

    let prefix = &content[..block_start];
    let suffix = &content[block_end..];
    let prefix = prefix.trim_end();
    let suffix = suffix.trim_start();
    let updated = match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => suffix.to_owned(),
        (false, true) => format!("{prefix}\n"),
        (false, false) => format!("{prefix}\n\n{suffix}"),
    };
    Ok(Some(updated))
}

pub async fn cleanup_legacy_runtime_agents_appendix(workspace_path: &Path) -> Result<bool> {
    let agents_path = workspace_path.join("AGENTS.md");
    let content = match fs::read_to_string(&agents_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", agents_path.display()));
        }
    };
    let Some(updated) = remove_legacy_runtime_agents_appendix(&content)? else {
        return Ok(false);
    };
    write_text_file_if_changed(&agents_path, &updated).await
}

async fn sync_workspace_runtime_skill(
    skill_template_path: &Path,
    runtime_root: &Path,
    stats: &mut WorkspaceEnsureStats,
) -> Result<()> {
    let skill_template = fs::read_to_string(skill_template_path)
        .await
        .with_context(|| {
            format!(
                "failed to read threadBridge runtime skill template: {}",
                skill_template_path.display()
            )
        })?;
    let skill_dir = runtime_root.join(THREADBRIDGE_RUNTIME_SKILL_DIR);
    fs::create_dir_all(&skill_dir).await.with_context(|| {
        format!(
            "failed to create threadBridge runtime skill directory: {}",
            skill_dir.display()
        )
    })?;
    stats.record_write(
        write_text_file_if_changed(
            &skill_dir.join(THREADBRIDGE_RUNTIME_SKILL_FILE),
            &skill_template,
        )
        .await?,
    );

    let Some(template_dir) = skill_template_path.parent() else {
        return Ok(());
    };
    let references_src = template_dir.join(THREADBRIDGE_RUNTIME_SKILL_REFERENCES_DIR);
    if !fs::try_exists(&references_src).await.with_context(|| {
        format!(
            "failed to inspect threadBridge runtime skill references: {}",
            references_src.display()
        )
    })? {
        return Ok(());
    }

    let references_dst = skill_dir.join(THREADBRIDGE_RUNTIME_SKILL_REFERENCES_DIR);
    fs::create_dir_all(&references_dst).await.with_context(|| {
        format!(
            "failed to create threadBridge runtime skill references directory: {}",
            references_dst.display()
        )
    })?;
    let mut entries = fs::read_dir(&references_src).await.with_context(|| {
        format!(
            "failed to read threadBridge runtime skill references: {}",
            references_src.display()
        )
    })?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_file() {
            return Err(anyhow!(
                "unsupported threadBridge runtime skill reference entry: {}",
                entry.path().display()
            ));
        }
        let content = fs::read_to_string(entry.path()).await.with_context(|| {
            format!(
                "failed to read threadBridge runtime skill reference: {}",
                entry.path().display()
            )
        })?;
        stats.record_write(
            write_text_file_if_changed(&references_dst.join(entry.file_name()), &content).await?,
        );
    }

    Ok(())
}

fn symlink_points_to_runtime_skill(
    link_path: &Path,
    current_target: &Path,
    runtime_skill_dir: &Path,
) -> bool {
    if current_target == Path::new(CODEX_REPO_RUNTIME_SKILL_TARGET) {
        return true;
    }

    let resolved_target = if current_target.is_absolute() {
        current_target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(current_target)
    };
    match (
        resolved_target.canonicalize(),
        runtime_skill_dir.canonicalize(),
    ) {
        (Ok(resolved), Ok(expected)) => resolved == expected,
        _ => false,
    }
}

fn create_dir_symlink(target: &Path, link_path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link_path)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link_path)
    }
}

async fn ensure_codex_repo_runtime_skill_link(
    workspace_path: &Path,
    runtime_root: &Path,
    stats: &mut WorkspaceEnsureStats,
) -> Result<()> {
    let runtime_skill_dir = runtime_root.join(THREADBRIDGE_RUNTIME_SKILL_DIR);
    let link_path = workspace_path.join(CODEX_REPO_RUNTIME_SKILL_DIR);
    let link_parent = link_path
        .parent()
        .context("Codex runtime skill link path has no parent")?;
    fs::create_dir_all(link_parent).await.with_context(|| {
        format!(
            "failed to create Codex workspace skills directory: {}",
            link_parent.display()
        )
    })?;

    match fs::symlink_metadata(&link_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current_target = fs::read_link(&link_path).await.with_context(|| {
                format!(
                    "failed to read Codex workspace runtime skill symlink: {}",
                    link_path.display()
                )
            })?;
            if symlink_points_to_runtime_skill(&link_path, &current_target, &runtime_skill_dir) {
                stats.record_write(false);
                return Ok(());
            }
            fs::remove_file(&link_path).await.with_context(|| {
                format!(
                    "failed to remove stale Codex workspace runtime skill symlink: {}",
                    link_path.display()
                )
            })?;
        }
        Ok(_) => {
            return Err(anyhow!(
                "Codex workspace runtime skill path already exists and is not a threadBridge symlink: {}",
                link_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect Codex workspace runtime skill link: {}",
                    link_path.display()
                )
            });
        }
    }

    create_dir_symlink(Path::new(CODEX_REPO_RUNTIME_SKILL_TARGET), &link_path).with_context(
        || {
            format!(
                "failed to create Codex workspace runtime skill symlink: {} -> {}",
                link_path.display(),
                CODEX_REPO_RUNTIME_SKILL_TARGET
            )
        },
    )?;
    stats.record_write(true);
    Ok(())
}

async fn set_mode_if_changed(path: &Path, mode: u32) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).await?;
        let current_mode = metadata.permissions().mode();
        if current_mode == mode {
            return Ok(false);
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).await?;
        return Ok(true);
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        return Ok(false);
    }
}

pub async fn ensure_workspace_runtime(
    runtime_support_root: &Path,
    data_root: &Path,
    runtime_skill_template_path: &Path,
    workspace_path: &Path,
) -> Result<PathBuf> {
    ensure_workspace_runtime_with_mode_and_telemetry(
        runtime_support_root,
        data_root,
        runtime_skill_template_path,
        workspace_path,
        WorkspaceRuntimeEnsureMode::ExplicitSync,
        None,
    )
    .await
}

pub async fn ensure_workspace_runtime_with_mode(
    runtime_support_root: &Path,
    data_root: &Path,
    runtime_skill_template_path: &Path,
    workspace_path: &Path,
    ensure_mode: WorkspaceRuntimeEnsureMode,
) -> Result<PathBuf> {
    ensure_workspace_runtime_with_mode_and_telemetry(
        runtime_support_root,
        data_root,
        runtime_skill_template_path,
        workspace_path,
        ensure_mode,
        None,
    )
    .await
}

pub async fn ensure_workspace_runtime_with_mode_and_telemetry(
    runtime_support_root: &Path,
    data_root: &Path,
    runtime_skill_template_path: &Path,
    workspace_path: &Path,
    ensure_mode: WorkspaceRuntimeEnsureMode,
    telemetry: Option<&RuntimeTelemetryHandle>,
) -> Result<PathBuf> {
    let started_at = Instant::now();
    let mut stats = WorkspaceEnsureStats::default();
    let result = async {
        let codex_source_preference = read_codex_source_preference(data_root).await?;
        let config_env_path = data_root.join("config.env.local");
        let threadbridge_executable = std::env::current_exe()
            .context("failed to resolve current threadBridge executable path")?;
        fs::create_dir_all(workspace_path).await.with_context(|| {
            format!(
                "failed to create workspace directory: {}",
                workspace_path.display()
            )
        })?;

        let runtime_root = workspace_path.join(THREADBRIDGE_RUNTIME_DIR);
        let bin_dir = runtime_root.join("bin");
        let shell_dir = runtime_root.join("shell");
        let tool_requests_dir = runtime_root.join("tool_requests");
        let tool_results_dir = runtime_root.join("tool_results");
        fs::create_dir_all(&bin_dir).await?;
        fs::create_dir_all(&shell_dir).await?;
        fs::create_dir_all(&tool_requests_dir).await?;
        fs::create_dir_all(&tool_results_dir).await?;
        stats.record_write(
            write_text_file_if_changed(&runtime_root.join(".gitignore"), build_runtime_gitignore())
                .await?,
        );
        sync_workspace_runtime_skill(runtime_skill_template_path, &runtime_root, &mut stats)
            .await?;
        ensure_codex_repo_runtime_skill_link(workspace_path, &runtime_root, &mut stats).await?;
        ensure_workspace_status_surface(workspace_path).await?;
        ensure_workspace_execution_config(workspace_path).await?;

        for (tool, filename) in [
            ("build_prompt_config.py", "build_prompt_config"),
            ("generate_image.py", "generate_image"),
            ("send_telegram_media.py", "send_telegram_media"),
        ] {
            let wrapper_path = bin_dir.join(filename);
            let wrapper = build_wrapper_script(tool, runtime_support_root, &config_env_path);
            stats.record_write(write_text_file_if_changed(&wrapper_path, &wrapper).await?);
            stats.record_mode_change(set_mode_if_changed(&wrapper_path, 0o755).await?);
        }

        let hcodex_path = bin_dir.join("hcodex");
        stats.record_write(
            write_text_file_if_changed(
                &hcodex_path,
                &build_hcodex_launcher_script(
                    workspace_path,
                    data_root,
                    &threadbridge_executable,
                    codex_source_preference,
                ),
            )
            .await?,
        );
        stats.record_mode_change(set_mode_if_changed(&hcodex_path, 0o755).await?);

        let shell_snippet_path = shell_dir.join("codex-sync.bash");
        stats.record_write(
            write_text_file_if_changed(
                &shell_snippet_path,
                &build_hcodex_shell_compat_script(workspace_path),
            )
            .await?,
        );
        stats.record_mode_change(set_mode_if_changed(&shell_snippet_path, 0o644).await?);

        if codex_source_preference == CodexSourcePreference::Brew {
            let managed_codex_source = data_root.join(MANAGED_CODEX_CACHE_BINARY);
            if fs::try_exists(&managed_codex_source)
                .await
                .with_context(|| {
                    format!(
                        "failed to inspect managed Codex binary: {}",
                        managed_codex_source.display()
                    )
                })?
            {
                let managed_codex_dest = bin_dir.join("codex");
                let should_copy = ensure_mode == WorkspaceRuntimeEnsureMode::ExplicitSync
                    || !fs::try_exists(&managed_codex_dest).await.with_context(|| {
                        format!(
                            "failed to inspect workspace managed Codex binary: {}",
                            managed_codex_dest.display()
                        )
                    })?;
                if should_copy {
                    fs::copy(&managed_codex_source, &managed_codex_dest)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to copy managed Codex binary from {} to {}",
                                managed_codex_source.display(),
                                managed_codex_dest.display()
                            )
                        })?;
                    stats.managed_codex_copied += 1;
                }
                stats.record_mode_change(set_mode_if_changed(&managed_codex_dest, 0o755).await?);
            }
        }

        Ok::<PathBuf, anyhow::Error>(runtime_root)
    }
    .await;

    if let Some(telemetry) = telemetry {
        let mut fields = RuntimeTelemetryFields::new();
        fields.insert("workspace".to_owned(), workspace_path.display().to_string());
        fields.insert(
            "ensure_mode".to_owned(),
            match ensure_mode {
                WorkspaceRuntimeEnsureMode::ExplicitSync => "explicit_sync",
                WorkspaceRuntimeEnsureMode::PassiveReconcile => "passive_reconcile",
            }
            .to_owned(),
        );
        let mut metrics = RuntimeTelemetryMetrics::new();
        metrics.insert("files_written".to_owned(), stats.files_written);
        metrics.insert("files_skipped".to_owned(), stats.files_skipped);
        metrics.insert("mode_changes".to_owned(), stats.mode_changes);
        metrics.insert("mode_unchanged".to_owned(), stats.mode_unchanged);
        metrics.insert(
            "managed_codex_copied".to_owned(),
            stats.managed_codex_copied,
        );
        match &result {
            Ok(_) => telemetry.record_duration(
                "workspace.ensure_runtime",
                started_at,
                "ok",
                fields,
                metrics,
                None,
            ),
            Err(error) => telemetry.record_duration(
                "workspace.ensure_runtime",
                started_at,
                "error",
                fields,
                metrics,
                Some(error.to_string()),
            ),
        }
    }

    result
}

pub fn validate_seed_template(runtime_skill_template_path: &Path) -> Result<PathBuf> {
    if !runtime_skill_template_path.exists() {
        anyhow::bail!(
            "Missing threadBridge runtime skill template: {}",
            runtime_skill_template_path.display()
        );
    }
    Ok(runtime_skill_template_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::{
        THREADBRIDGE_RUNTIME_DIR, THREADBRIDGE_RUNTIME_SKILL_DIR, WorkspaceRuntimeEnsureMode,
        cleanup_legacy_runtime_agents_appendix, create_dir_symlink, ensure_workspace_runtime,
        ensure_workspace_runtime_with_mode,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::fs;
    use tokio::time::sleep;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-workspace-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn workspace_runtime_preserves_project_agents_without_injection() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            "# Project AGENTS\n\nKeep local rules.\n",
        )
        .await
        .unwrap();
        fs::write(&template, "## threadBridge Runtime\n\n- use wrappers\n")
            .await
            .unwrap();

        ensure_workspace_runtime(
            Path::new("/runtime_support"),
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        let content = fs::read_to_string(workspace.join("AGENTS.md"))
            .await
            .unwrap();
        assert_eq!(content, "# Project AGENTS\n\nKeep local rules.\n");
        let skill = fs::read_to_string(
            workspace
                .join(THREADBRIDGE_RUNTIME_DIR)
                .join(THREADBRIDGE_RUNTIME_SKILL_DIR)
                .join("SKILL.md"),
        )
        .await
        .unwrap();
        assert!(skill.contains("## threadBridge Runtime"));
    }

    #[tokio::test]
    async fn workspace_runtime_creates_hidden_wrapper_surface() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        let runtime_root = ensure_workspace_runtime(
            Path::new("/runtime_support"),
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        assert_eq!(runtime_root, workspace.join(THREADBRIDGE_RUNTIME_DIR));
        assert!(
            fs::try_exists(workspace.join(".threadbridge/.gitignore"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/build_prompt_config"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/hcodex"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/shell/codex-sync.bash"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/tool_requests"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/tool_results"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(
                workspace
                    .join(".threadbridge")
                    .join(THREADBRIDGE_RUNTIME_SKILL_DIR)
                    .join("SKILL.md")
            )
            .await
            .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".codex/skills/threadbridge-runtime/SKILL.md"))
                .await
                .unwrap()
        );
        assert!(
            fs::symlink_metadata(workspace.join(".codex/skills/threadbridge-runtime"))
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/state/runtime-observer/current.json"))
                .await
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".threadbridge/.gitignore"))
                .await
                .unwrap(),
            "*\n!.gitignore\n"
        );
        let hcodex_launcher = fs::read_to_string(workspace.join(".threadbridge/bin/hcodex"))
            .await
            .unwrap();
        let compat_shell =
            fs::read_to_string(workspace.join(".threadbridge/shell/codex-sync.bash"))
                .await
                .unwrap();
        let build_prompt_wrapper =
            fs::read_to_string(workspace.join(".threadbridge/bin/build_prompt_config"))
                .await
                .unwrap();
        assert!(hcodex_launcher.contains("THREADBRIDGE_EXECUTABLE"));
        assert!(hcodex_launcher.contains("THREADBRIDGE_CODEX_SOURCE='brew'"));
        assert!(hcodex_launcher.contains("THREADBRIDGE_MANAGED_CODEX"));
        assert!(hcodex_launcher.contains(".threadbridge/bin/codex"));
        assert!(hcodex_launcher.contains("launch_ws_url=\"\""));
        assert!(hcodex_launcher.contains("ensure-hcodex-runtime"));
        assert!(hcodex_launcher.contains("resolve-hcodex-launch"));
        assert!(hcodex_launcher.contains("shared runtime did not become ready"));
        assert!(hcodex_launcher.contains("ingress launch URL"));
        assert!(hcodex_launcher.contains("run-hcodex-session"));
        assert!(hcodex_launcher.contains("--remote-ws-url \"$launch_ws_url\""));
        assert!(hcodex_launcher.contains("if [ \"${#codex_args[@]}\" -gt 0 ]; then"));
        assert!(hcodex_launcher.contains("codex_bin=\"$(command -v codex 2>/dev/null || true)\""));
        assert!(!hcodex_launcher.contains("THREADBRIDGE_HCODEX_RESOLVER"));
        assert!(build_prompt_wrapper.contains("THREADBRIDGE_RUNTIME_SUPPORT_ROOT"));
        assert!(build_prompt_wrapper.contains("THREADBRIDGE_CONFIG_ENV"));
        assert!(build_prompt_wrapper.contains("tools/build_prompt_config.py"));
        assert!(build_prompt_wrapper.contains("--config-env \"$THREADBRIDGE_CONFIG_ENV\""));
        assert!(compat_shell.contains("hcodex() {"));
        assert!(compat_shell.contains(".threadbridge/bin/hcodex"));
    }

    #[tokio::test]
    async fn workspace_runtime_copies_runtime_skill_references() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let workspace = root.join("workspace");
        let template_dir = root.join("templates/threadbridge-runtime-skill");
        let template = template_dir.join("SKILL.md");
        fs::create_dir_all(template_dir.join("references"))
            .await
            .unwrap();
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::write(&template, "runtime skill\n").await.unwrap();
        fs::write(template_dir.join("references/runtime-tools.md"), "tools\n")
            .await
            .unwrap();

        ensure_workspace_runtime(
            &runtime_support_root,
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        let skill_root = workspace
            .join(THREADBRIDGE_RUNTIME_DIR)
            .join(THREADBRIDGE_RUNTIME_SKILL_DIR);
        assert_eq!(
            fs::read_to_string(skill_root.join("SKILL.md"))
                .await
                .unwrap(),
            "runtime skill\n"
        );
        assert_eq!(
            fs::read_to_string(skill_root.join("references/runtime-tools.md"))
                .await
                .unwrap(),
            "tools\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".codex/skills/threadbridge-runtime/SKILL.md"))
                .await
                .unwrap(),
            "runtime skill\n"
        );
    }

    #[tokio::test]
    async fn workspace_runtime_repairs_stale_codex_runtime_skill_symlink() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::create_dir_all(workspace.join(".codex/skills"))
            .await
            .unwrap();
        fs::write(&template, "runtime skill\n").await.unwrap();
        create_dir_symlink(
            Path::new("../stale-threadbridge-runtime"),
            &workspace.join(".codex/skills/threadbridge-runtime"),
        )
        .unwrap();

        ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
            .await
            .unwrap();

        assert_eq!(
            fs::read_link(workspace.join(".codex/skills/threadbridge-runtime"))
                .await
                .unwrap(),
            PathBuf::from("../../.threadbridge/skills/threadbridge-runtime")
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".codex/skills/threadbridge-runtime/SKILL.md"))
                .await
                .unwrap(),
            "runtime skill\n"
        );
    }

    #[tokio::test]
    async fn workspace_runtime_rejects_existing_codex_runtime_skill_directory() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::create_dir_all(workspace.join(".codex/skills/threadbridge-runtime"))
            .await
            .unwrap();
        fs::write(&template, "runtime skill\n").await.unwrap();

        let error =
            ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
                .await
                .unwrap_err();
        assert!(
            error.to_string().contains(
                "Codex workspace runtime skill path already exists and is not a threadBridge symlink"
            ),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn workspace_runtime_copies_managed_codex_binary_when_available() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let managed_codex = data_root.join(".threadbridge/codex/codex");

        fs::create_dir_all(managed_codex.parent().unwrap())
            .await
            .unwrap();
        fs::write(&managed_codex, "managed codex binary")
            .await
            .unwrap();
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
            .await
            .unwrap();

        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/codex"))
                .await
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".threadbridge/bin/codex"))
                .await
                .unwrap(),
            "managed codex binary"
        );
    }

    #[tokio::test]
    async fn workspace_runtime_respects_source_codex_source_preference() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let source_file = data_root.join(".threadbridge/codex/source.txt");

        fs::create_dir_all(source_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&source_file, "source\n").await.unwrap();
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
            .await
            .unwrap();

        let hcodex_launcher = fs::read_to_string(workspace.join(".threadbridge/bin/hcodex"))
            .await
            .unwrap();
        assert!(hcodex_launcher.contains("THREADBRIDGE_CODEX_SOURCE='source'"));
        assert!(hcodex_launcher.contains("if [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then"));
        assert!(hcodex_launcher.contains(".threadbridge/codex/codex"));
        assert!(
            !fs::try_exists(workspace.join(".threadbridge/bin/codex"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn workspace_runtime_maps_legacy_alpha_codex_source_to_source() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let source_file = data_root.join(".threadbridge/codex/source.txt");

        fs::create_dir_all(source_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&source_file, "alpha\n").await.unwrap();
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
            .await
            .unwrap();

        let hcodex_launcher = fs::read_to_string(workspace.join(".threadbridge/bin/hcodex"))
            .await
            .unwrap();
        assert!(hcodex_launcher.contains("THREADBRIDGE_CODEX_SOURCE='source'"));
        assert!(hcodex_launcher.contains("if [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then"));
        assert!(hcodex_launcher.contains(".threadbridge/codex/codex"));
        assert!(
            !fs::try_exists(workspace.join(".threadbridge/bin/codex"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn workspace_runtime_does_not_create_agents_file_when_missing() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(
            Path::new("/runtime_support"),
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        assert!(!fs::try_exists(workspace.join("AGENTS.md")).await.unwrap());
        let content = fs::read_to_string(
            workspace
                .join(THREADBRIDGE_RUNTIME_DIR)
                .join(THREADBRIDGE_RUNTIME_SKILL_DIR)
                .join("SKILL.md"),
        )
        .await
        .unwrap();
        assert!(content.contains("runtime appendix"));
    }

    #[tokio::test]
    async fn legacy_runtime_agents_cleanup_removes_only_managed_block() {
        let root = temp_path();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            "# Project AGENTS\n\nKeep local rules.\n\n<!-- threadbridge:runtime:start -->\nmanaged runtime appendix\n<!-- threadbridge:runtime:end -->\n",
        )
        .await
        .unwrap();

        assert!(
            cleanup_legacy_runtime_agents_appendix(&workspace)
                .await
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md"))
                .await
                .unwrap(),
            "# Project AGENTS\n\nKeep local rules.\n"
        );
    }

    #[tokio::test]
    async fn legacy_runtime_agents_cleanup_preserves_files_without_marker() {
        let root = temp_path();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join("AGENTS.md"), "# Project AGENTS\n")
            .await
            .unwrap();

        assert!(
            !cleanup_legacy_runtime_agents_appendix(&workspace)
                .await
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md"))
                .await
                .unwrap(),
            "# Project AGENTS\n"
        );
    }

    #[tokio::test]
    async fn legacy_runtime_agents_cleanup_rejects_partial_marker() {
        let root = temp_path();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            "# Project AGENTS\n\n<!-- threadbridge:runtime:start -->\nmanaged runtime appendix\n",
        )
        .await
        .unwrap();

        let error = cleanup_legacy_runtime_agents_appendix(&workspace)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("legacy threadBridge runtime AGENTS.md block is missing end marker")
        );
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md"))
                .await
                .unwrap(),
            "# Project AGENTS\n\n<!-- threadbridge:runtime:start -->\nmanaged runtime appendix\n"
        );
    }

    #[tokio::test]
    async fn passive_reconcile_does_not_rewrite_unchanged_hcodex_launcher() {
        let root = temp_path();
        let runtime_support_root = root.join("runtime_support");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&runtime_support_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_support_root, &data_root, &template, &workspace)
            .await
            .unwrap();

        let hcodex_path = workspace.join(".threadbridge/bin/hcodex");
        let first_modified = fs::metadata(&hcodex_path)
            .await
            .unwrap()
            .modified()
            .unwrap();
        sleep(Duration::from_millis(20)).await;

        ensure_workspace_runtime_with_mode(
            &runtime_support_root,
            &data_root,
            &template,
            &workspace,
            WorkspaceRuntimeEnsureMode::PassiveReconcile,
        )
        .await
        .unwrap();

        let second_modified = fs::metadata(&hcodex_path)
            .await
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first_modified, second_modified);
    }
}
