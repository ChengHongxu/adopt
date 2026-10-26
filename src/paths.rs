//! Every filesystem location adopt touches. Built once in `main`; tests point all of
//! it into a temp dir so nothing under the real HOME or `/` is ever read or written.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    /// `~/.config/adopt`
    pub config_dir: PathBuf,
    /// `~/.cache/adopt`
    pub cache_dir: PathBuf,
    /// `$PIXI_HOME/manifests/pixi-global.toml`
    pub pixi_manifest: PathBuf,
    /// Prefix for system files (`/var/lib/dpkg/status`, ...). `/` in production.
    pub root: PathBuf,
    /// `NPM_CONFIG_PREFIX` / `npm_config_prefix` from the environment, if set.
    pub npm_prefix_env: Option<PathBuf>,
}

impl Paths {
    pub fn from_env() -> Result<Paths> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let xdg = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        let pixi_home = std::env::var_os("PIXI_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pixi"));
        Ok(Paths {
            config_dir: xdg("XDG_CONFIG_HOME", ".config").join("adopt"),
            cache_dir: xdg("XDG_CACHE_HOME", ".cache").join("adopt"),
            pixi_manifest: pixi_home.join("manifests").join("pixi-global.toml"),
            root: PathBuf::from("/"),
            npm_prefix_env: ["NPM_CONFIG_PREFIX", "npm_config_prefix"]
                .iter()
                .find_map(|v| std::env::var_os(v).filter(|v| !v.is_empty()))
                .map(PathBuf::from),
            home,
        })
    }

    /// Layout used by tests: everything lives under `base`.
    pub fn sandbox(base: &Path) -> Paths {
        let home = base.join("home");
        Paths {
            config_dir: home.join(".config/adopt"),
            cache_dir: home.join(".cache/adopt"),
            pixi_manifest: home.join(".pixi/manifests/pixi-global.toml"),
            root: base.join("root"),
            npm_prefix_env: None,
            home,
        }
    }

    pub fn local_toml(&self) -> PathBuf {
        self.config_dir.join("local.toml")
    }

    pub fn status_cache(&self) -> PathBuf {
        self.cache_dir.join("status.toml")
    }

    /// Resolve an absolute system path (e.g. `/var/lib/dpkg/status`) under `root`.
    pub fn sys(&self, abs: &str) -> PathBuf {
        self.root.join(abs.trim_start_matches('/'))
    }

    /// `/home/u/.zshrc` -> `~/.zshrc`, for display only.
    pub fn tilde(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => p.display().to_string(),
        }
    }

    /// Expand a leading `~/` typed by the user.
    pub fn expand(&self, s: &str) -> PathBuf {
        match s.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None if s == "~" => self.home.clone(),
            None => PathBuf::from(s),
        }
    }
}
