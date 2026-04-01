use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use tokio::fs;

use crate::execution_mode::ensure_workspace_execution_config;
use crate::workspace_status::ensure_workspace_status_surface;

pub const THREADBRIDGE_RUNTIME_DIR: &str = ".threadbridge";
pub const THREADBRIDGE_RUNTIME_START: &str = "<!-- threadbridge:runtime:start -->";
pub const THREADBRIDGE_RUNTIME_END: &str = "<!-- threadbridge:runtime:end -->";
const MANAGED_CODEX_CACHE_BINARY: &str = ".threadbridge/codex/codex";
const MANAGED_CODEX_SOURCE_FILE: &str = ".threadbridge/codex/source.txt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexSourcePreference {
    Brew,
    Source,
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn build_wrapper_script(
    tool_file_name: &str,
    runtime_assets_root: &Path,
    config_env_path: &Path,
) -> String {
    let quoted_runtime_assets_root = shell_single_quote(&runtime_assets_root.display().to_string());
    let quoted_config_env_path = shell_single_quote(&config_env_path.display().to_string());
    [
        "#!/bin/sh",
        "set -eu",
        "SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\"",
        "RUNTIME_DIR=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"",
        "WORKSPACE_DIR=\"$(CDPATH= cd -- \"$RUNTIME_DIR/..\" && pwd)\"",
        &format!("THREADBRIDGE_RUNTIME_ASSETS_ROOT={quoted_runtime_assets_root}"),
        &format!("THREADBRIDGE_CONFIG_ENV={quoted_config_env_path}"),
        "cd \"$WORKSPACE_DIR\"",
        &format!(
            "exec python3 \"$THREADBRIDGE_RUNTIME_ASSETS_ROOT/tools/{tool_file_name}\" --config-env \"$THREADBRIDGE_CONFIG_ENV\" \"$@\""
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

fn managed_appendix_block(appendix: &str) -> String {
    format!(
        "{THREADBRIDGE_RUNTIME_START}\n{}\n{THREADBRIDGE_RUNTIME_END}\n",
        appendix.trim_end()
    )
}

fn sync_managed_appendix(existing: &str, appendix: &str) -> String {
    let block = managed_appendix_block(appendix);
    if let (Some(start), Some(end)) = (
        existing.find(THREADBRIDGE_RUNTIME_START),
        existing.find(THREADBRIDGE_RUNTIME_END),
    ) {
        let suffix_end = end + THREADBRIDGE_RUNTIME_END.len();
        let mut updated = String::new();
        updated.push_str(existing[..start].trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block.trim_end());
        let suffix = existing[suffix_end..].trim();
        if !suffix.is_empty() {
            updated.push_str("\n\n");
            updated.push_str(suffix);
        }
        updated.push('\n');
        return updated;
    }

    if existing.trim().is_empty() {
        return block;
    }

    format!("{}\n\n{}", existing.trim_end(), block)
}

async fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)
        .await
        .map_err(|error| anyhow!("failed to write {}: {}", path.display(), error))
}

async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).await?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

pub async fn ensure_workspace_runtime(
    runtime_assets_root: &Path,
    data_root: &Path,
    seed_template_path: &Path,
    workspace_path: &Path,
) -> Result<PathBuf> {
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

    let appendix = fs::read_to_string(seed_template_path)
        .await
        .with_context(|| {
            format!(
                "failed to read threadBridge appendix template: {}",
                seed_template_path.display()
            )
        })?;

    let agents_path = workspace_path.join("AGENTS.md");
    match fs::read_to_string(&agents_path).await {
        Ok(existing) => {
            let updated = sync_managed_appendix(&existing, &appendix);
            if updated != existing {
                write_text_file(&agents_path, &updated).await?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let initial_content = managed_appendix_block(&appendix);
            write_text_file(&agents_path, &initial_content).await?;
        }
        Err(error) => {
            return Err(anyhow!(
                "failed to read {}: {}",
                agents_path.display(),
                error
            ));
        }
    }

    let runtime_root = workspace_path.join(THREADBRIDGE_RUNTIME_DIR);
    let bin_dir = runtime_root.join("bin");
    let shell_dir = runtime_root.join("shell");
    let tool_requests_dir = runtime_root.join("tool_requests");
    let tool_results_dir = runtime_root.join("tool_results");
    fs::create_dir_all(&bin_dir).await?;
    fs::create_dir_all(&shell_dir).await?;
    fs::create_dir_all(&tool_requests_dir).await?;
    fs::create_dir_all(&tool_results_dir).await?;
    write_text_file(&runtime_root.join(".gitignore"), build_runtime_gitignore()).await?;
    ensure_workspace_status_surface(workspace_path).await?;
    ensure_workspace_execution_config(workspace_path).await?;

    for (tool, filename) in [
        ("build_prompt_config.py", "build_prompt_config"),
        ("generate_image.py", "generate_image"),
        ("send_telegram_media.py", "send_telegram_media"),
    ] {
        let wrapper_path = bin_dir.join(filename);
        let wrapper = build_wrapper_script(tool, runtime_assets_root, &config_env_path);
        write_text_file(&wrapper_path, &wrapper).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&wrapper_path).await?;
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&wrapper_path, permissions).await?;
        }
    }

    let hcodex_path = bin_dir.join("hcodex");
    write_text_file(
        &hcodex_path,
        &build_hcodex_launcher_script(
            workspace_path,
            data_root,
            &threadbridge_executable,
            codex_source_preference,
        ),
    )
    .await?;
    set_mode(&hcodex_path, 0o755).await?;

    let shell_snippet_path = shell_dir.join("codex-sync.bash");
    write_text_file(
        &shell_snippet_path,
        &build_hcodex_shell_compat_script(workspace_path),
    )
    .await?;
    set_mode(&shell_snippet_path, 0o644).await?;

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
            fs::copy(&managed_codex_source, &managed_codex_dest)
                .await
                .with_context(|| {
                    format!(
                        "failed to copy managed Codex binary from {} to {}",
                        managed_codex_source.display(),
                        managed_codex_dest.display()
                    )
                })?;
            set_mode(&managed_codex_dest, 0o755).await?;
        }
    }

    Ok(runtime_root)
}

pub fn validate_seed_template(seed_template_path: &Path) -> Result<PathBuf> {
    if !seed_template_path.exists() {
        anyhow::bail!(
            "Missing template AGENTS.md: {}",
            seed_template_path.display()
        );
    }
    Ok(seed_template_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::{
        THREADBRIDGE_RUNTIME_DIR, THREADBRIDGE_RUNTIME_END, THREADBRIDGE_RUNTIME_START,
        ensure_workspace_runtime,
    };
    use std::path::{Path, PathBuf};
    use tokio::fs;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-workspace-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn workspace_runtime_appends_managed_block_without_overwriting() {
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
            Path::new("/runtime_assets"),
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        let content = fs::read_to_string(workspace.join("AGENTS.md"))
            .await
            .unwrap();
        assert!(content.contains("# Project AGENTS"));
        assert!(content.contains(THREADBRIDGE_RUNTIME_START));
        assert!(content.contains(THREADBRIDGE_RUNTIME_END));
    }

    #[tokio::test]
    async fn workspace_runtime_creates_hidden_wrapper_surface() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        let runtime_root = ensure_workspace_runtime(
            Path::new("/runtime_assets"),
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
        assert!(build_prompt_wrapper.contains("THREADBRIDGE_RUNTIME_ASSETS_ROOT"));
        assert!(build_prompt_wrapper.contains("THREADBRIDGE_CONFIG_ENV"));
        assert!(build_prompt_wrapper.contains("tools/build_prompt_config.py"));
        assert!(build_prompt_wrapper.contains("--config-env \"$THREADBRIDGE_CONFIG_ENV\""));
        assert!(compat_shell.contains("hcodex() {"));
        assert!(compat_shell.contains(".threadbridge/bin/hcodex"));
    }

    #[tokio::test]
    async fn workspace_runtime_copies_managed_codex_binary_when_available() {
        let root = temp_path();
        let runtime_assets_root = root.join("runtime_assets");
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
        fs::create_dir_all(&runtime_assets_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_assets_root, &data_root, &template, &workspace)
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
        let runtime_assets_root = root.join("runtime_assets");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let source_file = data_root.join(".threadbridge/codex/source.txt");

        fs::create_dir_all(source_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&source_file, "source\n").await.unwrap();
        fs::create_dir_all(&runtime_assets_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_assets_root, &data_root, &template, &workspace)
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
        let runtime_assets_root = root.join("runtime_assets");
        let data_root = root.join("data");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let source_file = data_root.join(".threadbridge/codex/source.txt");

        fs::create_dir_all(source_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&source_file, "alpha\n").await.unwrap();
        fs::create_dir_all(&runtime_assets_root).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&runtime_assets_root, &data_root, &template, &workspace)
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
    async fn workspace_runtime_creates_agents_file_when_missing() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(
            Path::new("/runtime_assets"),
            Path::new("/data"),
            &template,
            &workspace,
        )
        .await
        .unwrap();

        let content = fs::read_to_string(workspace.join("AGENTS.md"))
            .await
            .unwrap();
        assert!(content.contains(THREADBRIDGE_RUNTIME_START));
        assert!(content.contains("runtime appendix"));
        assert!(content.contains(THREADBRIDGE_RUNTIME_END));
    }
}
