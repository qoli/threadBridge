use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use tokio::fs;

use crate::workspace_status::ensure_workspace_status_surface;

pub const THREADBRIDGE_RUNTIME_DIR: &str = ".threadbridge";
pub const THREADBRIDGE_RUNTIME_START: &str = "<!-- threadbridge:runtime:start -->";
pub const THREADBRIDGE_RUNTIME_END: &str = "<!-- threadbridge:runtime:end -->";
const MANAGED_CODEX_CACHE_BINARY: &str = ".threadbridge/codex/codex";
const MANAGED_CODEX_SOURCE_FILE: &str = ".threadbridge/codex/source.txt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexSourcePreference {
    Brew,
    Alpha,
}

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

fn build_codex_sync_manage_wrapper_script(repo_root: &Path) -> String {
    let quoted_repo_root = shell_single_quote(&repo_root.display().to_string());
    [
        "#!/bin/sh",
        "set -eu",
        "SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname \"$0\")\" && pwd)\"",
        "RUNTIME_DIR=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"",
        "WORKSPACE_DIR=\"$(CDPATH= cd -- \"$RUNTIME_DIR/..\" && pwd)\"",
        &format!("REPO_ROOT={quoted_repo_root}"),
        "cd \"$WORKSPACE_DIR\"",
        "exec python3 \"$REPO_ROOT/tools/codex_sync.py\" \"$@\" --workspace \"$WORKSPACE_DIR\"",
        "",
    ]
    .join("\n")
}

fn build_codex_launch_wrapper_script() -> String {
    [
        "#!/bin/sh",
        "set -eu",
        "PID_FILE=\"${1:-}\"",
        "if [ $# -gt 0 ]; then",
        "  shift",
        "fi",
        "if [ $# -eq 0 ]; then",
        "  echo \"threadBridge codex launcher requires a command\" >&2",
        "  exit 2",
        "fi",
        "if [ -n \"$PID_FILE\" ]; then",
        "  CHILD_PGID=\"$(ps -o pgid= -p \"$$\" 2>/dev/null | tr -d ' ')\"",
        "  printf '%s\\t%s\\t%s\\n' \"$$\" \"$CHILD_PGID\" \"$*\" > \"$PID_FILE\" 2>/dev/null || true",
        "  if [ -n \"${THREADBRIDGE_CODEX_SYNC_MANAGE:-}\" ] && [ -n \"${THREADBRIDGE_CODEX_SHELL_PID:-}\" ]; then",
        "    \"$THREADBRIDGE_CODEX_SYNC_MANAGE\" record-child-process --shell-pid \"$THREADBRIDGE_CODEX_SHELL_PID\" --child-pid \"$$\" --child-pgid \"$CHILD_PGID\" --child-command \"$*\" >/dev/null 2>&1 || true",
        "  fi",
        "fi",
        "exec \"$@\"",
        "",
    ]
    .join("\n")
}

async fn read_codex_source_preference(repo_root: &Path) -> Result<CodexSourcePreference> {
    let source_path = repo_root.join(MANAGED_CODEX_SOURCE_FILE);
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
        "alpha" => Ok(CodexSourcePreference::Alpha),
        "brew" | "" => Ok(CodexSourcePreference::Brew),
        other => Err(anyhow!(
            "unsupported Codex source preference in {}: {}",
            source_path.display(),
            other
        )),
    }
}

fn build_codex_shell_snippet(
    workspace_path: &Path,
    repo_root: &Path,
    data_root: &Path,
    codex_source_preference: CodexSourcePreference,
) -> String {
    let workspace = shell_single_quote(&workspace_path.display().to_string());
    let data_root = shell_single_quote(&data_root.display().to_string());
    let resolver = shell_single_quote(
        &repo_root
            .join("tools/resolve_hcodex_launch.py")
            .display()
            .to_string(),
    );
    let managed_codex = shell_single_quote(
        &workspace_path
            .join(".threadbridge/bin/codex")
            .display()
            .to_string(),
    );
    let codex_source = match codex_source_preference {
        CodexSourcePreference::Brew => "brew",
        CodexSourcePreference::Alpha => "alpha",
    };
    let mut lines = vec![
        "# threadBridge shared app-server CLI".to_owned(),
        format!("export THREADBRIDGE_WORKSPACE_ROOT={workspace}"),
        format!("export THREADBRIDGE_DATA_ROOT={data_root}"),
        format!("export THREADBRIDGE_HCODEX_RESOLVER={resolver}"),
        format!(
            "export THREADBRIDGE_CODEX_SOURCE={}",
            shell_single_quote(codex_source)
        ),
        format!("export THREADBRIDGE_MANAGED_CODEX={managed_codex}"),
        "".to_owned(),
        "__threadbridge_codex_in_workspace() {".to_owned(),
        "  local current_dir".to_owned(),
        "  current_dir=\"$(pwd -P 2>/dev/null || pwd)\"".to_owned(),
        "  case \"$PWD/\" in".to_owned(),
        "    \"$THREADBRIDGE_WORKSPACE_ROOT\"/*|\"$THREADBRIDGE_WORKSPACE_ROOT/\") return 0 ;;"
            .to_owned(),
        "  esac".to_owned(),
        "  case \"$current_dir/\" in".to_owned(),
        "    \"$THREADBRIDGE_WORKSPACE_ROOT\"/*|\"$THREADBRIDGE_WORKSPACE_ROOT/\") return 0 ;;"
            .to_owned(),
        "    *) return 1 ;;".to_owned(),
        "  esac".to_owned(),
        "}".to_owned(),
        "".to_owned(),
        "hcodex() {".to_owned(),
        "  if ! __threadbridge_codex_in_workspace; then".to_owned(),
        "    command codex \"$@\"".to_owned(),
        "    return $?".to_owned(),
        "  fi".to_owned(),
        "  local codex_bin".to_owned(),
    ];
    match codex_source_preference {
        CodexSourcePreference::Brew => lines.extend([
            "  codex_bin=\"$(command -v codex 2>/dev/null || true)\"".to_owned(),
            "  if [ -z \"$codex_bin\" ] && [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then".to_owned(),
            "    codex_bin=\"$THREADBRIDGE_MANAGED_CODEX\"".to_owned(),
            "  fi".to_owned(),
        ]),
        CodexSourcePreference::Alpha => lines.extend([
            "  if [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then".to_owned(),
            "    codex_bin=\"$THREADBRIDGE_MANAGED_CODEX\"".to_owned(),
            "  else".to_owned(),
            "    codex_bin=\"$(command -v codex 2>/dev/null || true)\"".to_owned(),
            "  fi".to_owned(),
        ]),
    }
    lines.extend([
        "  if [ -z \"$codex_bin\" ]; then".to_owned(),
        "    echo \"hcodex: could not find a codex binary\" >&2".to_owned(),
        "    return 127".to_owned(),
        "  fi".to_owned(),
        "  local requested_thread_key=\"\"".to_owned(),
        "  local -a codex_args=()".to_owned(),
        "  while [ \"$#\" -gt 0 ]; do".to_owned(),
        "    case \"$1\" in".to_owned(),
        "      --thread-key)".to_owned(),
        "        shift".to_owned(),
        "        if [ \"$#\" -eq 0 ]; then".to_owned(),
        "          echo \"hcodex: missing value for --thread-key\" >&2".to_owned(),
        "          return 2".to_owned(),
        "        fi".to_owned(),
        "        requested_thread_key=\"$1\"".to_owned(),
        "        ;;".to_owned(),
        "      *)".to_owned(),
        "        codex_args+=(\"$1\")".to_owned(),
        "        ;;".to_owned(),
        "    esac".to_owned(),
        "    shift".to_owned(),
        "  done".to_owned(),
        "  local launch_info".to_owned(),
        "  if [ -n \"$requested_thread_key\" ]; then".to_owned(),
        "    launch_info=\"$(python3 \"$THREADBRIDGE_HCODEX_RESOLVER\" --data-root \"$THREADBRIDGE_DATA_ROOT\" --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\" --thread-key \"$requested_thread_key\")\" || return $?"
            .to_owned(),
        "  else".to_owned(),
        "    launch_info=\"$(python3 \"$THREADBRIDGE_HCODEX_RESOLVER\" --data-root \"$THREADBRIDGE_DATA_ROOT\" --workspace \"$THREADBRIDGE_WORKSPACE_ROOT\")\" || return $?"
            .to_owned(),
        "  fi".to_owned(),
        "  local daemon_ws_url resolved_thread_key current_thread_id".to_owned(),
        "  IFS=$'\\t' read -r daemon_ws_url resolved_thread_key current_thread_id <<< \"$launch_info\""
            .to_owned(),
        "  if [ \"${#codex_args[@]}\" -eq 0 ]; then".to_owned(),
        "    exec \"$codex_bin\" --remote \"$daemon_ws_url\" resume \"$current_thread_id\"".to_owned(),
        "  fi".to_owned(),
        "  exec \"$codex_bin\" --remote \"$daemon_ws_url\" \"${codex_args[@]}\"".to_owned(),
        "}".to_owned(),
        "".to_owned(),
    ]);
    lines.join("\n")
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
                    "command": format!("{} --hook-event SessionStart --shell-pid \"$THREADBRIDGE_CODEX_SHELL_PID\" --owner-thread-key \"$THREADBRIDGE_CODEX_OWNER_THREAD_KEY\"", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge session start sync"
                }]
            }],
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --hook-event UserPromptSubmit --shell-pid \"$THREADBRIDGE_CODEX_SHELL_PID\" --owner-thread-key \"$THREADBRIDGE_CODEX_OWNER_THREAD_KEY\"", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge prompt sync"
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --hook-event Stop --shell-pid \"$THREADBRIDGE_CODEX_SHELL_PID\" --owner-thread-key \"$THREADBRIDGE_CODEX_OWNER_THREAD_KEY\"", shell_single_quote(&event_wrapper)),
                    "statusMessage": "threadBridge stop sync"
                }]
            }]
        }
    }))
    .unwrap()
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
    repo_root: &Path,
    data_root: &Path,
    seed_template_path: &Path,
    workspace_path: &Path,
) -> Result<PathBuf> {
    let codex_source_preference = read_codex_source_preference(repo_root).await?;
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

    let manage_wrapper_path = bin_dir.join("codex_sync_manage");
    write_text_file(
        &manage_wrapper_path,
        &build_codex_sync_manage_wrapper_script(repo_root),
    )
    .await?;
    set_mode(&manage_wrapper_path, 0o755).await?;

    let launch_wrapper_path = bin_dir.join("codex_launch");
    write_text_file(&launch_wrapper_path, &build_codex_launch_wrapper_script()).await?;
    set_mode(&launch_wrapper_path, 0o755).await?;

    let managed_codex_source = repo_root.join(MANAGED_CODEX_CACHE_BINARY);
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

    let shell_snippet_path = shell_dir.join("codex-sync.bash");
    write_text_file(
        &shell_snippet_path,
        &build_codex_shell_snippet(
            workspace_path,
            repo_root,
            data_root,
            codex_source_preference,
        ),
    )
    .await?;
    set_mode(&shell_snippet_path, 0o644).await?;

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

        ensure_workspace_runtime(
            Path::new("/repo"),
            Path::new("/repo/data"),
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
            Path::new("/repo"),
            Path::new("/repo/data"),
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
            fs::try_exists(workspace.join(".threadbridge/bin/codex_sync_manage"))
                .await
                .unwrap()
        );
        assert!(
            fs::try_exists(workspace.join(".threadbridge/bin/codex_launch"))
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
        assert_eq!(
            fs::read_to_string(workspace.join(".threadbridge/.gitignore"))
                .await
                .unwrap(),
            "*\n!.gitignore\n"
        );
        let shell_snippet =
            fs::read_to_string(workspace.join(".threadbridge/shell/codex-sync.bash"))
                .await
                .unwrap();
        assert!(shell_snippet.contains("hcodex()"));
        assert!(shell_snippet.contains("THREADBRIDGE_HCODEX_RESOLVER"));
        assert!(shell_snippet.contains("THREADBRIDGE_CODEX_SOURCE='brew'"));
        assert!(shell_snippet.contains("THREADBRIDGE_MANAGED_CODEX"));
        assert!(shell_snippet.contains(".threadbridge/bin/codex"));
        assert!(shell_snippet.contains("--remote \"$daemon_ws_url\""));
        assert!(shell_snippet.contains("codex_bin=\"$(command -v codex 2>/dev/null || true)\""));
    }

    #[tokio::test]
    async fn workspace_runtime_copies_managed_codex_binary_when_available() {
        let root = temp_path();
        let repo_root = root.join("repo");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let managed_codex = repo_root.join(".threadbridge/codex/codex");

        fs::create_dir_all(managed_codex.parent().unwrap())
            .await
            .unwrap();
        fs::write(&managed_codex, "managed codex binary")
            .await
            .unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&repo_root, &repo_root.join("data"), &template, &workspace)
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
    async fn workspace_runtime_respects_alpha_codex_source_preference() {
        let root = temp_path();
        let repo_root = root.join("repo");
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        let source_file = repo_root.join(".threadbridge/codex/source.txt");

        fs::create_dir_all(source_file.parent().unwrap())
            .await
            .unwrap();
        fs::write(&source_file, "alpha\n").await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(&repo_root, &repo_root.join("data"), &template, &workspace)
            .await
            .unwrap();

        let shell_snippet =
            fs::read_to_string(workspace.join(".threadbridge/shell/codex-sync.bash"))
                .await
                .unwrap();
        assert!(shell_snippet.contains("THREADBRIDGE_CODEX_SOURCE='alpha'"));
        assert!(shell_snippet.contains("if [ -x \"$THREADBRIDGE_MANAGED_CODEX\" ]; then"));
    }

    #[tokio::test]
    async fn workspace_runtime_creates_agents_file_when_missing() {
        let root = temp_path();
        let workspace = root.join("workspace");
        let template = root.join("template.md");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(&template, "runtime appendix\n").await.unwrap();

        ensure_workspace_runtime(
            Path::new("/repo"),
            Path::new("/repo/data"),
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
