//! npm global packages (`npm install -g`). node/npm usually comes from pixi, so this
//! adapter only shows up after the first sync has installed `[pixi.nodejs]`.
//!
//! adopt never decides where npm installs: the prefix is whatever npm reports. A prefix
//! inside a pixi environment (lost when the env is rebuilt) or one this user cannot
//! write (would need sudo) fails that entry with a hint instead of installing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::{Adapter, Ctx, Op, Outcome, PkgSpec, batch_then_each};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};
use crate::state::entry::VERSION;

#[derive(Default)]
pub struct Npm;

/// Leftover in package names npm ships itself with node; never the user's choice.
const BUILT_IN: [&str; 2] = ["npm", "corepack"];

impl Npm {
    pub fn new() -> Npm {
        Npm
    }

    /// Cached `npm prefix -g`, written after a successful lookup so `status --prompt`
    /// can watch the right directory without starting node.
    fn cached_prefix_file(paths: &Paths) -> PathBuf {
        paths.cache_dir.join("npm-prefix")
    }

    /// The prefix without running anything: `NPM_CONFIG_PREFIX`, then `~/.npmrc`, then
    /// the cache. `None` simply means npm is not watched.
    pub fn prefix_from_config(paths: &Paths) -> Option<PathBuf> {
        if let Some(p) = &paths.npm_prefix_env {
            return Some(p.clone());
        }
        if let Some(p) = std::fs::read_to_string(paths.home.join(".npmrc"))
            .ok()
            .and_then(|text| npmrc_prefix(&text, paths))
        {
            return Some(p);
        }
        std::fs::read_to_string(Self::cached_prefix_file(paths))
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.is_absolute())
    }

    /// `npm prefix -g`, and remember it for [`Npm::prefix_from_config`].
    fn ask_prefix(cx: &Ctx) -> Result<PathBuf> {
        let cmd = Cmd::new("npm").args(["prefix", "-g"]);
        let out = cx.runner.run(&cmd)?;
        if !out.success() {
            bail!("`{cmd}` 失败：\n{}", out.tail());
        }
        let prefix = PathBuf::from(out.stdout.trim());
        if !prefix.is_absolute() {
            bail!("`{cmd}` 给出的不是绝对路径：{}", out.stdout.trim());
        }
        Self::remember_prefix(cx, &prefix);
        Ok(prefix)
    }

    /// Best effort: only used to make the next `status --prompt` cheap.
    fn remember_prefix(cx: &Ctx, prefix: &Path) {
        if cx.dry_run {
            return;
        }
        let file = Self::cached_prefix_file(cx.paths);
        let text = format!("{}\n", prefix.display());
        if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
            let _ = crate::state::write_atomic(&file, &text);
        }
    }

    fn node_modules(prefix: &Path) -> PathBuf {
        prefix.join("lib").join("node_modules")
    }

    fn install_cmd(specs: &[&str]) -> Cmd {
        Cmd::new("npm")
            .arg("install")
            .arg("-g")
            .args(specs.iter().copied())
    }

    fn uninstall_cmd(names: &[&str]) -> Cmd {
        Cmd::new("npm")
            .arg("uninstall")
            .arg("-g")
            .args(names.iter().copied())
    }

    /// Why npm cannot install here, if it cannot.
    fn prefix_problem(prefix: &Path, paths: &Paths) -> Option<String> {
        if prefix.to_string_lossy().contains("/.pixi/envs/") {
            return Some(format!(
                "npm 的全局 prefix 在 pixi 环境里（{}），重建那个环境时这些包会丢。\n    \
                 请在 ~/.npmrc 写一行 `prefix=${{HOME}}/.local`",
                paths.tilde(prefix)
            ));
        }
        match std::fs::metadata(prefix) {
            Ok(_) if is_writable(prefix, &paths.home) => None,
            Ok(_) => Some(format!(
                "npm 的全局 prefix（{}）不可写，adopt 不会用 sudo 装 npm 包。\n    \
                 请在 ~/.npmrc 写一行 `prefix=${{HOME}}/.local`",
                paths.tilde(prefix)
            )),
            Err(_) => Some(format!(
                "npm 的全局 prefix（{}）不存在，请先创建它。\n    \
                 在 ~/.npmrc 写一行 `prefix=${{HOME}}/.local` 即可",
                paths.tilde(prefix)
            )),
        }
    }
}

/// `prefix=/x` or `prefix = ${HOME}/.local`, with `~` and `${HOME}` expanded.
pub fn npmrc_prefix(text: &str, paths: &Paths) -> Option<PathBuf> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "prefix" {
            continue;
        }
        let value = value.split('#').next().unwrap_or("").trim();
        let expanded = value
            .strip_prefix("${HOME}/")
            .or_else(|| value.strip_prefix("$HOME/"))
            .map(|rest| paths.home.join(rest))
            .or_else(|| value.strip_prefix("~/").map(|rest| paths.home.join(rest)))
            .unwrap_or_else(|| PathBuf::from(value));
        if expanded.as_os_str().is_empty() {
            continue;
        }
        return Some(expanded);
    }
    None
}

/// Writable by us, judged from the mode bits. `home` stands in for "our uid": a file
/// we own there tells us what our owner bits mean. No writes, no `access(2)`.
fn is_writable(p: &Path, home: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    let mine = std::fs::metadata(home).map(|h| h.uid()).ok();
    let perm = if Some(m.uid()) == mine {
        (m.mode() >> 6) & 0o7
    } else {
        m.mode() & 0o7
    };
    perm & 0o2 != 0
}

/// `npm ls -g --depth=0 --parseable`: `<prefix>/lib` first, then one path per package
/// under `<prefix>/lib/node_modules`. npm itself and corepack are dropped.
pub fn parse_ls_parseable(out: &str) -> BTreeSet<String> {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return BTreeSet::new();
    };
    let base = format!("{}/node_modules/", first.trim_end_matches('/'));
    lines
        .filter_map(|l| l.strip_prefix(&base))
        .filter(|name| !name.is_empty() && !BUILT_IN.contains(name))
        .map(str::to_string)
        .collect()
}

/// An empty prefix makes `npm ls -g` exit 254 with ENOENT; that is "nothing installed".
fn is_empty_prefix_error(out: &crate::runner::Output) -> bool {
    !out.success() && out.stderr.contains("ENOENT")
}

/// `name` or `name@range`, from the entry's `version`.
fn spec_string(p: &PkgSpec) -> String {
    let version = p.body.as_ref().and_then(|t| t.get(VERSION)?.as_str());
    match version {
        Some(v) => format!("{}@{v}", p.native),
        None => p.native.clone(),
    }
}

impl Adapter for Npm {
    fn id(&self) -> ManagerId {
        ManagerId::Npm
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool {
        runner.which("npm")
    }

    fn needs_root(&self) -> bool {
        false
    }

    /// node_modules plus each `@scope` directory: installing a second package into an
    /// existing scope only changes the scope directory's mtime, not node_modules'.
    fn watch_paths(&self, paths: &Paths) -> Vec<PathBuf> {
        let Some(prefix) = Self::prefix_from_config(paths) else {
            return Vec::new();
        };
        let modules = Self::node_modules(&prefix);
        let mut out = vec![modules.clone()];
        if let Ok(entries) = std::fs::read_dir(&modules) {
            for entry in entries.flatten() {
                let scoped = entry.file_name().to_string_lossy().starts_with('@')
                    && entry.file_type().is_ok_and(|t| t.is_dir());
                if scoped {
                    out.push(entry.path());
                }
            }
        }
        out
    }

    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>> {
        let cmd = Cmd::new("npm").args(["ls", "-g", "--depth=0", "--parseable"]);
        let out = cx.runner.run(&cmd)?;
        if is_empty_prefix_error(&out) {
            return Ok(BTreeSet::new());
        }
        if !out.success() {
            bail!("`{cmd}` 失败：\n{}", out.tail());
        }
        // The first line is `<prefix>/lib`: the prefix comes for free, no `npm prefix -g`.
        if let Some(prefix) = out
            .stdout
            .lines()
            .next()
            .and_then(|l| Path::new(l.trim()).parent())
            .filter(|p| p.is_absolute())
        {
            Self::remember_prefix(cx, prefix);
        }
        Ok(parse_ls_parseable(&out.stdout))
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd> {
        if pkgs.is_empty() {
            return Vec::new();
        }
        let specs: Vec<String> = pkgs.iter().map(spec_string).collect();
        let names: Vec<&str> = specs.iter().map(String::as_str).collect();
        match op {
            Op::Install => vec![Self::install_cmd(&names)],
            Op::Remove => vec![Self::uninstall_cmd(&names)],
        }
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        if pkgs.is_empty() {
            return Ok(Vec::new());
        }
        if cx.dry_run {
            // Nothing runs, so the prefix does not matter; show what would.
            for cmd in self.planned_commands(pkgs, Op::Install) {
                cx.ui.would(cmd.to_string());
            }
            return Ok(pkgs
                .iter()
                .map(|p| Outcome {
                    logical: p.logical.clone(),
                    result: Ok(()),
                })
                .collect());
        }
        let prefix = Self::ask_prefix(cx)?;
        if let Some(why) = Self::prefix_problem(&prefix, cx.paths) {
            return Ok(pkgs
                .iter()
                .map(|p| Outcome {
                    logical: p.logical.clone(),
                    result: Err(why.clone()),
                })
                .collect());
        }
        batch_then_each(cx, pkgs, spec_string, Self::install_cmd, false)
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        batch_then_each(cx, pkgs, |p| p.native.clone(), Self::uninstall_cmd, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::sandbox(dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        (dir, paths)
    }

    #[test]
    fn parses_real_parseable_output() {
        let out = include_str!("../../tests/fixtures/npm-ls-parseable.txt");
        let names = parse_ls_parseable(out);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["@anthropic-ai/claude-code", "@openai/codex", "pyright"]
        );
    }

    #[test]
    fn drops_npm_and_corepack_but_keeps_scopes() {
        let out = "/p/lib\n/p/lib/node_modules/npm\n/p/lib/node_modules/corepack\n/p/lib/node_modules/@a/b\n";
        let names = parse_ls_parseable(out);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["@a/b"]);
    }

    #[test]
    fn empty_output_has_no_packages() {
        assert!(parse_ls_parseable("").is_empty());
        // npm on a prefix without lib/ prints ENOENT; an empty set is the right reading.
        let err =
            crate::runner::Output::fail(254, "npm error code ENOENT\nnpm error syscall lstat");
        assert!(is_empty_prefix_error(&err));
    }

    #[test]
    fn reads_npmrc_prefix() {
        let (_d, paths) = paths();
        assert_eq!(
            npmrc_prefix("prefix=${HOME}/.local\n", &paths),
            Some(paths.home.join(".local"))
        );
        assert_eq!(
            npmrc_prefix(
                "; prefix=/nope\nregistry=x\nprefix = ~/.npm-global\n",
                &paths
            ),
            Some(paths.home.join(".npm-global"))
        );
        assert_eq!(
            npmrc_prefix("prefix=/opt/npm\n", &paths),
            Some("/opt/npm".into())
        );
        assert_eq!(npmrc_prefix("registry=https://example\n", &paths), None);
    }

    #[test]
    fn prefix_problems() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::sandbox(dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        let own = paths.home.join(".local");
        std::fs::create_dir_all(&own).unwrap();
        assert_eq!(Npm::prefix_problem(&own, &paths), None);
        let pixi = paths.home.join(".pixi/envs/nodejs");
        std::fs::create_dir_all(&pixi).unwrap();
        let why = Npm::prefix_problem(&pixi, &paths).unwrap();
        assert!(
            why.contains("pixi 环境") && why.contains("prefix=${HOME}/.local"),
            "{why}"
        );
        if std::fs::metadata("/usr").is_ok_and(|m| {
            use std::os::unix::fs::MetadataExt;
            m.uid() != std::fs::metadata(&paths.home).unwrap().uid()
        }) {
            assert!(Npm::prefix_problem(Path::new("/usr"), &paths).is_some());
        }
        assert!(Npm::prefix_problem(&paths.home.join("missing"), &paths).is_some());
    }
}
