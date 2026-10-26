//! `adopt sync [--yes]`: pull and apply, never claim. Safe for systemd timers and
//! cloud-init: a failed pull or missing sudo is reported, not fatal.

use anyhow::Result;

use crate::App;
use crate::apply::{self, RootPolicy};
use crate::claim::{Session, pull};
use crate::cli::SyncArgs;
use crate::plan::{self, Action, Inventory};

pub fn run(app: &App, args: &SyncArgs) -> Result<()> {
    let ui = app.ui;
    let mut s = Session::open(app)?;
    match pull(app, &s.repo) {
        Ok(pulled) if !pulled.is_empty() => {
            ui.line(format!("↓ 拉取 {} 个提交", pulled.len()));
            s.reload_shared()?;
            if crate::chezmoi::detect(app, &s.repo).is_some() {
                crate::chezmoi::apply(app);
            }
        }
        Ok(_) => {}
        Err(e) => ui.warn(format!("{e:#}\n  继续使用本地的清单")),
    }

    let adapters = app.adapters();
    let inv = Inventory::collect(&adapters, &app.ctx())?;
    let plan = plan::reconcile(&s.shared, &s.local, &inv)?;
    let changes: Vec<String> = plan
        .actions
        .iter()
        .filter_map(|(m, a)| describe(*m, a))
        .collect();

    if !changes.is_empty() && !args.yes && !app.dry_run {
        ui.line(format!("将要应用：{}", changes.join("、")));
        for c in apply::preview(app, &adapters, &plan) {
            ui.line(format!("  {}", ui.bold(c)));
        }
        if !app.interactive {
            ui.warn("没有终端，未做任何改动；加 --yes 自动应用");
            return Ok(());
        }
        if !app.prompter.confirm("应用这些改动吗？", true)? {
            ui.warn("未做任何改动");
            return Ok(());
        }
    }

    // Without --yes the user has just confirmed the full commands, sudo included.
    let (adapters, report) = apply::apply_and_follow_up(
        app,
        &s.shared,
        &mut s.local,
        adapters,
        &plan,
        RootPolicy::Allow,
    )?;
    if !app.dry_run && s.local.file.is_dirty() {
        s.local.file.save()?;
    }
    crate::status::refresh(app);

    // Count what is left against the machine as it is now (an adapter may have appeared).
    let (pending, new_count) = if app.dry_run {
        (
            plan.pending.len(),
            plan::find_new(&s.shared, &s.local, &inv)?.len(),
        )
    } else {
        let inv = Inventory::collect(&adapters, &app.ctx())?;
        let after = plan::reconcile(&s.shared, &s.local, &inv)?;
        (
            after.pending.len(),
            plan::find_new(&s.shared, &s.local, &inv)?.len(),
        )
    };
    let waiting = pending + new_count;
    match apply::summary(&report) {
        Some(line) => ui.ok(line),
        None if waiting == 0 && report.skipped == 0 => ui.ok("软件清单与本机一致"),
        None => {}
    }
    if waiting > 0 {
        ui.warn(format!("{waiting} 个条目等你处理，运行 `adopt`"));
    }
    Ok(())
}

/// `安装 htop(apt)` for actions that change the machine.
pub fn describe(manager: crate::model::ManagerId, a: &Action) -> Option<String> {
    match a {
        Action::Install(w) => Some(format!("安装 {}({manager})", w.native)),
        Action::Update(w) => Some(format!("更新 {}({manager})", w.native)),
        Action::Remove { native, .. } => Some(format!("卸载 {native}({manager})")),
        _ => None,
    }
}
