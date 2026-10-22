//! `adopt` with no subcommand: pull, then walk through every difference and let the
//! user decide, then commit and push the shared manifest.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use console::measure_text_width;

use crate::App;
use crate::commit_msg::{self, Change};
use crate::git::{Git, Pull, Push};
use crate::model::{ManagerId, Section};
use crate::plan::{self, Inventory, NewItem};
use crate::state::{Applied, Entry, Local, Shared, fingerprint, shared};

/// Mutable state for one interactive run. Nothing is written until [`Session::finish`],
/// so Ctrl-C at any prompt leaves every file untouched.
pub struct Session<'a> {
    pub app: &'a App<'a>,
    pub repo: PathBuf,
    pub local: Local,
    pub shared: Shared,
    /// Goes into the commit message.
    pub changes: Vec<Change>,
    /// Decisions that only touched local.toml.
    pub local_changes: usize,
}

impl<'a> Session<'a> {
    pub fn open(app: &'a App<'a>) -> Result<Session<'a>> {
        let (local, repo) = app.open()?;
        let shared = app.load_shared(&repo)?;
        Ok(Session {
            app,
            repo,
            local,
            shared,
            changes: Vec::new(),
            local_changes: 0,
        })
    }

    pub fn reload_shared(&mut self) -> Result<()> {
        self.shared = self.app.load_shared(&self.repo)?;
        Ok(())
    }

    /// Save local.toml, and commit + push packages.toml if it changed.
    pub fn finish(mut self, extra_paths: &[String]) -> Result<()> {
        let ui = self.app.ui;
        let dry = self.app.dry_run;
        if self.local.file.is_dirty() {
            if dry {
                ui.would(format!(
                    "写入 {}",
                    self.app.paths.tilde(&self.local.file.path)
                ));
            } else {
                self.local.file.save()?;
            }
        }
        let mut paths: Vec<&str> = extra_paths.iter().map(String::as_str).collect();
        if self.shared.file.is_dirty() {
            if dry {
                ui.would(format!(
                    "写入 {}",
                    self.app.paths.tilde(&self.shared.file.path)
                ));
            } else {
                self.shared.file.save()?;
            }
            paths.push(shared::FILE_NAME);
        }
        if self.changes.is_empty() || paths.is_empty() {
            if self.local_changes > 0 {
                ui.ok(format!(
                    "已更新本机状态（{} 项，不进 git）",
                    self.local_changes
                ));
            } else {
                ui.ok("没有需要提交的改动");
            }
            return Ok(());
        }
        let message = commit_msg::message(&self.app.host(), &self.changes);
        let summary = commit_msg::summary(&self.changes);
        commit_and_push(self.app, &self.repo, &paths, &message, &summary)
    }
}

pub fn commit_and_push(
    app: &App,
    repo: &Path,
    paths: &[&str],
    message: &str,
    summary: &str,
) -> Result<()> {
    let ui = app.ui;
    if app.dry_run {
        ui.would(format!(
            "git commit -m {:?} -- {}",
            message.lines().next().unwrap_or_default(),
            paths.join(" ")
        ));
        ui.would("git push");
        return Ok(());
    }
    let git = Git::new(app.runner, repo);
    if !git.commit(paths, message)? {
        ui.ok("没有需要提交的改动");
        return Ok(());
    }
    match git.push()? {
        Push::Pushed | Push::PushedAfterRebase => ui.ok(format!("已提交并推送：{summary}")),
        Push::NoRemote => ui.warn(format!("已提交（仓库没有 remote，未推送）：{summary}")),
    }
    Ok(())
}

/// Pull the dotfiles repo and report what came in. Dry-run skips the pull.
pub fn pull(app: &App, repo: &Path) -> Result<Vec<String>> {
    if app.dry_run {
        app.ui.would("git pull --rebase --autostash");
        return Ok(Vec::new());
    }
    match Git::new(app.runner, repo).pull()? {
        Pull::NoUpstream => Ok(Vec::new()),
        Pull::Pulled(subjects) => Ok(subjects),
    }
}

pub fn run(app: &App) -> Result<()> {
    if !app.interactive {
        bail!("交互模式需要终端；非交互场景请用 `adopt sync --yes`");
    }
    let mut s = Session::open(app)?;
    let pulled = pull(app, &s.repo)?;
    if !pulled.is_empty() {
        app.ui.line(format!("↓ 拉取 {} 个提交", pulled.len()));
        s.reload_shared()?;
    }

    let adapters = app.adapters();
    let inv = Inventory::collect(&adapters, &app.ctx())?;
    let new = plan::find_new(&s.shared, &s.local, &inv)?;
    claim_new(&mut s, &new)?;
    s.finish(&[])
}

// ---- new packages ----

const NEW_KEYS: [char; 5] = ['a', 'g', 'l', 'i', 's'];
const NEW_LEGEND: &str = "[a]所有机器 [g]指定分组 [l]仅本机 [i]忽略 [s]稍后";

fn label(manager: ManagerId, name: &str, width: usize) -> String {
    let pad = width.saturating_sub(measure_text_width(name));
    format!("  {:<5} {name}{} ", manager.key(), " ".repeat(pad))
}

fn name_width<'n>(names: impl Iterator<Item = &'n str>) -> usize {
    names.map(measure_text_width).max().unwrap_or(0).max(10) + 1
}

pub fn claim_new(s: &mut Session, items: &[NewItem]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ui = s.app.ui;
    ui.line(format!("本机新装了 {} 个软件：", items.len()));
    let width = name_width(items.iter().map(|i| i.native.as_str()));
    for (idx, item) in items.iter().enumerate() {
        let legend = if idx == 0 {
            format!("{NEW_LEGEND} > ")
        } else {
            "> ".into()
        };
        let key = s.app.prompter.key(
            &format!("{}{legend}", label(item.manager, &item.native, width)),
            &NEW_KEYS,
        )?;
        match key {
            'a' => adopt_shared(s, item, None)?,
            'g' => {
                let groups = ask_groups(s)?;
                adopt_shared(s, item, (!groups.is_empty()).then_some(groups))?;
            }
            'l' => {
                s.local.upsert_entry(&entry_for(item));
                mark_applied(&mut s.local, item);
                s.local_changes += 1;
            }
            'i' => {
                s.local.add_ignore(item.manager, &item.native);
                s.local_changes += 1;
            }
            _ => {}
        }
    }
    Ok(())
}

fn entry_for(item: &NewItem) -> Entry {
    match item.section() {
        Section::Pixi => Entry::pixi(&item.native, item.body.clone().unwrap_or_default()),
        Section::System => Entry::system(&item.native),
    }
}

fn mark_applied(local: &mut Local, item: &NewItem) {
    let applied = Applied {
        manager: item.manager,
        native: item.native.clone(),
        by_adopt: false,
        fp: item.body.as_ref().map(fingerprint),
    };
    local.set_applied(item.section(), &item.native, &applied);
}

fn adopt_shared(s: &mut Session, item: &NewItem, groups: Option<Vec<String>>) -> Result<()> {
    let tag = match &groups {
        Some(gs) => format!("{}@{}", item.manager, gs.join(",")),
        None => item.manager.to_string(),
    };
    s.shared.upsert(&entry_for(item).with_groups(groups));
    mark_applied(&mut s.local, item);
    s.changes.push(Change::Add {
        name: item.native.clone(),
        tag,
    });
    Ok(())
}

/// Comma-separated group names, defaulting to this machine's groups.
pub fn ask_groups(s: &Session) -> Result<Vec<String>> {
    let mut known = s.shared.groups();
    known.extend(s.local.groups());
    let hint = if known.is_empty() {
        String::new()
    } else {
        format!(
            "（已有：{}）",
            known.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    let answer = s.app.prompter.input(
        &format!("    分组，逗号分隔{hint}"),
        &s.local.groups().join(","),
    )?;
    Ok(parse_groups(&answer))
}

pub fn parse_groups(s: &str) -> Vec<String> {
    let mut out: Vec<String> = s
        .split([',', ' ', '，'])
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}
