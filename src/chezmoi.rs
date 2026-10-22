//! chezmoi: run `chezmoi apply` after a pull brought new commits, and offer the
//! uncommitted changes in the source repo next to the package changes. Everything
//! here is skipped when chezmoi is not installed or manages a different repo.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use console::measure_text_width;

use crate::App;
use crate::claim::Session;
use crate::commit_msg::Change;
use crate::git::Git;
use crate::runner::Cmd;
use crate::state::shared;

/// chezmoi's source directory, if chezmoi is installed.
pub fn source_dir(app: &App) -> Option<PathBuf> {
    if !app.runner.which("chezmoi") {
        return None;
    }
    let out = app
        .runner
        .run(&Cmd::new("chezmoi").arg("source-path"))
        .ok()
        .filter(|o| o.success())?;
    let p = PathBuf::from(out.stdout.trim());
    (!p.as_os_str().is_empty()).then_some(p)
}

/// The git toplevel that holds chezmoi's source directory.
pub fn repo_root(app: &App) -> Option<PathBuf> {
    let src = source_dir(app)?;
    Some(Git::new(app.runner, &src).toplevel().unwrap_or(src))
}

/// chezmoi's source directory when it lives in `repo`, else `None` (no chezmoi step).
pub fn detect(app: &App, repo: &Path) -> Option<PathBuf> {
    let src = source_dir(app)?;
    let root = Git::new(app.runner, &src)
        .toplevel()
        .unwrap_or_else(|_| src.clone());
    same_path(&root, repo).then_some(src)
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `chezmoi apply` after a pull. Attached to the terminal when there is one, so
/// chezmoi can ask about conflicts; otherwise with `--no-tty`, where a needed
/// prompt makes it fail. A failure is reported, never fatal.
pub fn apply(app: &App) {
    let ui = app.ui;
    let cmd = if app.interactive {
        Cmd::new("chezmoi").arg("apply")
    } else {
        Cmd::new("chezmoi").args(["apply", "--no-tty"])
    };
    if app.dry_run {
        ui.would(cmd.to_string());
        return;
    }
    let result = if app.interactive {
        app.runner
            .run_attached(&cmd)
            .map(|code| (code == 0, format!("退出码 {code}")))
    } else {
        app.runner.run(&cmd).map(|o| (o.success(), o.tail()))
    };
    match result {
        Ok((true, _)) => ui.ok("已执行 chezmoi apply"),
        Ok((false, why)) => ui.warn(format!("`{cmd}` 失败：{why}")),
        Err(e) => ui.warn(format!("`{cmd}` 失败：{e:#}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Deleted,
    /// Untracked, or staged as new.
    New,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChange {
    /// Relative to the repo.
    pub path: String,
    /// Renames: the old path.
    pub orig: Option<String>,
    pub kind: Kind,
    /// Not in the index at all (`??`).
    pub untracked: bool,
}

impl ConfigChange {
    /// Every path a commit of this change has to include.
    pub fn paths(&self) -> Vec<String> {
        let mut v = vec![self.path.clone()];
        v.extend(self.orig.clone());
        v
    }
}

/// `git status --porcelain=v1 -z`: `XY path\0`, renames and copies as `XY new\0old\0`.
pub fn parse_status(out: &str) -> Vec<ConfigChange> {
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(f) = fields.next() {
        if f.len() < 4 || !f.is_char_boundary(3) {
            continue;
        }
        let (xy, path) = (&f[..2], f[3..].to_string());
        let renamed = xy.starts_with('R') || xy.starts_with('C');
        let orig = if renamed {
            fields.next().map(str::to_string)
        } else {
            None
        };
        let kind = if renamed {
            Kind::Renamed
        } else if xy == "??" || xy.starts_with('A') {
            Kind::New
        } else if xy.contains('D') {
            Kind::Deleted
        } else {
            Kind::Modified
        };
        changes.push(ConfigChange {
            path,
            orig,
            kind,
            untracked: xy == "??",
        });
    }
    changes
}

/// Uncommitted changes in the dotfiles repo, except packages.toml (adopt's own).
pub fn changes(app: &App, repo: &Path) -> Result<Vec<ConfigChange>> {
    let out = Git::new(app.runner, repo).status_z()?;
    let mut v: Vec<ConfigChange> = parse_status(&out)
        .into_iter()
        .filter(|c| c.path != shared::FILE_NAME)
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(v)
}

/// Best guess at the target of a source path when `chezmoi target-path` cannot
/// say (deleted files): drop attribute prefixes, `dot_` → `.`, drop `.tmpl`.
pub fn guess_target(in_source: &Path) -> String {
    const ATTRS: [&str; 11] = [
        "encrypted_",
        "private_",
        "readonly_",
        "empty_",
        "executable_",
        "symlink_",
        "exact_",
        "create_",
        "modify_",
        "remove_",
        "external_",
    ];
    let mut parts: Vec<String> = Vec::new();
    for comp in in_source.components() {
        let mut s = comp.as_os_str().to_string_lossy().into_owned();
        while let Some(rest) = ATTRS.iter().find_map(|a| s.strip_prefix(a)) {
            s = rest.to_string();
        }
        if let Some(rest) = s.strip_prefix("dot_") {
            s = format!(".{rest}");
        }
        parts.push(s);
    }
    if let Some(last) = parts.last_mut()
        && let Some(rest) = last.strip_suffix(".tmpl")
    {
        *last = rest.to_string();
    }
    format!("~/{}", parts.join("/"))
}

/// How a repo file shows up for the user: its target under HOME when it is part
/// of chezmoi's source state, else its path in the repo.
fn display_target(app: &App, repo: &Path, source: &Path, rel: &str) -> String {
    let abs = repo.join(rel);
    // The source dir may be reached through a symlink, so compare canonical paths too.
    let in_source = abs
        .strip_prefix(source)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let src = source.canonicalize().ok()?;
            let r = repo.canonicalize().ok()?;
            r.join(rel).strip_prefix(&src).ok().map(Path::to_path_buf)
        });
    let Some(in_source) = in_source else {
        return rel.to_string();
    };
    let special = in_source
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with(".chezmoi"));
    if special {
        return rel.to_string();
    }
    let cmd = Cmd::new("chezmoi").args(["target-path".to_string(), abs.display().to_string()]);
    if let Ok(o) = app.runner.run(&cmd)
        && o.success()
        && !o.stdout.trim().is_empty()
    {
        return app.paths.tilde(Path::new(o.stdout.trim()));
    }
    guess_target(&in_source)
}

/// `+2 行`, `+3 -1 行`, `新文件 +5 行`, `已删除`, ...
fn describe(git: &Git, repo: &Path, c: &ConfigChange) -> String {
    match c.kind {
        Kind::Deleted => "已删除".into(),
        Kind::Renamed => format!("由 {} 改名", c.orig.as_deref().unwrap_or("?")),
        Kind::New => match count_lines(&repo.join(&c.path)) {
            Some(n) => format!("新文件 +{n} 行"),
            None => "新文件".into(),
        },
        Kind::Modified => match git.numstat(&c.path) {
            Some((a, 0)) => format!("+{a} 行"),
            Some((0, d)) => format!("-{d} 行"),
            Some((a, d)) => format!("+{a} -{d} 行"),
            None => "已修改".into(),
        },
    }
}

/// Line count of a text file; `None` for binaries and unreadable files.
fn count_lines(p: &Path) -> Option<usize> {
    let bytes = fs::read(p).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    Some(
        bytes.iter().filter(|b| **b == b'\n').count()
            + usize::from(!bytes.ends_with(b"\n") && !bytes.is_empty()),
    )
}

/// `~/.zshrc` → `zshrc`, for the commit message.
fn commit_name(target: &str) -> String {
    let base = target.rsplit('/').next().unwrap_or(target);
    base.strip_prefix('.').unwrap_or(base).to_string()
}

/// Walk through the repo's uncommitted changes: [a] commit with this run,
/// [r] revert (asks again first), [d] show the diff, [s] leave it.
/// Paths to commit are appended to `commit_paths`.
pub fn claim(s: &mut Session, source: &Path, commit_paths: &mut Vec<String>) -> Result<()> {
    let app = s.app;
    let ui = app.ui;
    let repo = s.repo.clone();
    let items = changes(app, &repo)?;
    if items.is_empty() {
        return Ok(());
    }
    let git = Git::new(app.runner, &repo);
    let rows: Vec<(String, String)> = items
        .iter()
        .map(|c| {
            (
                display_target(app, &repo, source, &c.path),
                describe(&git, &repo, c),
            )
        })
        .collect();
    let width = rows
        .iter()
        .map(|(t, _)| measure_text_width(t))
        .max()
        .unwrap_or(0);

    ui.line("配置文件改动（来自 chezmoi 源仓库的 git diff）：");
    for (c, (target, stat)) in items.iter().zip(&rows) {
        let revertable = matches!(c.kind, Kind::Modified | Kind::Deleted);
        let (keys, legend): (&[char], &str) = if revertable {
            (&['a', 'r', 'd', 's'], "[a]同步 [r]撤销 [d]看 diff [s]稍后")
        } else {
            (&['a', 'd', 's'], "[a]同步 [d]看 diff [s]稍后")
        };
        let pad = " ".repeat(width.saturating_sub(measure_text_width(target)));
        loop {
            let prompt = format!("  {target}{pad} {stat}   {legend} > ");
            match app.prompter.key(&prompt, keys)? {
                'a' => {
                    commit_paths.extend(c.paths());
                    s.changes.push(Change::Config {
                        name: commit_name(target),
                    });
                }
                'r' => {
                    let question = format!("撤销 {target} 的本地改动？改动会丢失");
                    if !app.prompter.confirm(&question, false)? {
                        continue;
                    }
                    if app.dry_run {
                        ui.would(format!("git checkout HEAD -- {}", c.path));
                    } else {
                        git.restore(&c.path)?;
                        ui.ok(format!("已撤销 {target}"));
                    }
                }
                'd' => {
                    let paths: Vec<String> =
                        c.orig.iter().cloned().chain([c.path.clone()]).collect();
                    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
                    let text = git.diff_text(&refs, c.untracked)?;
                    for line in text.trim_end().lines() {
                        ui.line(format!("    {line}"));
                    }
                    continue;
                }
                _ => {}
            }
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_z() {
        let out = "MM dot_zshrc\0?? dir/new file\0R  dot_zprofile\0dot_bash_profile\0 D dot_gone\0A  dot_staged\0";
        let c = parse_status(out);
        assert_eq!(c.len(), 5);
        assert_eq!(
            (c[0].path.as_str(), c[0].kind),
            ("dot_zshrc", Kind::Modified)
        );
        assert_eq!(
            (c[1].path.as_str(), c[1].kind, c[1].untracked),
            ("dir/new file", Kind::New, true)
        );
        assert_eq!(c[2].kind, Kind::Renamed);
        assert_eq!(c[2].orig.as_deref(), Some("dot_bash_profile"));
        assert_eq!(c[2].paths(), ["dot_zprofile", "dot_bash_profile"]);
        assert_eq!(c[3].kind, Kind::Deleted);
        assert_eq!((c[4].kind, c[4].untracked), (Kind::New, false));
    }

    #[test]
    fn guesses_targets_from_source_names() {
        let g = |p: &str| guess_target(Path::new(p));
        assert_eq!(g("dot_zshrc"), "~/.zshrc");
        assert_eq!(g("private_dot_ssh/private_config"), "~/.ssh/config");
        assert_eq!(
            g("dot_config/nvim/init.lua.tmpl"),
            "~/.config/nvim/init.lua"
        );
        assert_eq!(
            g("dot_local/bin/executable_setproxy"),
            "~/.local/bin/setproxy"
        );
    }

    #[test]
    fn commit_names_drop_the_dot() {
        assert_eq!(commit_name("~/.zshrc"), "zshrc");
        assert_eq!(commit_name("~/.config/nvim/init.lua"), "init.lua");
        assert_eq!(commit_name("README.md"), "README.md");
    }
}
