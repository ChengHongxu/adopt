//! Homebrew formulae (casks are not handled yet). Listing uses
//! `brew leaves --installed-on-request`, so a formula that only became a leaf because
//! whatever needed it was uninstalled is not mistaken for the user's choice.
//! Homebrew refuses to run as root, so nothing here goes through sudo.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::{Adapter, Ctx, Op, Outcome, PkgSpec};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};

pub struct Brew;

/// One formula per line; tap formulae keep their `user/tap/` prefix.
pub fn parse_leaves(out: &str) -> BTreeSet<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

impl Brew {
    fn brew(op: Op, names: &[&str]) -> Cmd {
        let verb = match op {
            Op::Install => "install",
            Op::Remove => "uninstall",
        };
        Cmd::new("brew")
            .args([verb, "--formula"])
            .args(names.iter().copied())
    }
}

impl Adapter for Brew {
    fn id(&self) -> ManagerId {
        ManagerId::Brew
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool {
        runner.which("brew")
    }

    fn needs_root(&self) -> bool {
        false
    }

    fn watch_paths(&self, paths: &Paths) -> Vec<PathBuf> {
        // Asking `brew --prefix` costs more than the whole status budget, so watch the
        // Cellar in each standard prefix; a keg appearing or going away changes its mtime.
        ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"]
            .iter()
            .map(|p| paths.sys(&format!("{p}/Cellar")))
            .collect()
    }

    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>> {
        let cmd = Cmd::new("brew").args(["leaves", "--installed-on-request"]);
        let out = cx.runner.run(&cmd)?;
        if !out.success() {
            bail!("`{cmd}` 失败：\n{}", out.tail());
        }
        Ok(parse_leaves(&out.stdout))
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd> {
        if pkgs.is_empty() {
            return Vec::new();
        }
        let names: Vec<&str> = pkgs.iter().map(|p| p.native.as_str()).collect();
        vec![Self::brew(op, &names)]
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        super::batch_then_each(cx, pkgs, |n| Self::brew(Op::Install, n), false)
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        super::batch_then_each(cx, pkgs, |n| Self::brew(Op::Remove, n), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leaves_including_tap_formulae() {
        let names = parse_leaves(include_str!("../../tests/fixtures/brew-leaves.txt"));
        assert_eq!(names.len(), 8);
        assert!(names.contains("ripgrep"));
        assert!(names.contains("hashicorp/tap/terraform"));
    }

    #[test]
    fn commands_are_formula_only() {
        let p = PkgSpec {
            logical: "htop".into(),
            native: "htop".into(),
            body: None,
        };
        let cmds = Brew.planned_commands(std::slice::from_ref(&p), Op::Remove);
        assert_eq!(cmds[0].to_string(), "brew uninstall --formula htop");
    }
}
