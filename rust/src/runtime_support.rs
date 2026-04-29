use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use tokio::fs;

use crate::config::RuntimeConfig;

pub async fn ensure_runtime_support(runtime: &RuntimeConfig) -> Result<()> {
    ensure_seed_template_exists(runtime).await?;
    if !runtime.supports_runtime_support_rebuild() {
        return Ok(());
    }
    copy_runtime_support_tree(
        &runtime.runtime_support_seed_root_path,
        &runtime.runtime_support_root_path,
        CopyMode::MissingOnly,
    )
    .await?;
    ensure_active_template_exists(runtime).await
}

pub async fn rebuild_runtime_support(runtime: &RuntimeConfig) -> Result<()> {
    ensure!(
        runtime.supports_runtime_support_rebuild(),
        "runtime support rebuild is only available in the bundled desktop app"
    );
    ensure_seed_template_exists(runtime).await?;
    if fs::try_exists(&runtime.runtime_support_root_path)
        .await
        .with_context(|| {
            format!(
                "failed to inspect {}",
                runtime.runtime_support_root_path.display()
            )
        })?
    {
        fs::remove_dir_all(&runtime.runtime_support_root_path)
            .await
            .with_context(|| {
                format!(
                    "failed to remove {}",
                    runtime.runtime_support_root_path.display()
                )
            })?;
    }
    copy_runtime_support_tree(
        &runtime.runtime_support_seed_root_path,
        &runtime.runtime_support_root_path,
        CopyMode::OverwriteAll,
    )
    .await?;
    ensure_active_template_exists(runtime).await
}

#[derive(Clone, Copy)]
enum CopyMode {
    MissingOnly,
    OverwriteAll,
}

async fn ensure_seed_template_exists(runtime: &RuntimeConfig) -> Result<()> {
    let seed_template = runtime
        .runtime_support_seed_root_path
        .join("templates")
        .join("threadbridge-runtime-skill")
        .join("SKILL.md");
    ensure!(
        fs::try_exists(&seed_template)
            .await
            .with_context(|| format!("failed to inspect {}", seed_template.display()))?,
        "missing runtime support template: {}",
        seed_template.display()
    );
    Ok(())
}

async fn ensure_active_template_exists(runtime: &RuntimeConfig) -> Result<()> {
    let active_template = runtime.runtime_skill_template_path();
    ensure!(
        fs::try_exists(&active_template)
            .await
            .with_context(|| format!("failed to inspect {}", active_template.display()))?,
        "missing runtime support template: {}",
        active_template.display()
    );
    Ok(())
}

async fn copy_runtime_support_tree(src: &Path, dst: &Path, mode: CopyMode) -> Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((src_dir, dst_dir)) = stack.pop() {
        fs::create_dir_all(&dst_dir)
            .await
            .with_context(|| format!("failed to create {}", dst_dir.display()))?;
        let mut entries = fs::read_dir(&src_dir)
            .await
            .with_context(|| format!("failed to read {}", src_dir.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let src_path = entry.path();
            let dst_path = dst_dir.join(entry.file_name());
            if file_type.is_dir() {
                stack.push((src_path, dst_path));
                continue;
            }
            if !file_type.is_file() {
                return Err(anyhow!(
                    "unsupported runtime support entry: {}",
                    src_path.display()
                ));
            }
            match mode {
                CopyMode::MissingOnly => {
                    if fs::try_exists(&dst_path)
                        .await
                        .with_context(|| format!("failed to inspect {}", dst_path.display()))?
                    {
                        continue;
                    }
                }
                CopyMode::OverwriteAll => {}
            }
            fs::copy(&src_path, &dst_path).await.with_context(|| {
                format!(
                    "failed to copy runtime support entry from {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}
