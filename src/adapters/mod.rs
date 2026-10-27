//! One adapter per package manager. Adding a new one means a new [`Section`], a new
//! [`ManagerId`], and one more impl of [`Adapter`].

pub mod apt;
pub mod brew;
pub mod dnf;
pub mod npm;
pub mod pixi;
pub mod script;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use toml_edit::Table;

use crate::model::{ManagerId, Section};
use crate::paths::Paths;
use crate::privilege::PrivilegeCell;
use crate::runner::{Cmd, CommandRunner};
use crate::ui::Ui;

pub struct Ctx<'a> {
    pub runner: &'a dyn CommandRunner,
    pub paths: &'a Paths,
    pub ui: &'a Ui,
    pub dry_run: bool,
    pub interactive: bool,
    pub privilege: &'a PrivilegeCell,
}

impl Ctx<'_> {
    /// `cmd` wrapped in sudo when needed, or why root is not available.
    pub fn elevate(&self, cmd: Cmd) -> std::result::Result<Cmd, String> {
        self.privilege.elevate(self.runner, self.interactive, cmd)
    }
}

/// What to install or remove.
#[derive(Debug, Clone)]
pub struct PkgSpec {
    /// Key in packages.toml / local.toml.
    pub logical: String,
    /// Name this manager knows it by.
    pub native: String,
    /// Pixi: the env table to write. Npm: the entry's `version`, if any.
    /// Script: `install` (and `remove`, when adopting an entry that has one).
    pub body: Option<Table>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub logical: String,
    pub result: std::result::Result<(), String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Install,
    Remove,
}

pub trait Adapter {
    fn id(&self) -> ManagerId;

    fn section(&self) -> Section {
        self.id().section()
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool;

    /// Whether install/remove need root (they get wrapped in sudo).
    fn needs_root(&self) -> bool;

    /// Whether an interactive run shows the commands and asks before running them even
    /// without sudo (root adapters already ask in the sudo step): a script entry runs
    /// arbitrary shell from the repo.
    fn needs_confirm(&self) -> bool {
        false
    }

    /// Whether `adopt init` should record what is installed now as the baseline.
    fn baseline_on_init(&self) -> bool {
        false
    }

    /// Files whose mtime changes whenever this manager's package set does. `adopt
    /// status --prompt` stats these instead of running the manager.
    fn watch_paths(&self, paths: &Paths) -> Vec<std::path::PathBuf>;

    /// Native names of everything the user asked for (not dependencies).
    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>>;

    /// Pixi: the env tables as currently written, for fingerprints. Others: empty.
    fn tables(&self, _cx: &Ctx) -> Result<BTreeMap<String, Table>> {
        Ok(BTreeMap::new())
    }

    /// The commands install/remove will run, for confirmation prompts and --dry-run.
    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd>;

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>>;

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>>;
}

/// The adapters usable on this machine: at most one system manager (apt, then dnf,
/// then brew, so Linuxbrew next to apt does not take over), then pixi, then npm, then
/// script (whose installers may need curl or node). The order is also the apply order.
/// `scripts` is the `[script.*]` entries of both manifests, since those are only known
/// from the manifest itself.
pub fn detect(
    runner: &dyn CommandRunner,
    paths: &Paths,
    scripts: &[crate::state::Entry],
) -> Vec<Box<dyn Adapter>> {
    let system: Vec<Box<dyn Adapter>> = vec![
        Box::new(apt::Apt),
        Box::new(dnf::Dnf::new()),
        Box::new(brew::Brew),
    ];
    let mut out: Vec<Box<dyn Adapter>> = system
        .into_iter()
        .filter(|a| a.is_available(runner))
        .take(1)
        .collect();
    let pixi = pixi::Pixi::new(paths.pixi_manifest.clone());
    if pixi.is_available(runner) {
        out.push(Box::new(pixi));
    }
    let npm = npm::Npm::new();
    if npm.is_available(runner) {
        out.push(Box::new(npm));
    }
    let script = script::Script::new(scripts.to_vec());
    if script.is_available(runner) {
        out.push(Box::new(script));
    }
    out
}

/// Run one command for all of `pkgs`; if it fails and there is more than one, retry
/// each on its own so a single unknown name only fails itself. `arg` turns a spec into
/// its command-line word (usually the native name); `root` wraps the commands in sudo.
pub fn batch_then_each(
    cx: &Ctx,
    pkgs: &[PkgSpec],
    arg: impl Fn(&PkgSpec) -> String,
    build: impl Fn(&[&str]) -> Cmd,
    root: bool,
) -> Result<Vec<Outcome>> {
    if pkgs.is_empty() {
        return Ok(Vec::new());
    }
    let prepare = |names: &[&str]| -> std::result::Result<Cmd, String> {
        let cmd = build(names);
        if root { cx.elevate(cmd) } else { Ok(cmd) }
    };
    let words: Vec<String> = pkgs.iter().map(&arg).collect();
    let all: Vec<&str> = words.iter().map(String::as_str).collect();
    let outcome = |p: &PkgSpec, r: std::result::Result<(), String>| Outcome {
        logical: p.logical.clone(),
        result: r,
    };
    let batch = match prepare(&all) {
        Ok(c) => c,
        Err(why) => return Ok(pkgs.iter().map(|p| outcome(p, Err(why.clone()))).collect()),
    };
    if cx.dry_run {
        cx.ui.would(batch.to_string());
        return Ok(pkgs.iter().map(|p| outcome(p, Ok(()))).collect());
    }
    let code = cx.runner.run_attached(&batch)?;
    if code == 0 {
        return Ok(pkgs.iter().map(|p| outcome(p, Ok(()))).collect());
    }
    if pkgs.len() == 1 {
        return Ok(vec![outcome(
            &pkgs[0],
            Err(format!("`{batch}` 退出码 {code}")),
        )]);
    }
    cx.ui
        .warn(format!("批量执行失败（退出码 {code}），逐个重试"));
    let mut out = Vec::new();
    for (p, word) in pkgs.iter().zip(&words) {
        let cmd = match prepare(&[word.as_str()]) {
            Ok(c) => c,
            Err(why) => {
                out.push(outcome(p, Err(why)));
                continue;
            }
        };
        let code = cx.runner.run_attached(&cmd)?;
        out.push(outcome(
            p,
            if code == 0 {
                Ok(())
            } else {
                Err(format!("`{cmd}` 退出码 {code}"))
            },
        ));
    }
    Ok(out)
}
