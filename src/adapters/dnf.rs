//! dnf (Fedora / RHEL). dnf4 and dnf5 take different repoquery flags, so the version
//! is probed once with `dnf --version`. Install/remove are the same for both.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::{Adapter, Ctx, Op, Outcome, PkgSpec};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnfVersion {
    V4,
    V5,
}

#[derive(Default)]
pub struct Dnf {
    version: OnceCell<DnfVersion>,
}

/// `dnf --version`: dnf5 starts with `dnf5 version 5.x`, dnf4 with the bare version.
pub fn parse_version(out: &str) -> DnfVersion {
    let first = out
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if first.starts_with("dnf5") || first.starts_with('5') {
        DnfVersion::V5
    } else {
        DnfVersion::V4
    }
}

/// One name per line. dnf5 can print repository-loading progress on stdout; those
/// lines contain spaces or end in `:`, which package names never do.
pub fn parse_userinstalled(out: &str) -> BTreeSet<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace) && !l.ends_with(':'))
        .map(str::to_string)
        .collect()
}

impl Dnf {
    pub fn new() -> Dnf {
        Dnf::default()
    }

    fn version(&self, runner: &dyn CommandRunner) -> DnfVersion {
        *self.version.get_or_init(|| {
            runner
                .run(&Cmd::new("dnf").arg("--version"))
                .ok()
                .filter(|o| o.success())
                .map_or(DnfVersion::V4, |o| parse_version(&o.stdout))
        })
    }

    pub fn list_cmd(version: DnfVersion) -> Cmd {
        let base = Cmd::new("dnf").args(["-q", "repoquery", "--userinstalled"]);
        match version {
            DnfVersion::V4 => base.args(["--qf", "%{name}"]),
            // dnf5 does not end each record with a newline by itself.
            DnfVersion::V5 => base.args(["--queryformat", "%{name}\n"]),
        }
    }

    fn dnf(op: Op, names: &[&str]) -> Cmd {
        let verb = match op {
            Op::Install => "install",
            Op::Remove => "remove",
        };
        Cmd::new("dnf")
            .args(["-y", verb])
            .args(names.iter().copied())
    }
}

impl Adapter for Dnf {
    fn id(&self) -> ManagerId {
        ManagerId::Dnf
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool {
        runner.which("dnf")
    }

    fn needs_root(&self) -> bool {
        true
    }

    fn baseline_on_init(&self) -> bool {
        true
    }

    fn watch_paths(&self, paths: &Paths) -> Vec<PathBuf> {
        vec![
            // The rpm database changes on every install/remove (new and old location).
            paths.sys("/usr/lib/sysimage/rpm/rpmdb.sqlite"),
            paths.sys("/var/lib/rpm/rpmdb.sqlite"),
            // dnf's own transaction history; `dnf mark` only touches these.
            paths.sys("/var/lib/dnf/history.sqlite"),
            paths.sys("/usr/lib/sysimage/libdnf5/transaction_history.sqlite"),
            paths.sys("/usr/lib/sysimage/libdnf5/system.toml"),
        ]
    }

    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>> {
        let cmd = Self::list_cmd(self.version(cx.runner));
        let out = cx.runner.run(&cmd)?;
        if !out.success() {
            bail!("`{cmd}` 失败：\n{}", out.tail());
        }
        Ok(parse_userinstalled(&out.stdout))
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd> {
        if pkgs.is_empty() {
            return Vec::new();
        }
        let names: Vec<&str> = pkgs.iter().map(|p| p.native.as_str()).collect();
        vec![Self::dnf(op, &names)]
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        super::batch_then_each(
            cx,
            pkgs,
            |p| p.native.clone(),
            |n| Self::dnf(Op::Install, n),
            true,
        )
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        super::batch_then_each(
            cx,
            pkgs,
            |p| p.native.clone(),
            |n| Self::dnf(Op::Remove, n),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dnf4_and_dnf5() {
        let v4 = include_str!("../../tests/fixtures/dnf4-version.txt");
        let v5 = include_str!("../../tests/fixtures/dnf5-version.txt");
        assert_eq!(parse_version(v4), DnfVersion::V4);
        assert_eq!(parse_version(v5), DnfVersion::V5);
    }

    #[test]
    fn parses_userinstalled_lists() {
        let v4 = parse_userinstalled(include_str!(
            "../../tests/fixtures/dnf4-repoquery-userinstalled.txt"
        ));
        assert_eq!(v4.len(), 12);
        assert!(v4.contains("vim-enhanced"));
        let v5 = parse_userinstalled(include_str!(
            "../../tests/fixtures/dnf5-repoquery-userinstalled.txt"
        ));
        assert_eq!(v5.len(), 11);
        assert!(v5.contains("git-core"));
    }

    #[test]
    fn ignores_progress_lines() {
        let out = "Updating and loading repositories:\n Fedora 41 - x86_64  100% | 12.3 MiB/s\nRepositories loaded.\nhtop\n";
        let names = parse_userinstalled(out);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["htop"]);
    }

    #[test]
    fn list_flags_differ_per_version() {
        assert_eq!(
            Dnf::list_cmd(DnfVersion::V4).to_string(),
            "dnf -q repoquery --userinstalled --qf '%{name}'"
        );
        assert_eq!(
            Dnf::list_cmd(DnfVersion::V5).to_string(),
            "dnf -q repoquery --userinstalled --queryformat '%{name}\n'"
        );
    }
}
