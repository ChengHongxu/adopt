//! adopt: install packages with native tools, then adopt them into a shared manifest.

pub mod adapters;
pub mod claim;
pub mod cli;
pub mod commit_msg;
pub mod git;
pub mod init;
pub mod model;
pub mod paths;
pub mod plan;
pub mod privilege;
pub mod prompt;
pub mod runner;
pub mod state;
pub mod ui;

use anyhow::{Result, bail};

use crate::adapters::{Adapter, Ctx};
use crate::cli::{Cli, Command};
use crate::paths::Paths;
use crate::privilege::PrivilegeCell;
use crate::prompt::Prompter;
use crate::runner::{Cmd, CommandRunner};
use crate::state::{Local, Shared};
use crate::ui::Ui;

/// Everything a command needs from the outside world, injected so tests can fake it.
pub struct App<'a> {
    pub paths: Paths,
    pub runner: &'a dyn CommandRunner,
    pub prompter: &'a dyn Prompter,
    pub ui: &'a Ui,
    pub dry_run: bool,
    /// Whether a human is at the terminal.
    pub interactive: bool,
    /// Overrides `uname -n` (tests).
    pub hostname: Option<String>,
    pub privilege: PrivilegeCell,
}

impl App<'_> {
    pub fn run(&self, cli: &Cli) -> Result<()> {
        match &cli.command {
            None => claim::run(self),
            Some(Command::Init(args)) => init::run(self, args),
        }
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx {
            runner: self.runner,
            paths: &self.paths,
            ui: self.ui,
            dry_run: self.dry_run,
            interactive: self.interactive,
            privilege: &self.privilege,
        }
    }

    pub fn adapters(&self) -> Vec<Box<dyn Adapter>> {
        adapters::detect(self.runner, &self.paths)
    }

    pub fn host(&self) -> String {
        if let Some(h) = &self.hostname {
            return h.clone();
        }
        self.runner
            .run(&Cmd::new("uname").arg("-n"))
            .ok()
            .filter(|o| o.success())
            .map(|o| {
                o.stdout
                    .trim()
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "unknown".into())
    }

    /// local.toml plus the shared manifest it points at. Errors if `adopt init` never ran.
    pub fn open(&self) -> Result<(Local, std::path::PathBuf)> {
        let local = Local::load(&self.paths)?;
        let Some(repo) = local.repo().filter(|_| local.file.exists()) else {
            bail!("还没有初始化，请先运行 `adopt init`");
        };
        if !repo.is_dir() {
            bail!(
                "dotfiles 仓库 {} 不存在（local.toml 里的 repo），可以重新运行 `adopt init`",
                repo.display()
            );
        }
        Ok((local, repo))
    }

    pub fn load_shared(&self, repo: &std::path::Path) -> Result<Shared> {
        Shared::load(repo)
    }
}
