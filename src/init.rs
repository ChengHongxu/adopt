//! `adopt init`: point at the dotfiles repo, set this machine's groups, take a baseline.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::App;
use crate::adapters::apt::{self, Apt};
use crate::chezmoi;
use crate::claim::{commit_and_push, parse_groups};
use crate::cli::InitArgs;
use crate::git::Git;
use crate::model::ManagerId;
use crate::runner::Cmd;
use crate::state::{Local, Shared, shared, write_atomic};

pub fn run(app: &App, args: &InitArgs) -> Result<()> {
    let ui = app.ui;
    let ask = app.interactive && !args.yes;
    let mut local = Local::load(&app.paths)?;

    // ---- repo ----
    let repo = match &args.repo {
        Some(r) => app.paths.expand(r),
        None => {
            let guess = local
                .repo()
                .or_else(|| chezmoi::repo_root(app))
                .unwrap_or_else(|| default_repo(app));
            if ask {
                app.paths.expand(
                    &app.prompter
                        .input("dotfiles 仓库路径", &app.paths.tilde(&guess))?,
                )
            } else {
                guess
            }
        }
    };
    let repo = if repo.is_absolute() {
        repo
    } else {
        std::env::current_dir()?.join(repo)
    };
    if let Some(url) = &args.clone {
        clone_if_missing(app, url, &repo)?;
    }
    let cloned_in_dry_run = app.dry_run && args.clone.is_some() && !repo.exists();
    if !cloned_in_dry_run && !Git::new(app.runner, &repo).is_repo() {
        bail!(
            "{} 不是 git 仓库；可以用 --clone <URL> 先 clone 下来",
            repo.display()
        );
    }
    local.set_repo(&repo);

    // ---- groups ----
    let groups = match &args.groups {
        Some(g) => parse_groups(g),
        None if ask => {
            let mut hint = if cloned_in_dry_run {
                BTreeSet::new()
            } else {
                Shared::load(&repo)?.groups()
            };
            hint.extend(local.groups());
            let hint = if hint.is_empty() {
                "，例如 work,homelab".to_string()
            } else {
                format!(
                    "（已有：{}）",
                    hint.into_iter().collect::<Vec<_>>().join(", ")
                )
            };
            parse_groups(&app.prompter.input(
                &format!("本机分组，逗号分隔{hint}"),
                &local.groups().join(","),
            )?)
        }
        None => local.groups(),
    };
    local.set_groups(&groups);

    // ---- baseline ----
    let cx = app.ctx();
    for adapter in app.adapters().iter().filter(|a| a.baseline_on_init()) {
        let id = adapter.id();
        let retake = match local.baseline(id) {
            None => true,
            Some(_) if args.rebaseline => true,
            Some(_) => false,
        };
        if !retake {
            ui.line(format!("  {id}：已有基线，保留（--rebaseline 可重新记录）"));
            continue;
        }
        let names = adapter.list_user_installed(&cx)?;
        local.set_baseline(id, &names);
        ui.ok(format!(
            "{id}：记录了 {} 个已装的包作为基线，之后只关心新装的",
            names.len()
        ));
    }

    if app.dry_run {
        ui.would(format!("写入 {}", app.paths.tilde(&local.file.path)));
    } else {
        local.file.save()?;
        ui.ok(format!("已写入 {}", app.paths.tilde(&local.file.path)));
    }

    offer_apt_hook(app, args)?;
    if cloned_in_dry_run {
        return Ok(());
    }
    prepare_repo(app, &repo)
}

/// The hook needs root and writes under /etc, so it is only installed with explicit
/// consent: `--apt-hook`, or a yes after seeing exactly what will be written.
fn offer_apt_hook(app: &App, args: &InitArgs) -> Result<()> {
    let ui = app.ui;
    if !app.adapters().iter().any(|a| a.id() == ManagerId::Apt)
        || app.paths.sys(apt::HOOK_FILE).exists()
    {
        return Ok(());
    }
    let wanted = if args.apt_hook {
        true
    } else if app.interactive && !args.yes {
        ui.line("可选：安装 apt 钩子，每次 apt/dpkg 装卸后更新一个时间戳，`adopt status` 据此判断是否需要重新计算。");
        ui.line(format!("将以 root 写入 {}：", apt::HOOK_FILE));
        for l in apt::HOOK_TEXT.lines() {
            ui.line(format!("  {}", ui.dim(l)));
        }
        app.prompter
            .confirm("安装 apt 钩子吗？（需要 sudo）", false)?
    } else {
        false
    };
    if !wanted {
        return Ok(());
    }
    let cmd = match app.ctx().elevate(Apt::hook_install_cmd()) {
        Ok(c) => c,
        Err(why) => {
            ui.warn(format!("跳过 apt 钩子：{why}"));
            return Ok(());
        }
    };
    if app.dry_run {
        ui.would(cmd.to_string());
        return Ok(());
    }
    if let Err(why) = app.privilege.ensure(app.runner, app.interactive) {
        ui.warn(format!("跳过 apt 钩子：{why}"));
        return Ok(());
    }
    match app.runner.run_attached(&cmd)? {
        0 => ui.ok(format!("已安装 {}", apt::HOOK_FILE)),
        code => ui.warn(format!(
            "安装 apt 钩子失败（退出码 {code}），不影响其他功能"
        )),
    }
    Ok(())
}

/// Create packages.toml (and keep chezmoi from treating it as a dotfile), then commit.
fn prepare_repo(app: &App, repo: &Path) -> Result<()> {
    let ui = app.ui;
    let mut paths: Vec<&str> = Vec::new();
    let mut shared = Shared::load(repo)?;
    if !shared.file.exists() {
        if app.dry_run {
            ui.would(format!("创建 {}", shared.file.path.display()));
        } else {
            shared.file.save()?;
            ui.ok(format!("已创建 {}", shared.file.path.display()));
        }
        paths.push(shared::FILE_NAME);
    }
    if is_chezmoi_source_root(app, repo) && add_chezmoiignore(app, repo)? {
        paths.push(".chezmoiignore");
    }
    if paths.is_empty() {
        return Ok(());
    }
    let host = app.host();
    commit_and_push(app, repo, &paths, &format!("adopt({host}): init"), "init")
}

fn default_repo(app: &App) -> PathBuf {
    app.paths.home.join(".local/share/chezmoi")
}

/// True when chezmoi reads its source state straight from the repo root, so a
/// `packages.toml` there would otherwise become `~/packages.toml`.
fn is_chezmoi_source_root(app: &App, repo: &Path) -> bool {
    if repo.join(".chezmoiroot").exists() {
        return false;
    }
    if let Some(r) = chezmoi::repo_root(app) {
        return chezmoi::same_path(&r, repo);
    }
    fs::read_dir(repo).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("dot_") || n.starts_with("private_dot_") || n.starts_with(".chezmoi")
        })
    })
}

fn add_chezmoiignore(app: &App, repo: &Path) -> Result<bool> {
    let path = repo.join(".chezmoiignore");
    let text = fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == shared::FILE_NAME) {
        return Ok(false);
    }
    let mut new = text;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(&format!("{}\n", shared::FILE_NAME));
    if app.dry_run {
        app.ui.would(format!(
            "在 {} 里加入 {}",
            path.display(),
            shared::FILE_NAME
        ));
    } else {
        write_atomic(&path, &new)?;
        app.ui
            .ok(format!("已把 {} 加入 .chezmoiignore", shared::FILE_NAME));
    }
    Ok(true)
}

fn clone_if_missing(app: &App, url: &str, repo: &Path) -> Result<()> {
    let empty = fs::read_dir(repo)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);
    if !empty {
        return Ok(());
    }
    let cmd = Cmd::new("git").args(["clone", "--quiet", url, &repo.display().to_string()]);
    if app.dry_run {
        app.ui.would(cmd.to_string());
        return Ok(());
    }
    let out = app.runner.run(&cmd)?;
    if !out.success() {
        bail!("git clone {url} 失败：\n{}", out.tail());
    }
    app.ui.ok(format!("已 clone 到 {}", repo.display()));
    fs::metadata(repo).with_context(|| format!("clone 之后找不到 {}", repo.display()))?;
    Ok(())
}
