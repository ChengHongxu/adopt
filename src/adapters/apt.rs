//! apt (Debian / Ubuntu). Listing uses `apt-mark showmanual`; changes go through
//! `sudo apt-get`, batched, with a per-package retry so one bad name does not sink
//! the rest.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::{Adapter, Ctx, Op, Outcome, PkgSpec};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};

pub struct Apt;

/// Touched by the optional `DPkg::Post-Invoke` hook after every dpkg run.
pub const DPKG_STAMP: &str = "/var/lib/adopt/dpkg.stamp";
pub const HOOK_FILE: &str = "/etc/apt/apt.conf.d/99adopt";
pub const HOOK_TEXT: &str = "// Installed by adopt: lets `adopt status` notice package changes cheaply.\n\
DPkg::Post-Invoke { \"mkdir -p /var/lib/adopt && touch /var/lib/adopt/dpkg.stamp || true\"; };\n";

impl Apt {
    /// The root command that writes [`HOOK_FILE`].
    pub fn hook_install_cmd() -> Cmd {
        Cmd::new("sh").args([
            "-c".to_string(),
            format!("printf '%s' \"$1\" > {HOOK_FILE}"),
            "sh".to_string(),
            HOOK_TEXT.to_string(),
        ])
    }
}

impl Apt {
    fn apt_get(op: Op, names: &[&str]) -> Cmd {
        let verb = match op {
            Op::Install => "install",
            Op::Remove => "remove",
        };
        Cmd::new("apt-get")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .args(["-y", "-q", verb])
            .args(names.iter().copied())
    }

    fn update_cmd() -> Cmd {
        Cmd::new("apt-get").args(["-q", "update"])
    }

    fn change(&self, cx: &Ctx, pkgs: &[PkgSpec], op: Op) -> Result<Vec<Outcome>> {
        super::batch_then_each(
            cx,
            pkgs,
            |p| p.native.clone(),
            |names| Self::apt_get(op, names),
            true,
        )
    }
}

/// `apt-mark showmanual`: one package per line, foreign-arch ones as `name:arch`.
pub fn parse_showmanual(out: &str) -> BTreeSet<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

impl Adapter for Apt {
    fn id(&self) -> ManagerId {
        ManagerId::Apt
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool {
        runner.which("apt-get") && runner.which("apt-mark")
    }

    fn needs_root(&self) -> bool {
        true
    }

    fn baseline_on_init(&self) -> bool {
        true
    }

    fn watch_paths(&self, paths: &Paths) -> Vec<PathBuf> {
        vec![
            paths.sys(DPKG_STAMP),
            paths.sys("/var/lib/dpkg/status"),
            // `apt-mark manual/auto` only touches this one.
            paths.sys("/var/lib/apt/extended_states"),
        ]
    }

    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>> {
        let out = cx.runner.run(&Cmd::new("apt-mark").arg("showmanual"))?;
        if !out.success() {
            bail!("apt-mark showmanual 失败：\n{}", out.tail());
        }
        Ok(parse_showmanual(&out.stdout))
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd> {
        if pkgs.is_empty() {
            return Vec::new();
        }
        let names: Vec<&str> = pkgs.iter().map(|p| p.native.as_str()).collect();
        let mut cmds = Vec::new();
        if op == Op::Install {
            cmds.push(Self::update_cmd());
        }
        cmds.push(Self::apt_get(op, &names));
        cmds
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        if pkgs.is_empty() {
            return Ok(Vec::new());
        }
        // A stale index (fresh cloud images) makes every install fail. Failing to
        // refresh is only a warning; without root, the install reports why below.
        if let Ok(cmd) = cx.elevate(Self::update_cmd()) {
            if cx.dry_run {
                cx.ui.would(cmd.to_string());
            } else {
                let code = cx.runner.run_attached(&cmd)?;
                if code != 0 {
                    cx.ui.warn(format!("`{cmd}` 退出码 {code}，继续安装"));
                }
            }
        }
        self.change(cx, pkgs, Op::Install)
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        self.change(cx, pkgs, Op::Remove)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_showmanual_output() {
        // Captured from an Ubuntu 25.10 WSL machine.
        let out = include_str!("../../tests/fixtures/apt-mark-showmanual.txt");
        let names = parse_showmanual(out);
        assert_eq!(names.len(), 56);
        assert!(names.contains("htop"));
        assert!(names.contains("python3.13-minimal"));
        assert!(names.iter().all(|n| !n.contains(char::is_whitespace)));
    }

    #[test]
    fn keeps_arch_qualifier() {
        let names = parse_showmanual("libc6:i386\nzsh\n\n");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["libc6:i386", "zsh"]);
    }

    #[test]
    fn install_command_is_noninteractive() {
        let p = |n: &str| PkgSpec {
            logical: n.into(),
            native: n.into(),
            body: None,
        };
        let cmds = Apt.planned_commands(&[p("htop"), p("tmux")], Op::Install);
        assert_eq!(cmds[0].to_string(), "apt-get -q update");
        assert_eq!(
            cmds[1].to_string(),
            "DEBIAN_FRONTEND=noninteractive apt-get -y -q install htop tmux"
        );
    }
}
