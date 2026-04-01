use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;

const DEBUG_DATA_DIR_NAME: &str = "data";
const APP_DATA_DIR_NAME: &str = "threadBridge";
const RUNTIME_ASSETS_DIR_NAME: &str = "runtime_assets";
const DEBUG_EVENTS_RELATIVE_PATH: &str = "debug/events.jsonl";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BuildFlavor {
    Debug,
    Release,
}

impl BuildFlavor {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimePathOverrides {
    pub data_root: Option<String>,
    pub bot_data_path: Option<String>,
    pub debug_log_path: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimePaths {
    pub data_root_path: PathBuf,
    pub debug_log_path: PathBuf,
    pub runtime_assets_root_path: PathBuf,
    pub runtime_assets_seed_root_path: PathBuf,
}

pub fn resolve_runtime_paths(overrides: RuntimePathOverrides) -> Result<RuntimePaths> {
    let cwd = std::env::current_dir().context("failed to read current working directory")?;
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    resolve_runtime_paths_with(
        &cwd,
        &current_exe,
        BuildFlavor::current(),
        default_local_data_dir(),
        overrides,
    )
}

fn default_local_data_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|dirs| dirs.data_local_dir().to_path_buf())
}

fn resolve_runtime_paths_with(
    cwd: &Path,
    current_exe: &Path,
    build_flavor: BuildFlavor,
    platform_local_data_dir: Option<PathBuf>,
    overrides: RuntimePathOverrides,
) -> Result<RuntimePaths> {
    let source_tree_root = resolve_source_tree_root(current_exe, cwd);
    let data_root_path = resolve_data_root(
        cwd,
        build_flavor,
        platform_local_data_dir,
        &overrides,
        source_tree_root.as_deref(),
    )?;
    let debug_log_path = match overrides.debug_log_path {
        Some(path) => resolve_from_base(cwd, path),
        None => data_root_path.join(DEBUG_EVENTS_RELATIVE_PATH),
    };
    let (runtime_assets_root_path, runtime_assets_seed_root_path) =
        resolve_runtime_assets_roots(current_exe, cwd, &data_root_path)?;
    Ok(RuntimePaths {
        data_root_path,
        debug_log_path,
        runtime_assets_root_path,
        runtime_assets_seed_root_path,
    })
}

fn resolve_data_root(
    cwd: &Path,
    build_flavor: BuildFlavor,
    platform_local_data_dir: Option<PathBuf>,
    overrides: &RuntimePathOverrides,
    source_tree_root: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(path) = overrides.data_root.clone() {
        return Ok(resolve_from_base(cwd, path));
    }

    if let Some(path) = overrides.bot_data_path.clone() {
        let bot_data_path = resolve_from_base(cwd, path);
        return Ok(bot_data_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join(DEBUG_DATA_DIR_NAME)));
    }

    match build_flavor {
        BuildFlavor::Debug => Ok(source_tree_root
            .map(|root| root.join(DEBUG_DATA_DIR_NAME))
            .unwrap_or_else(|| resolve_from_base(cwd, DEBUG_DATA_DIR_NAME))),
        BuildFlavor::Release => {
            let root = platform_local_data_dir.context(
                "failed to resolve a local application data directory for the release runtime",
            )?;
            Ok(root.join(APP_DATA_DIR_NAME).join(DEBUG_DATA_DIR_NAME))
        }
    }
}

fn resolve_runtime_assets_roots(
    current_exe: &Path,
    cwd: &Path,
    data_root_path: &Path,
) -> Result<(PathBuf, PathBuf)> {
    if let Some(seed_root) = resolve_bundle_seed_runtime_assets_root(current_exe) {
        let runtime_home = data_root_path.parent().context(
            "failed to resolve runtime assets root because data root has no parent directory",
        )?;
        return Ok((runtime_home.join(RUNTIME_ASSETS_DIR_NAME), seed_root));
    }

    if let Some(source_tree_root) = resolve_source_tree_root(current_exe, cwd) {
        let runtime_assets_root = source_tree_root.join(RUNTIME_ASSETS_DIR_NAME);
        return Ok((runtime_assets_root.clone(), runtime_assets_root));
    }

    bail!(
        "failed to resolve runtime assets root; expected `{}` beside the source checkout or inside the app bundle resources",
        RUNTIME_ASSETS_DIR_NAME
    );
}

fn resolve_bundle_seed_runtime_assets_root(current_exe: &Path) -> Option<PathBuf> {
    let resources_dir = current_exe.parent()?.parent()?.join("Resources");
    let runtime_assets_root = resources_dir.join(RUNTIME_ASSETS_DIR_NAME);
    runtime_assets_root.is_dir().then_some(runtime_assets_root)
}

fn resolve_source_tree_root(current_exe: &Path, cwd: &Path) -> Option<PathBuf> {
    find_source_tree_root(current_exe.parent()).or_else(|| find_source_tree_root(Some(cwd)))
}

fn find_source_tree_root(start: Option<&Path>) -> Option<PathBuf> {
    let mut current = start;
    while let Some(path) = current {
        if path.join(RUNTIME_ASSETS_DIR_NAME).is_dir() && path.join("Cargo.toml").is_file() {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

fn resolve_from_base(base: &Path, input: impl AsRef<Path>) -> PathBuf {
    let path = input.as_ref();
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    joined
        .canonicalize()
        .unwrap_or_else(|_| joined.components().collect())
}

#[cfg(test)]
mod tests {
    use super::{
        BuildFlavor, DEBUG_EVENTS_RELATIVE_PATH, RuntimePathOverrides, resolve_runtime_paths_with,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "threadbridge-runtime-paths-{name}-{}",
            Uuid::new_v4()
        ))
    }

    fn path_string(path: &Path) -> String {
        path.display().to_string()
    }

    fn create_source_tree() -> (PathBuf, PathBuf) {
        let root = temp_path("source-tree");
        fs::create_dir_all(root.join("runtime_assets")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"threadbridge-test\"\n",
        )
        .unwrap();
        let current_exe = root.join("target/debug/threadbridge_desktop");
        fs::create_dir_all(current_exe.parent().unwrap()).unwrap();
        (root, current_exe)
    }

    #[test]
    fn debug_build_defaults_to_source_tree_layout() {
        let (source_root, current_exe) = create_source_tree();
        let cwd = temp_path("debug-default");
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Debug,
            Some(temp_path("platform")),
            RuntimePathOverrides::default(),
        )
        .unwrap();
        assert_eq!(paths.data_root_path, source_root.join("data"));
        assert_eq!(
            paths.debug_log_path,
            source_root.join("data").join(DEBUG_EVENTS_RELATIVE_PATH)
        );
        assert_eq!(
            paths.runtime_assets_root_path,
            source_root.join("runtime_assets")
        );
        assert_eq!(
            paths.runtime_assets_seed_root_path,
            source_root.join("runtime_assets")
        );
    }

    #[test]
    fn release_build_defaults_to_platform_data_dir_and_bundle_runtime_assets() {
        let cwd = temp_path("release-default");
        let platform_root = temp_path("platform");
        let app_root = temp_path("bundle-root").join("threadBridge.app");
        let bundle_seed_root = app_root.join("Contents/Resources/runtime_assets");
        fs::create_dir_all(&bundle_seed_root).unwrap();
        let current_exe = app_root.join("Contents/MacOS/threadbridge_desktop");
        fs::create_dir_all(current_exe.parent().unwrap()).unwrap();
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Release,
            Some(platform_root.clone()),
            RuntimePathOverrides::default(),
        )
        .unwrap();
        assert_eq!(
            paths.data_root_path,
            platform_root.join("threadBridge/data")
        );
        assert_eq!(
            paths.debug_log_path,
            platform_root
                .join("threadBridge/data")
                .join(DEBUG_EVENTS_RELATIVE_PATH)
        );
        assert_eq!(
            paths.runtime_assets_root_path,
            platform_root.join("threadBridge/runtime_assets")
        );
        assert_eq!(paths.runtime_assets_seed_root_path, bundle_seed_root);
    }

    #[test]
    fn data_root_override_has_highest_precedence() {
        let (source_root, current_exe) = create_source_tree();
        let cwd = temp_path("override");
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Release,
            None,
            RuntimePathOverrides {
                data_root: Some("./custom-data".to_owned()),
                bot_data_path: Some("./ignored/state.json".to_owned()),
                debug_log_path: None,
            },
        )
        .unwrap();
        assert_eq!(paths.data_root_path, cwd.join("custom-data"));
        assert_eq!(
            paths.debug_log_path,
            cwd.join("custom-data").join(DEBUG_EVENTS_RELATIVE_PATH)
        );
        assert_eq!(
            paths.runtime_assets_root_path,
            source_root.join("runtime_assets")
        );
    }

    #[test]
    fn legacy_bot_data_path_uses_parent_directory() {
        let (_, current_exe) = create_source_tree();
        let cwd = temp_path("legacy");
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Release,
            Some(temp_path("platform")),
            RuntimePathOverrides {
                data_root: None,
                bot_data_path: Some("./legacy/state.json".to_owned()),
                debug_log_path: None,
            },
        )
        .unwrap();
        assert_eq!(paths.data_root_path, cwd.join("legacy"));
    }

    #[test]
    fn debug_log_override_is_resolved_relative_to_cwd() {
        let (_, current_exe) = create_source_tree();
        let cwd = temp_path("debug-log");
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Debug,
            None,
            RuntimePathOverrides {
                data_root: Some("./custom-data".to_owned()),
                bot_data_path: None,
                debug_log_path: Some("./logs/custom.jsonl".to_owned()),
            },
        )
        .unwrap();
        assert_eq!(paths.data_root_path, cwd.join("custom-data"));
        assert_eq!(paths.debug_log_path, cwd.join("logs/custom.jsonl"));
    }

    #[test]
    fn release_build_requires_platform_local_data_dir_without_overrides() {
        let (_, current_exe) = create_source_tree();
        let error = resolve_runtime_paths_with(
            &temp_path("missing-platform"),
            &current_exe,
            BuildFlavor::Release,
            None,
            RuntimePathOverrides::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to resolve a local application data directory")
        );
    }

    #[test]
    fn absolute_overrides_are_preserved() {
        let (_, current_exe) = create_source_tree();
        let cwd = temp_path("absolute");
        let data_root = temp_path("explicit-data-root");
        let debug_log = temp_path("explicit-debug-log").join("events.jsonl");
        let paths = resolve_runtime_paths_with(
            &cwd,
            &current_exe,
            BuildFlavor::Debug,
            None,
            RuntimePathOverrides {
                data_root: Some(path_string(&data_root)),
                bot_data_path: None,
                debug_log_path: Some(path_string(&debug_log)),
            },
        )
        .unwrap();
        assert_eq!(paths.data_root_path, data_root);
        assert_eq!(paths.debug_log_path, debug_log);
    }
}
