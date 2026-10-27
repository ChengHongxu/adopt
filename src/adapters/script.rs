//! Software installed by a command from the manifest (`[script.*]`), for tools that
//! only ship a `curl … | sh` installer.
//!
//! adopt never guesses whether such software is installed: the entry names the path it
//! creates, and one `stat` answers it. That is also how `status --prompt` counts
//! without starting anything. Nothing here goes through sudo.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{Adapter, Ctx, Op, Outcome, PkgSpec};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};
use crate::state::entry::{self, Entry};

pub struct Script {
    /// `[script.*]` from packages.toml and local.toml; local wins when both name one.
    entries: Vec<Entry>,
}

impl Script {
    pub fn new(entries: Vec<Entry>) -> Script {
        Script { entries }
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().rev().find(|e| e.name == name)
    }

    /// Where `entry` says its software lands. Commands run with `cwd = $HOME`, so a
    /// relative `creates` is relative to `$HOME` too.
    pub fn creates_path(entry: &Entry, paths: &Paths) -> Option<PathBuf> {
        let raw = entry.creates()?;
        let p = paths.expand(raw);
        Some(if p.is_absolute() {
            p
        } else {
            paths.home.join(p)
        })
    }

    fn exists(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    /// `sh -c <script>`, in `$HOME`, with adopt's environment and without sudo.
    fn run_cmd(script: &str, paths: &Paths) -> Cmd {
        Cmd::new("sh").arg("-c").arg(script).cwd(paths.home.clone())
    }

    fn body_cmd(spec: &PkgSpec, key: &str) -> Option<String> {
        let cmd = spec.body.as_ref()?.get(key)?.as_str()?;
        Some(cmd.to_string())
    }
}

impl Adapter for Script {
    fn id(&self) -> ManagerId {
        ManagerId::Script
    }

    fn is_available(&self, _runner: &dyn CommandRunner) -> bool {
        // POSIX `sh` is always there.
        true
    }

    fn needs_root(&self) -> bool {
        false
    }

    fn needs_confirm(&self) -> bool {
        // The command comes from the repo and runs on every machine: show it first.
        true
    }

    /// The `creates` paths: present or absent is exactly the state the count depends on.
    fn watch_paths(&self, paths: &Paths) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .entries
            .iter()
            .filter_map(|e| Self::creates_path(e, paths))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn list_user_installed(&self, cx: &Ctx) -> Result<BTreeSet<String>> {
        Ok(self
            .entries
            .iter()
            .filter(|e| Self::creates_path(e, cx.paths).is_some_and(|p| Self::exists(&p)))
            .map(|e| e.name.clone())
            .collect())
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], op: Op) -> Vec<Cmd> {
        let key = match op {
            Op::Install => entry::INSTALL,
            Op::Remove => entry::REMOVE,
        };
        // Only for display; the working directory does not show in `Display`.
        pkgs.iter()
            .filter_map(|p| Self::body_cmd(p, key))
            .map(|s| Cmd::new("sh").arg("-c").arg(s))
            .collect()
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        let mut out = Vec::new();
        for p in pkgs {
            out.push(Outcome {
                logical: p.logical.clone(),
                result: self.run_one(cx, p, entry::INSTALL),
            });
        }
        Ok(out)
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        let mut out = Vec::new();
        for p in pkgs {
            out.push(Outcome {
                logical: p.logical.clone(),
                result: self.run_one(cx, p, entry::REMOVE),
            });
        }
        Ok(out)
    }
}

impl Script {
    /// Run one entry's command and, for an install, check that `creates` appeared.
    fn run_one(&self, cx: &Ctx, p: &PkgSpec, key: &str) -> std::result::Result<(), String> {
        let Some(script) = Self::body_cmd(p, key) else {
            return Err(format!("{}(script) 的条目没有写 {key}", p.logical));
        };
        let cmd = Self::run_cmd(&script, cx.paths);
        if cx.dry_run {
            cx.ui.would(cmd.to_string());
            return Ok(());
        }
        let code = cx
            .runner
            .run_attached(&cmd)
            .map_err(|e| format!("`{cmd}` 无法执行：{e}"))?;
        if code != 0 {
            return Err(format!("`{cmd}` 退出码 {code}"));
        }
        if key == entry::INSTALL
            && let Some(creates) = self
                .entry(&p.logical)
                .or_else(|| self.entry(&p.native))
                .and_then(|e| Self::creates_path(e, cx.paths))
            && !Self::exists(&creates)
        {
            return Err(format!(
                "命令成功退出，但 {} 不存在，检查条目里的 creates 是否写错",
                cx.paths.tilde(&creates)
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::sandbox(dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        (dir, paths)
    }

    fn entry_with(name: &str, creates: &str, install: &str, remove: Option<&str>) -> Entry {
        let mut e = Entry::script(name);
        let body = e.body.as_mut().unwrap();
        body.insert(entry::CREATES, creates.into());
        body.insert(entry::INSTALL, install.into());
        if let Some(r) = remove {
            body.insert(entry::REMOVE, r.into());
        }
        e
    }

    #[test]
    fn creates_is_expanded_against_home() {
        let (_d, paths) = paths();
        for raw in [
            "~/.local/bin/x",
            "$HOME/.local/bin/x",
            "${HOME}/.local/bin/x",
        ] {
            let e = entry_with("x", raw, "true", None);
            assert_eq!(
                Script::creates_path(&e, &paths),
                Some(paths.home.join(".local/bin/x")),
                "{raw}"
            );
        }
        let e = entry_with("x", ".local/bin/x", "true", None);
        assert_eq!(
            Script::creates_path(&e, &paths),
            Some(paths.home.join(".local/bin/x"))
        );
    }

    #[test]
    fn lists_only_entries_whose_creates_exists() {
        let (_d, paths) = paths();
        let here = paths.home.join(".local/bin/here");
        std::fs::create_dir_all(here.parent().unwrap()).unwrap();
        std::fs::write(&here, "").unwrap();
        let s = Script::new(vec![
            entry_with("here", "~/.local/bin/here", "true", None),
            entry_with("gone", "~/.local/bin/gone", "true", None),
        ]);
        let (ui, _buf) = Ui::capture();
        let cx = Ctx {
            runner: &crate::runner::SystemRunner,
            paths: &paths,
            ui: &ui,
            dry_run: true,
            interactive: false,
            privilege: &Default::default(),
        };
        assert_eq!(
            s.list_user_installed(&cx)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["here"]
        );
        assert_eq!(s.watch_paths(&paths).len(), 2);
    }

    #[test]
    fn planned_commands_use_sh_c() {
        let mut spec = PkgSpec {
            logical: "magpie".into(),
            native: "magpie".into(),
            body: Some(toml_edit::Table::new()),
        };
        spec.body
            .as_mut()
            .unwrap()
            .insert(entry::INSTALL, "curl -fsSL https://x | sh".into());
        let cmds = Script::new(vec![]).planned_commands(&[spec], Op::Install);
        assert_eq!(cmds[0].to_string(), "sh -c 'curl -fsSL https://x | sh'");
    }
}
