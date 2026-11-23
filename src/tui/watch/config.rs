//! Resolving the watch target: the workspace, the sources, and the binary a
//! successful build should replace this process with.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Which cargo profile the restarted binary comes from. Inferred from the
/// running executable so `--watch` never silently switches a release run
/// to an unoptimized rebuild (or the reverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    fn running() -> Self {
        let under_release = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .and_then(|dir| dir.file_name().map(|name| name == "release"))
            .unwrap_or(false);
        if under_release {
            Self::Release
        } else {
            Self::Debug
        }
    }

    pub(super) fn dir(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    pub(super) fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Self::Debug => None,
            Self::Release => Some("--release"),
        }
    }

    /// For the startup line and the footer.
    pub fn label(self) -> &'static str {
        self.dir()
    }
}

/// What to watch and what to exec after a successful build.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub profile: Profile,
    pub binary: PathBuf,
    pub(super) roots: Vec<PathBuf>,
}

impl Config {
    /// Resolve the project around `cwd` and the binary this run should
    /// become. Fails unless that project has a `gg` binary target.
    pub fn discover(cwd: &Path) -> anyhow::Result<Self> {
        let output = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(cwd)
            .output()
            .context("running cargo metadata")?;
        if !output.status.success() {
            bail!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let meta: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("parsing cargo metadata")?;
        let root = meta["workspace_root"]
            .as_str()
            .context("cargo metadata: no workspace_root")?;
        let target_dir = meta["target_directory"]
            .as_str()
            .context("cargo metadata: no target_directory")?;

        let packages = meta["packages"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let builds_gg = packages.iter().any(|package| {
            package["name"] == "ganggang"
                && package["targets"].as_array().is_some_and(|targets| {
                    targets.iter().any(|target| {
                        target["name"] == "gg"
                            && target["kind"]
                                .as_array()
                                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
                    })
                })
        });
        if !builds_gg {
            bail!(
                "--watch must run inside the ganggang repo: {root} does not build the `gg` binary"
            );
        }

        let root = PathBuf::from(root);
        let src = root.join("src");
        if !src.is_dir() {
            bail!("--watch: {} has no src/ directory", root.display());
        }
        let mut roots = vec![src, root.join("Cargo.toml")];
        let lock = root.join("Cargo.lock");
        if lock.exists() {
            roots.push(lock);
        }
        let cargo_config = root.join(".cargo").join("config.toml");
        if cargo_config.exists() {
            roots.push(cargo_config);
        }

        let profile = Profile::running();
        let binary = Path::new(target_dir).join(profile.dir()).join("gg");
        Ok(Self {
            root,
            profile,
            binary,
            roots,
        })
    }
}
