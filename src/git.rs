//! The dotfiles repo, driven through the git CLI. Never force-pushes.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::runner::{Cmd, CommandRunner, Output};

pub struct Git<'a> {
    runner: &'a dyn CommandRunner,
    pub repo: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pull {
    /// No remote or no upstream branch: nothing to pull from.
    NoUpstream,
    /// Subjects of the commits that came in, oldest first.
    Pulled(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Push {
    Pushed,
    /// Rejected, rebased onto the remote, then pushed.
    PushedAfterRebase,
    NoRemote,
}

impl<'a> Git<'a> {
    pub fn new(runner: &'a dyn CommandRunner, repo: &Path) -> Git<'a> {
        Git {
            runner,
            repo: repo.to_path_buf(),
        }
    }

    fn cmd(&self) -> Cmd {
        Cmd::new("git")
            .arg("-C")
            .arg(self.repo.display().to_string())
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        self.runner.run(&self.cmd().args(args.iter().copied()))
    }

    fn run_ok(&self, args: &[&str]) -> Result<String> {
        let out = self.run(args)?;
        if !out.success() {
            bail!("git {} 失败：\n{}", args.join(" "), out.tail());
        }
        Ok(out.stdout)
    }

    pub fn is_repo(&self) -> bool {
        self.run(&["rev-parse", "--is-inside-work-tree"])
            .is_ok_and(|o| o.success() && o.stdout.trim() == "true")
    }

    pub fn toplevel(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(
            self.run_ok(&["rev-parse", "--show-toplevel"])?.trim(),
        ))
    }

    fn head(&self) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", "HEAD"])
            .ok()
            .filter(Output::success)
            .map(|o| o.stdout.trim().to_string())
    }

    fn has_upstream(&self) -> bool {
        self.run(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .is_ok_and(|o| o.success())
    }

    fn has_remote(&self) -> bool {
        self.run(&["remote"])
            .is_ok_and(|o| o.success() && !o.stdout.trim().is_empty())
    }

    fn rebase_in_progress(&self) -> bool {
        ["rebase-merge", "rebase-apply"].iter().any(|d| {
            self.run(&["rev-parse", "--git-path", d])
                .is_ok_and(|o| o.success() && self.repo.join(o.stdout.trim()).exists())
        })
    }

    /// `git pull --rebase --autostash`. A conflicted rebase is aborted so the repo is
    /// left exactly as it was, and the error says what to do by hand.
    pub fn pull(&self) -> Result<Pull> {
        if !self.has_upstream() {
            return Ok(Pull::NoUpstream);
        }
        let before = self.head();
        let out = self.run(&["pull", "--rebase", "--autostash", "--quiet"])?;
        if !out.success() {
            let aborted = self.rebase_in_progress()
                && self.run(&["rebase", "--abort"]).is_ok_and(|o| o.success());
            let hint = if aborted {
                "（已 rebase --abort，仓库保持原样）"
            } else {
                ""
            };
            bail!(
                "git pull --rebase 失败{hint}，请在 {} 里手动处理：\n{}",
                self.repo.display(),
                out.tail()
            );
        }
        let subjects = match before {
            Some(b) => self.run_ok(&["log", "--reverse", "--format=%s", &format!("{b}..HEAD")])?,
            None => self.run_ok(&["log", "--reverse", "--format=%s"])?,
        };
        Ok(Pull::Pulled(subjects.lines().map(str::to_string).collect()))
    }

    /// Files under `rel` that differ from HEAD (staged, unstaged or untracked).
    pub fn is_dirty(&self, rel: &str) -> Result<bool> {
        Ok(!self
            .run_ok(&["status", "--porcelain", "--", rel])?
            .trim()
            .is_empty())
    }

    /// Commit exactly `paths` (relative to the repo), leaving anything else the user
    /// has staged alone. Returns false when there was nothing to commit.
    pub fn commit(&self, paths: &[&str], message: &str) -> Result<bool> {
        let mut add = vec!["add", "--"];
        add.extend_from_slice(paths);
        self.run_ok(&add)?;
        let mut diff = vec!["diff", "--cached", "--quiet", "--"];
        diff.extend_from_slice(paths);
        if self.run(&diff)?.success() {
            return Ok(false);
        }
        let mut commit = vec!["commit", "--quiet", "-m", message, "--"];
        commit.extend_from_slice(paths);
        self.run_ok(&commit)?;
        Ok(true)
    }

    /// Push; if rejected, `pull --rebase` once and retry. Never `--force`.
    pub fn push(&self) -> Result<Push> {
        if !self.has_remote() {
            return Ok(Push::NoRemote);
        }
        let first = if self.has_upstream() {
            self.run(&["push", "--quiet"])?
        } else {
            self.run(&["push", "--quiet", "-u", "origin", "HEAD"])?
        };
        if first.success() {
            return Ok(Push::Pushed);
        }
        if !is_rejection(&first) {
            bail!("git push 失败：\n{}", first.tail());
        }
        self.pull().map_err(|e| {
            e.context("push 被拒，pull --rebase 也失败了；提交保留在本地，没有强推")
        })?;
        let second = self.run(&["push", "--quiet"])?;
        if !second.success() {
            bail!(
                "push 在 rebase 之后仍被拒，提交保留在本地，没有强推：\n{}",
                second.tail()
            );
        }
        Ok(Push::PushedAfterRebase)
    }
}

fn is_rejection(out: &Output) -> bool {
    let e = &out.stderr;
    e.contains("[rejected]") || e.contains("non-fast-forward") || e.contains("fetch first")
}
