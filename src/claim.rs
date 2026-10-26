//! `adopt` with no subcommand: pull, then walk through every difference and let the
//! user decide, then commit and push the shared manifest.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use console::measure_text_width;

use toml_edit::Table;

use crate::App;
use crate::adapters::Adapter;
use crate::apply::{self, RootPolicy};
use crate::commit_msg::{self, Change};
use crate::git::{Git, Pull, Push};
use crate::model::{ManagerId, Section};
use crate::plan::{self, Action, Inventory, NewItem, Origin, Pending, Plan, Want};
use crate::state::{Applied, Entry, Local, Mapping, Shared, fingerprint, shared};

/// Mutable state for one interactive run. Decisions are only written by
/// [`Session::finish`], so Ctrl-C at a prompt discards them; what was already
/// installed or removed before the prompts is recorded right away.
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
    let chezmoi_src = crate::chezmoi::detect(app, &s.repo);
    let pulled = pull(app, &s.repo)?;
    if !pulled.is_empty() {
        app.ui.line(format!("↓ 拉取 {} 个提交", pulled.len()));
        s.reload_shared()?;
        if chezmoi_src.is_some() {
            crate::chezmoi::apply(app);
        }
    }

    // First bring the machine in line with the manifests (asking before sudo)...
    let adapters = app.adapters();
    let inv = Inventory::collect(&adapters, &app.ctx())?;
    let plan = plan::reconcile(&s.shared, &s.local, &inv)?;
    let (adapters, report) = apply::apply_and_follow_up(
        app,
        &s.shared,
        &mut s.local,
        adapters,
        &plan,
        RootPolicy::Confirm,
    )?;
    if let Some(line) = apply::summary(&report) {
        app.ui.ok(line);
    }
    // ...and persist that right away: those packages really are installed now.
    if !app.dry_run && s.local.file.is_dirty() {
        s.local.file.save()?;
    }

    // ...then ask about everything that needs a decision. A dry run installed
    // nothing, so it keeps using the first snapshot.
    let (pending, new) = if app.dry_run {
        (plan.pending, plan::find_new(&s.shared, &s.local, &inv)?)
    } else {
        let inv = Inventory::collect(&adapters, &app.ctx())?;
        let plan = plan::reconcile(&s.shared, &s.local, &inv)?;
        (plan.pending, plan::find_new(&s.shared, &s.local, &inv)?)
    };
    let (mut failed, mut missing, mut drift) = (Vec::new(), Vec::new(), Vec::new());
    for p in pending {
        match p {
            Pending::Failed {
                manager,
                want,
                error,
            } => failed.push((manager, want, error)),
            Pending::Missing { manager, want } => missing.push((manager, want)),
            Pending::Drift {
                manager,
                want,
                current,
            } => drift.push((manager, want, current)),
        }
    }
    claim_failed(&mut s, &adapters, &failed)?;
    claim_new(&mut s, &new)?;
    claim_missing(&mut s, &adapters, &missing)?;
    claim_drift(&mut s, &adapters, &drift)?;
    let mut config_paths = Vec::new();
    if let Some(src) = &chezmoi_src {
        crate::chezmoi::claim(&mut s, src, &mut config_paths)?;
    }
    s.finish(&config_paths)?;
    crate::status::refresh(app);
    Ok(())
}

/// Install/update one package right now, asking before sudo.
fn apply_one(
    s: &mut Session,
    adapters: &[Box<dyn Adapter>],
    manager: ManagerId,
    action: Action,
) -> Result<()> {
    let plan = Plan {
        actions: vec![(manager, action)],
        pending: Vec::new(),
    };
    let report = apply::apply(s.app, &mut s.local, adapters, &plan, RootPolicy::Confirm)?;
    if let Some(line) = apply::summary(&report) {
        s.app.ui.ok(line);
    }
    Ok(())
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
                if groups.is_empty() {
                    ui.line("    没有选分组，按所有机器处理");
                }
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
        // Claimed unpinned: the user decides later whether a version is worth pinning.
        Section::Npm => Entry::npm(&item.native),
    }
}

fn mark_applied(local: &mut Local, item: &NewItem) {
    let applied = Applied {
        manager: item.manager,
        native: item.native.clone(),
        by_adopt: false,
        fp: entry_for(item).body.as_ref().map(fingerprint),
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

const NEW_GROUP: &str = "+ 新分组…";

/// Pick from the groups already in use (this machine's are pre-checked), or type new
/// ones. With no groups anywhere yet, just ask for names.
pub fn ask_groups(s: &Session) -> Result<Vec<String>> {
    let mine = s.local.groups();
    let mut known = s.shared.groups();
    known.extend(mine.iter().cloned());
    let known: Vec<String> = known.into_iter().collect();
    let prompter = s.app.prompter;
    if known.is_empty() {
        let answer = prompter.input("    分组，逗号分隔，例如 work,homelab", "")?;
        return Ok(parse_groups(&answer));
    }
    let mut items = known.clone();
    items.push(NEW_GROUP.into());
    let defaults: Vec<bool> = items.iter().map(|g| mine.contains(g)).collect();
    let picked = prompter.multi_select("    选择分组（空格勾选，回车确认）", &items, &defaults)?;
    let mut out: Vec<String> = picked
        .iter()
        .filter_map(|&i| known.get(i).cloned())
        .collect();
    if picked.contains(&known.len()) {
        out.extend(parse_groups(&prompter.input("    新分组，逗号分隔", "")?));
    }
    out.sort();
    out.dedup();
    Ok(out)
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

// ---- installs that failed ----

const RETRY_KEYS: [char; 3] = ['u', 'k', 's'];
const RETRY_LEGEND: &str = "[u]重试 [k]本机例外 [s]稍后";

pub fn claim_failed(
    s: &mut Session,
    adapters: &[Box<dyn Adapter>],
    items: &[(ManagerId, Want, String)],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ui = s.app.ui;
    ui.line(format!("有 {} 个软件安装失败：", items.len()));
    let width = name_width(items.iter().map(|(_, w, _)| w.native.as_str()));
    for (manager, want, error) in items {
        let (manager, section) = (*manager, manager.section());
        let reason = error.lines().last().unwrap_or_default();
        ui.line(format!(
            "{}{}",
            label(manager, &want.native, width),
            ui.dim(reason)
        ));
        if section == Section::System {
            // Usually the name differs on this platform: ask once, write it back.
            let answer = s.app.prompter.input(
                &format!("    {manager} 上的正确包名（留空跳过，- 表示 {manager} 不装）"),
                "",
            )?;
            match answer.trim() {
                "" => {}
                "-" => {
                    remap(s, want, manager, Mapping::Skip)?;
                    s.local.clear_failed(section, &want.logical);
                }
                name => {
                    remap(s, want, manager, Mapping::Name(name.to_string()))?;
                    s.local.clear_failed(section, &want.logical);
                    let renamed = Want {
                        native: name.to_string(),
                        ..want.clone()
                    };
                    apply_one(s, adapters, manager, Action::Install(renamed))?;
                }
            }
            continue;
        }
        let key = s
            .app
            .prompter
            .key(&format!("    {RETRY_LEGEND} > "), &RETRY_KEYS)?;
        match key {
            'u' => {
                s.local.clear_failed(section, &want.logical);
                apply_one(s, adapters, manager, Action::Install(want.clone()))?;
            }
            'k' => {
                opt_out(s, section, want);
                s.local.clear_failed(section, &want.logical);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Record a per-manager name (or `false`) on the entry, wherever it lives.
fn remap(s: &mut Session, want: &Want, manager: ManagerId, mapping: Mapping) -> Result<()> {
    match want.origin {
        Origin::Shared => {
            s.shared.set_mapping(&want.logical, manager, &mapping)?;
            let tag = match &mapping {
                Mapping::Name(n) => format!("{manager}={n}"),
                Mapping::Skip => format!("{manager}=false"),
            };
            s.changes.push(Change::Update {
                name: want.logical.clone(),
                tag,
            });
        }
        Origin::Local => {
            let entry = s
                .local
                .entries(Section::System)?
                .into_iter()
                .find(|e| e.name == want.logical);
            if let Some(mut e) = entry {
                e.mappings.insert(manager, mapping);
                s.local.upsert_entry(&e);
                s.local_changes += 1;
            }
        }
    }
    Ok(())
}

/// This machine stops wanting `want`: an exception for shared entries, deletion for local ones.
fn opt_out(s: &mut Session, section: Section, want: &Want) {
    match want.origin {
        Origin::Shared => {
            s.local.add_exception(section, &want.logical);
        }
        Origin::Local => {
            s.local.remove_entry(section, &want.logical);
        }
    }
    s.local.remove_applied(section, &want.logical);
    s.local_changes += 1;
}

// ---- packages the user uninstalled ----

const MISSING_KEYS: [char; 4] = ['r', 'k', 'u', 's'];
const MISSING_LEGEND: &str = "[r]从所有机器移除 [k]本机例外 [u]重新装上 [s]稍后";

pub fn claim_missing(
    s: &mut Session,
    adapters: &[Box<dyn Adapter>],
    items: &[(ManagerId, Want)],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    s.app
        .ui
        .line(format!("本机缺少清单里的 {} 个软件：", items.len()));
    let width = name_width(items.iter().map(|(_, w)| w.native.as_str()));
    for (idx, (manager, want)) in items.iter().enumerate() {
        let (manager, section) = (*manager, manager.section());
        let legend = if idx == 0 {
            format!("{MISSING_LEGEND} > ")
        } else {
            "> ".into()
        };
        let key = s.app.prompter.key(
            &format!("{}{legend}", label(manager, &want.native, width)),
            &MISSING_KEYS,
        )?;
        match key {
            'r' => {
                match want.origin {
                    Origin::Shared => {
                        s.shared.remove(section, &want.logical);
                        s.changes.push(Change::Remove {
                            name: want.logical.clone(),
                            tag: manager.to_string(),
                        });
                    }
                    Origin::Local => {
                        s.local.remove_entry(section, &want.logical);
                        s.local_changes += 1;
                    }
                }
                s.local.remove_applied(section, &want.logical);
            }
            'k' => opt_out(s, section, want),
            'u' => apply_one(s, adapters, manager, Action::Install(want.clone()))?,
            _ => {}
        }
    }
    Ok(())
}

// ---- pixi envs edited on this machine ----

const DRIFT_KEYS: [char; 3] = ['a', 'r', 's'];
const DRIFT_LEGEND: &str = "[a]推到共享清单 [r]恢复为清单版本 [s]稍后";

pub fn claim_drift(
    s: &mut Session,
    adapters: &[Box<dyn Adapter>],
    items: &[(ManagerId, Want, Table)],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    s.app.ui.line(format!(
        "本机改动了 {} 个环境（和清单不一致）：",
        items.len()
    ));
    let width = name_width(items.iter().map(|(_, w, _)| w.native.as_str()));
    for (idx, (manager, want, current)) in items.iter().enumerate() {
        let (manager, section) = (*manager, manager.section());
        let legend = if idx == 0 {
            format!("{DRIFT_LEGEND} > ")
        } else {
            "> ".into()
        };
        let key = s.app.prompter.key(
            &format!("{}{legend}", label(manager, &want.native, width)),
            &DRIFT_KEYS,
        )?;
        match key {
            'a' => {
                match want.origin {
                    Origin::Shared => {
                        let groups = s.shared.get(section, &want.logical)?.and_then(|e| e.groups);
                        s.shared.upsert(
                            &Entry::pixi(&want.logical, current.clone()).with_groups(groups),
                        );
                        s.changes.push(Change::Update {
                            name: want.logical.clone(),
                            tag: manager.to_string(),
                        });
                    }
                    Origin::Local => {
                        s.local
                            .upsert_entry(&Entry::pixi(&want.logical, current.clone()));
                        s.local_changes += 1;
                    }
                }
                let by_adopt = s
                    .local
                    .applied(section)
                    .get(&want.logical)
                    .is_some_and(|a| a.by_adopt);
                s.local.set_applied(
                    section,
                    &want.logical,
                    &Applied {
                        manager,
                        native: want.native.clone(),
                        by_adopt,
                        fp: Some(fingerprint(current)),
                    },
                );
            }
            'r' => apply_one(s, adapters, manager, Action::Update(want.clone()))?,
            _ => {}
        }
    }
    Ok(())
}
