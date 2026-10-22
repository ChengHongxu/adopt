//! Carry out a [`Plan`]'s actions: batch per manager, ask before sudo, record the
//! outcome in local.toml. Missing root never aborts the run; those changes are skipped.

use anyhow::Result;

use crate::App;
use crate::adapters::{Adapter, Op, PkgSpec};
use crate::plan::{Action, Plan, Want};
use crate::privilege::Privilege;
use crate::state::{Applied, Failed, Local, fingerprint};

/// What to do before running a command that needs root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootPolicy {
    /// Show the commands and ask (interactive runs).
    Confirm,
    /// Just run them (`sync --yes`).
    Allow,
    /// Skip them, with this hint (`sync` without a terminal or `--yes`).
    Skip(&'static str),
}

#[derive(Debug, Default)]
pub struct Report {
    /// `安装 htop(apt)` etc., for the summary line.
    pub done: Vec<String>,
    pub failed: usize,
    pub skipped: usize,
    /// Nothing actually ran (`--dry-run`).
    pub dry: bool,
}

pub fn apply(
    app: &App,
    local: &mut Local,
    adapters: &[Box<dyn Adapter>],
    plan: &Plan,
    policy: RootPolicy,
) -> Result<Report> {
    let mut report = Report {
        dry: app.dry_run,
        ..Report::default()
    };
    for adapter in adapters {
        let id = adapter.id();
        let section = adapter.section();
        let mut installs: Vec<(&Want, bool)> = Vec::new();
        let mut removes: Vec<PkgSpec> = Vec::new();
        for (m, a) in &plan.actions {
            if *m != id {
                continue;
            }
            match a {
                Action::Install(w) => installs.push((w, false)),
                Action::Update(w) => installs.push((w, true)),
                Action::Remove { logical, native } => removes.push(PkgSpec {
                    logical: logical.clone(),
                    native: native.clone(),
                    body: None,
                }),
                Action::MarkApplied { logical, applied } => {
                    local.set_applied(section, logical, applied);
                    local.clear_failed(section, logical);
                }
                Action::Forget { logical } => {
                    local.remove_applied(section, logical);
                }
                Action::ClearFailed { logical } => {
                    local.clear_failed(section, logical);
                }
            }
        }
        if installs.is_empty() && removes.is_empty() {
            continue;
        }
        let install_specs: Vec<PkgSpec> = installs
            .iter()
            .map(|(w, _)| PkgSpec {
                logical: w.logical.clone(),
                native: w.native.clone(),
                body: w.body.clone(),
            })
            .collect();
        let n = install_specs.len() + removes.len();

        if adapter.needs_root()
            && let Err(why) = root_ok(app, adapter.as_ref(), &install_specs, &removes, policy)?
        {
            app.ui.warn(format!("跳过 {n} 个 {id} 改动：{why}"));
            report.skipped += n;
            continue;
        }

        let cx = app.ctx();
        let prev = local.applied(section);
        for (o, (w, update)) in adapter
            .install(&cx, &install_specs)?
            .into_iter()
            .zip(&installs)
        {
            match o.result {
                Ok(()) => {
                    // An update keeps whoever installed it originally.
                    let by_adopt = !*update || prev.get(&w.logical).is_some_and(|a| a.by_adopt);
                    local.set_applied(
                        section,
                        &w.logical,
                        &Applied {
                            manager: id,
                            native: w.native.clone(),
                            by_adopt,
                            fp: w.body.as_ref().map(fingerprint),
                        },
                    );
                    local.clear_failed(section, &w.logical);
                    let verb = if *update { "更新" } else { "安装" };
                    report.done.push(format!("{verb} {}({id})", w.native));
                }
                Err(error) => {
                    local.set_failed(
                        section,
                        &w.logical,
                        &Failed {
                            manager: id,
                            native: w.native.clone(),
                            error: error.clone(),
                        },
                    );
                    app.ui.fail(format!("{id} 安装 {} 失败：{error}", w.native));
                    report.failed += 1;
                }
            }
        }
        for (o, spec) in adapter.remove(&cx, &removes)?.into_iter().zip(&removes) {
            match o.result {
                Ok(()) => {
                    local.remove_applied(section, &spec.logical);
                    report.done.push(format!("卸载 {}({id})", spec.native));
                }
                Err(error) => {
                    // Stays applied, so the next run tries again.
                    app.ui
                        .fail(format!("{id} 卸载 {} 失败：{error}", spec.native));
                    report.failed += 1;
                }
            }
        }
    }
    Ok(report)
}

/// Whether root changes may go ahead: privilege is available and, under
/// [`RootPolicy::Confirm`], the user said yes to the exact commands. The inner
/// error says why not; the outer one is Ctrl-C.
fn root_ok(
    app: &App,
    adapter: &dyn Adapter,
    installs: &[PkgSpec],
    removes: &[PkgSpec],
    policy: RootPolicy,
) -> Result<std::result::Result<(), String>> {
    if let RootPolicy::Skip(hint) = policy {
        return Ok(Err(hint.to_string()));
    }
    let cx = app.ctx();
    if let Privilege::Unavailable(why) = cx.privilege.get(cx.runner, cx.interactive) {
        return Ok(Err(why));
    }
    if app.dry_run {
        // The adapter prints the exact commands itself.
        return Ok(Ok(()));
    }
    if policy == RootPolicy::Confirm {
        let cmds = command_lines(app, adapter, installs, removes);
        app.ui.line("将要执行：");
        for c in &cmds {
            app.ui.line(format!("  {}", app.ui.bold(c)));
        }
        if !app.prompter.confirm("执行吗？", true)? {
            return Ok(Err("你选择了不执行".into()));
        }
    }
    Ok(cx.privilege.ensure(cx.runner, cx.interactive))
}

/// The exact commands `adapter` will run, sudo included (or why it cannot).
fn command_lines(
    app: &App,
    adapter: &dyn Adapter,
    installs: &[PkgSpec],
    removes: &[PkgSpec],
) -> Vec<String> {
    let cx = app.ctx();
    adapter
        .planned_commands(installs, Op::Install)
        .into_iter()
        .chain(adapter.planned_commands(removes, Op::Remove))
        .map(|c| {
            if !adapter.needs_root() {
                return c.to_string();
            }
            match cx.elevate(c) {
                Ok(c) => c.to_string(),
                Err(why) => format!("（跳过：{why}）"),
            }
        })
        .collect()
}

/// Every command a plan will run, grouped per adapter, for one up-front confirmation.
pub fn preview(app: &App, adapters: &[Box<dyn Adapter>], plan: &Plan) -> Vec<String> {
    let mut out = Vec::new();
    for adapter in adapters {
        let (mut installs, mut removes) = (Vec::new(), Vec::new());
        for (m, a) in &plan.actions {
            if *m != adapter.id() {
                continue;
            }
            match a {
                Action::Install(w) | Action::Update(w) => installs.push(PkgSpec {
                    logical: w.logical.clone(),
                    native: w.native.clone(),
                    body: w.body.clone(),
                }),
                Action::Remove { logical, native } => removes.push(PkgSpec {
                    logical: logical.clone(),
                    native: native.clone(),
                    body: None,
                }),
                _ => {}
            }
        }
        out.extend(command_lines(app, adapter.as_ref(), &installs, &removes));
    }
    out
}

/// One line for the user, e.g. `已应用：安装 setproxy(pixi)、卸载 tmux(apt)`.
pub fn summary(report: &Report) -> Option<String> {
    let verb = if report.dry { "将应用" } else { "已应用" };
    (!report.done.is_empty()).then(|| format!("{verb}：{}", report.done.join("、")))
}
