//! One adapter per package manager. Adding uv / npm -g / cargo install means a new
//! `Section`, a new `ManagerId`, and one more impl of [`Adapter`].

pub mod pixi;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use toml_edit::Table;

use crate::model::{ManagerId, Section};
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};
use crate::ui::Ui;

pub struct Ctx<'a> {
    pub runner: &'a dyn CommandRunner,
    pub paths: &'a Paths,
    pub ui: &'a Ui,
    pub dry_run: bool,
}

/// What to install or remove.
#[derive(Debug, Clone)]
pub struct PkgSpec {
    /// Key in packages.toml / local.toml.
    pub logical: String,
    /// Name this manager knows it by.
    pub native: String,
    /// Pixi only: the env table to write.
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

    /// Whether `adopt init` should record what is installed now as the baseline.
    fn baseline_on_init(&self) -> bool {
        false
    }

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

/// The adapters usable on this machine: at most one system manager plus pixi.
pub fn detect(runner: &dyn CommandRunner, paths: &Paths) -> Vec<Box<dyn Adapter>> {
    let candidates: Vec<Box<dyn Adapter>> =
        vec![Box::new(pixi::Pixi::new(paths.pixi_manifest.clone()))];
    candidates
        .into_iter()
        .filter(|a| a.is_available(runner))
        .collect()
}
