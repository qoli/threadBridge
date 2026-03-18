use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use tokio::fs;

use crate::workspace_status::ensure_workspace_status_surface;

pub const THREADBRIDGE_RUNTIME_DIR: &str = ".threadbridge";
pub const THREADBRIDGE_RUNTIME_START: &str = "<!-- threadbridge:runtime:start -->";
pub const THREADBRIDGE_RUNTIME_END: &str = "<!-- threadbridge:runtime:end -->";

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn build_wrapper_script(tool_file_name: &str, repo_root: &Path) -> String {
    let quoted_repo_root = shell_single_quote(&repo_root.display().to_string());
    [
        "#!/bin/sh",
        "set -eu",
        "SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\"",
        "RUNTIME_DIR=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"",
        "WORKSPACE_DIR=\"$(CDPATH= cd -- \"$RUNTIME_DIR/..\" && pwd)\"",
        &format!("REPO_ROOT={quoted_repo_root}"),
        "cd \"$WORKSPACE_DIR\"",
        &format!(
            "exec python3 \"$REPO_ROOT/tools/{tool_file_name}\" --repo-root \"$REPO_ROOT\" \"$@\""
        ),
        "",
    ]
    .join("\n")
}

fn build_codex_sync_wrapper_script(subcommand: &str, repo_root: &Path) -> String {
    let quoted_repo_root = shell_single_quote(&repo_root.display().to_string());
    [
        "#!/bin/sh",
        "set -eu",
        "SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\"",
        "RUNTIME_DIR=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"",
        "WORKSPACE_DIR=\"$(CDPATH= cd -- \"$RUNTIME_DIR/..\" && pwd)\"",
        &format!("REPO_ROOT={quoted_repo_root}"),
        "cd \"$WORKSPACE_DIR\"",
        &format!(
            "exec python3 \"$REPO_ROOT/tools/codex_sync.py\" {subcommand} --workspace \"$WORKSPACE_DIR\" \"$@\""
        ),
        "",
    ]
    .join("\n")
}

fn build_codex_shell_snippet(workspace_path: &Path) -> String {
    let workspace = shell_single_quote(&workspace_path.display().to_string());
    let event_wrapper = shell_single_quote(
        &workspace_path
            .join(".threadbridge/bin/codex_sync_event")
            .display()
            .to_string(),
    );
    let notify_wrapper = workspace_path
        .join(".threadbridge/bin/codex_sync_notify")
        .display()
        .to_string();
    let notify_json = serde_json::to_string(&vec![notify_wrapper]).unwrap_or_else(|_| "[]".into());
    let notify_json = shell_single_quote(&notify_json);
    [
        "# threadBridge Codex CLI sync",
        &format!("export THREADBRIDGE_WORKSPACE_ROOT={workspace}"),
        &format!("export THREADBRIDGE_CODEX_SYNC_EVENT={event_wrapper}"),
        &format!("export THREADBRIDGE_CODEX_NOTIFY_JSON={notify_json}"),
        "",
        "__threadbridge_codex_in_workspace() {",
        "  case \"$PWD/\" in",
        "    \"$THREADBRIDGE_WORKSPACE_ROOT\"/*|\"$THREADBRIDGE_WORKSPACE_ROOT/\") return 0 ;;",
        "    *) return 1 ;;",
        "  esac",
        "}",
        "",
        "codex() {",
        "  if ! __threadbridge_codex_in_workspace; then",
        "    command codex \"$@\"",
        "    return $?",
        "  fi",
        "  \"$THREADBRIDGE_CODEX_SYNC_EVENT\" shell_process_started --shell-pid \"$$\" >/dev/null 2>&1 || true",
        "  command codex -c features.codex_hooks=true -c \"notify=$THREADBRIDGE_CODEX_NOTIFY_JSON\" \"$@\"",
        "  local status=$?",
        "  \"$THREADBRIDGE_CODEX_SYNC_EVENT\" shell_process_exited --shell-pid \"$$\" --exit-code \"$status\" >/dev/null 2>&1 || true",
        "  return \"$status\"",
        "}",
        "",
    ]
    .join("\n")
}

fn build_codex_hooks_json(workspace_path: &Path) -> String {
    let event_wrapper = workspace_path
        .join(".threadbridge/bin/codex_sync_event")
        .display()
        .to_string();
    serde_json::to_string_pretty(&serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --hook-event SessionStart", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge session start sync"
                }]
            }],
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --hook-event UserPromptSubmit", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge prompt sync"
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --hook-event Stop", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge stop sync"
                }]
            }]
        }
    }))
    .unwrap()
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

pub async fn ensure_workspace_runtime(
    repo_root: &Path,
    seed_template_path: &Path,
    workspace_path: &Path,
) -> Result<PathBuf> {
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
    ensure_workspace_status_surface(workspace_path).await?;

    for (tool, filename) in [
        ("build_prompt_config.py", "build_prompt_config"),
        ("generate_image.py", "generate_image"),
        ("send_telegram_media.py", "send_telegram_media"),
    ] {
        let wrapper_path = bin_dir.join(filename);
        let wrapper = build_wrapper_script(tool, repo_root);
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

    for (subcommand, filename) in [
        ("event", "codex_sync_event"),
        ("notify", "codex_sync_notify"),
    ] {
        let wrapper_path = bin_dir.join(filename);
        let wrapper = build_codex_sync_wrapper_script(subcommand, repo_root);
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

    let shell_snippet_path = shell_dir.join("codex-sync.bash");
    write_text_file(
        &shell_snippet_path,
        &build_codex_shell_snippet(workspace_path),
    )
    .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&shell_snippet_path).await?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&shell_snippet_path, permissions).await?;
    }

    let codex_dir = workspace_path.join(".codex");
    fs::create_dir_all(&codex_dir).await?;
    write_text_file(
        &codex_dir.join("hooks.json"),
        &format!("{}\n", build_codex_hooks_json(workspace_path)),
    )
    .await?;

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

        ensure_workspace_runtime(Path::new("/repo"), &template, &workspace)
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

        let runtime_root = ensure_workspace_runtime(Path::new("/repo"), &template, &workspace)
            .await
            .unwrap();

        assert_eq!(runtime_root, workspace.join(THREADBRIDGE_RUNTIME_DIR));
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/build_prompt_config"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/codex_sync_event"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/codex_sync_notify"))
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
            fs::try_exists(workspace.join(".codex/hooks.json"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/state/codex-sync/current.json"))
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

        ensure_workspace_runtime(Path::new("/repo"), &template, &workspace)
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
