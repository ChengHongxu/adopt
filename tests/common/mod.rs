//! Test harness: a fake machine in a temp dir. Package managers are simulated in
//! memory; git is real but sandboxed (own HOME, no global/system config), so nothing
//! outside the temp dir is ever touched.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use adopt::App;
use adopt::cli::Cli;
use adopt::paths::Paths;
use adopt::prompt::{Aborted, Prompter};
use adopt::runner::{Cmd, CommandRunner, Output, SystemRunner};
use adopt::ui::{SharedBuf, Ui};
use anyhow::Result;
use clap::Parser;

pub const HOST: &str = "testhost";

/// A simulated package manager: what is installed and what its repos offer.
#[derive(Default)]
pub struct FakePm {
    pub installed: BTreeSet<String>,
    pub repo: BTreeSet<String>,
}

pub struct FakeSystem {
    pub base: PathBuf,
    /// Programs `which` reports as present.
    pub programs: RefCell<BTreeSet<String>>,
    /// Exit code for `pixi global sync`.
    pub pixi_sync_code: RefCell<i32>,
    /// apt: what `apt-mark showmanual` reports.
    pub apt_manual: RefCell<BTreeSet<String>>,
    /// apt: names `apt-get install` can find. Anything else fails with 100.
    pub apt_repo: RefCell<BTreeSet<String>>,
    /// Output of `id -u`.
    pub uid: RefCell<u32>,
    /// Whether `sudo -n true` / `sudo -v` succeed.
    pub sudo_ok: RefCell<bool>,
    /// dnf. `@group` names install fine but never appear in `--userinstalled`.
    pub dnf: RefCell<FakePm>,
    /// Whether `dnf` is dnf5 (different `--version` output and repoquery flags).
    pub dnf5: RefCell<bool>,
    /// Homebrew formulae; `leaves` is `installed`.
    pub brew: RefCell<FakePm>,
    /// Every non-git command run, as displayed.
    pub log: RefCell<Vec<String>>,
}

impl FakeSystem {
    fn git_env(&self, cmd: &Cmd) -> Cmd {
        let mut c = cmd.clone();
        let home = self.base.join("home");
        for (k, v) in [
            ("HOME", home.display().to_string()),
            (
                "GIT_CONFIG_GLOBAL",
                self.base.join("gitconfig").display().to_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_AUTHOR_NAME", "adopt-test".into()),
            ("GIT_AUTHOR_EMAIL", "test@example.invalid".into()),
            ("GIT_COMMITTER_NAME", "adopt-test".into()),
            ("GIT_COMMITTER_EMAIL", "test@example.invalid".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
        ] {
            c = c.env(k, v);
        }
        c
    }

    fn apt_get(&self, cmd: &Cmd, args: &[&str]) -> i32 {
        if cmd.program != "sudo" && *self.uid.borrow() != 0 {
            return 100; // "are you root?"
        }
        if args.contains(&"update") {
            return 0;
        }
        let Some(pos) = args.iter().position(|a| *a == "install" || *a == "remove") else {
            return 64;
        };
        let names = &args[pos + 1..];
        if names.is_empty() {
            return 64;
        }
        if args[pos] == "install" {
            if names.iter().any(|n| !self.apt_repo.borrow().contains(*n)) {
                return 100; // E: Unable to locate package
            }
            self.apt_manual
                .borrow_mut()
                .extend(names.iter().map(|n| n.to_string()));
        } else {
            for n in names {
                self.apt_manual.borrow_mut().remove(*n);
            }
        }
        self.touch_dpkg();
        0
    }

    /// Rewrite the fake `/var/lib/dpkg/status`, as dpkg does on every change.
    pub fn touch_dpkg(&self) {
        let p = self.base.join("root/var/lib/dpkg/status");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let names: Vec<String> = self.apt_manual.borrow().iter().cloned().collect();
        fs::write(p, format!("{}\n", names.join("\n"))).unwrap();
    }

    /// `sh -c 'printf ... > /etc/apt/apt.conf.d/99adopt' sh TEXT`, redirected into the sandbox.
    fn sh(&self, args: &[&str]) -> i32 {
        match args {
            ["-c", script, "sh", text] if script.contains("99adopt") => {
                let p = self.base.join("root/etc/apt/apt.conf.d/99adopt");
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, text).unwrap();
                0
            }
            _ => 127,
        }
    }

    fn dnf_cmd(&self, cmd: &Cmd, args: &[&str]) -> i32 {
        if cmd.program != "sudo" && *self.uid.borrow() != 0 {
            return 1; // "This command has to be run with superuser privileges"
        }
        let ["-y", verb, names @ ..] = args else {
            return 64;
        };
        if names.is_empty() {
            return 64;
        }
        let mut d = self.dnf.borrow_mut();
        match *verb {
            "install" => {
                if names.iter().any(|n| !d.repo.contains(*n)) {
                    return 1; // No match for argument
                }
                for n in names.iter().filter(|n| !n.starts_with('@')) {
                    d.installed.insert(n.to_string());
                }
            }
            "remove" => {
                for n in names {
                    d.installed.remove(*n);
                }
            }
            _ => return 64,
        }
        drop(d);
        self.touch_rpm();
        0
    }

    /// Rewrite the fake rpm database, as every dnf transaction does.
    pub fn touch_rpm(&self) {
        let p = self.base.join("root/usr/lib/sysimage/rpm/rpmdb.sqlite");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let names: Vec<String> = self.dnf.borrow().installed.iter().cloned().collect();
        fs::write(p, names.join("\n")).unwrap();
    }

    fn brew_cmd(&self, cmd: &Cmd, args: &[&str]) -> i32 {
        if cmd.program == "sudo" {
            return 1; // "Running Homebrew as root is extremely dangerous"
        }
        let [verb, "--formula", names @ ..] = args else {
            return 64;
        };
        let cellar = self.base.join("root/opt/homebrew/Cellar");
        let mut b = self.brew.borrow_mut();
        match *verb {
            "install" => {
                if names.is_empty() || names.iter().any(|n| !b.repo.contains(*n)) {
                    return 1; // No available formula
                }
                for n in names {
                    b.installed.insert(n.to_string());
                    fs::create_dir_all(cellar.join(n.replace('/', "_"))).unwrap();
                }
            }
            "uninstall" => {
                for n in names {
                    b.installed.remove(*n);
                    let _ = fs::remove_dir_all(cellar.join(n.replace('/', "_")));
                }
            }
            _ => return 64,
        }
        0
    }

    pub fn ran(&self, needle: &str) -> bool {
        self.log.borrow().iter().any(|l| l.contains(needle))
    }
}

impl CommandRunner for FakeSystem {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let (program, args) = cmd.unwrapped();
        if program == "git" {
            return SystemRunner.run(&self.git_env(cmd));
        }
        self.log.borrow_mut().push(cmd.to_string());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match (program, args.as_slice()) {
            ("uname", ["-n"]) => Output::ok(format!("{HOST}\n")),
            ("id", ["-u"]) => Output::ok(format!("{}\n", self.uid.borrow())),
            ("true", []) if cmd.program == "sudo" => {
                if *self.sudo_ok.borrow() {
                    Output::ok("")
                } else {
                    Output::fail(1, "sudo: a password is required")
                }
            }
            ("dnf", ["--version"]) => Output::ok(if *self.dnf5.borrow() {
                include_str!("../fixtures/dnf5-version.txt")
            } else {
                include_str!("../fixtures/dnf4-version.txt")
            }),
            ("dnf", ["-q", "repoquery", "--userinstalled", flags @ ..]) => {
                let v5 = *self.dnf5.borrow();
                let expected: &[&str] = if v5 {
                    &["--queryformat", "%{name}\n"]
                } else {
                    &["--qf", "%{name}"]
                };
                if flags != expected {
                    Output::fail(2, format!("fake dnf: unknown arguments {flags:?}"))
                } else {
                    // dnf5 may print repository progress on stdout.
                    let mut s = String::new();
                    if v5 {
                        s.push_str("Updating and loading repositories:\nRepositories loaded.\n");
                    }
                    for n in self.dnf.borrow().installed.iter() {
                        s.push_str(n);
                        s.push('\n');
                    }
                    Output::ok(s)
                }
            }
            ("brew", ["leaves", "--installed-on-request"]) => {
                let names: Vec<String> = self.brew.borrow().installed.iter().cloned().collect();
                Output::ok(names.iter().map(|n| format!("{n}\n")).collect::<String>())
            }
            ("apt-mark", ["showmanual"]) => {
                let mut s = String::new();
                for n in self.apt_manual.borrow().iter() {
                    s.push_str(n);
                    s.push('\n');
                }
                Output::ok(s)
            }
            _ => Output::fail(127, format!("fake: no handler for `{cmd}`")),
        })
    }

    fn run_attached(&self, cmd: &Cmd) -> Result<i32> {
        let (program, args) = cmd.unwrapped();
        if program == "git" {
            return SystemRunner.run_attached(&self.git_env(cmd));
        }
        self.log.borrow_mut().push(cmd.to_string());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match (program, args.as_slice()) {
            ("pixi", ["global", "sync"]) => *self.pixi_sync_code.borrow(),
            ("apt-get", a) => self.apt_get(cmd, a),
            ("sh", a) => self.sh(a),
            ("dnf", a) => self.dnf_cmd(cmd, a),
            ("brew", a) => self.brew_cmd(cmd, a),
            ("-v", []) if cmd.program == "sudo" => i32::from(!*self.sudo_ok.borrow()),
            _ => 127,
        })
    }

    fn which(&self, program: &str) -> bool {
        program == "git" || self.programs.borrow().contains(program)
    }
}

#[derive(Debug)]
pub enum Answer {
    Key(char),
    Yes(bool),
    Text(String),
    /// Multi-select: check exactly these items (by label).
    Pick(Vec<&'static str>),
    /// Behave like Ctrl-C.
    Abort,
}

#[derive(Default)]
pub struct Script {
    answers: RefCell<VecDeque<Answer>>,
    pub asked: RefCell<Vec<String>>,
}

impl Script {
    pub fn new(answers: impl IntoIterator<Item = Answer>) -> Script {
        Script {
            answers: RefCell::new(answers.into_iter().collect()),
            asked: RefCell::default(),
        }
    }

    fn next(&self, prompt: &str) -> Answer {
        self.asked.borrow_mut().push(prompt.to_string());
        self.answers
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected prompt: {prompt:?}"))
    }

    pub fn assert_done(&self) {
        let left = self.answers.borrow();
        assert!(
            left.is_empty(),
            "unused answers: {left:?}; asked: {:?}",
            self.asked.borrow()
        );
    }
}

impl Prompter for Script {
    fn key(&self, prompt: &str, keys: &[char]) -> Result<char> {
        match self.next(prompt) {
            Answer::Key(c) => {
                assert!(
                    keys.contains(&c),
                    "{c:?} not offered at {prompt:?} ({keys:?})"
                );
                Ok(c)
            }
            Answer::Abort => Err(Aborted.into()),
            other => panic!("expected key at {prompt:?}, scripted {other:?}"),
        }
    }

    fn confirm(&self, prompt: &str, _default: bool) -> Result<bool> {
        match self.next(prompt) {
            Answer::Yes(b) => Ok(b),
            Answer::Abort => Err(Aborted.into()),
            other => panic!("expected yes/no at {prompt:?}, scripted {other:?}"),
        }
    }

    fn multi_select(
        &self,
        prompt: &str,
        items: &[String],
        _defaults: &[bool],
    ) -> Result<Vec<usize>> {
        match self.next(prompt) {
            Answer::Pick(labels) => Ok(labels
                .iter()
                .map(|l| {
                    items
                        .iter()
                        .position(|i| i.contains(l))
                        .unwrap_or_else(|| panic!("{l:?} not offered at {prompt:?}: {items:?}"))
                })
                .collect()),
            Answer::Abort => Err(Aborted.into()),
            other => panic!("expected multi-select at {prompt:?}, scripted {other:?}"),
        }
    }

    fn input(&self, prompt: &str, default: &str) -> Result<String> {
        match self.next(prompt) {
            Answer::Text(t) if t.is_empty() => Ok(default.to_string()),
            Answer::Text(t) => Ok(t),
            Answer::Abort => Err(Aborted.into()),
            other => panic!("expected text at {prompt:?}, scripted {other:?}"),
        }
    }
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub paths: Paths,
    pub sys: FakeSystem,
    /// Bare repo playing GitHub.
    pub remote: PathBuf,
    /// This machine's dotfiles checkout (chezmoi's default location).
    pub repo: PathBuf,
}

impl Env {
    /// A machine with pixi installed and a dotfiles repo cloned from a remote.
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let paths = Paths::sandbox(&base);
        fs::create_dir_all(&paths.home).unwrap();
        fs::write(base.join("gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
        let sys = FakeSystem {
            base: base.clone(),
            programs: RefCell::new(["pixi".to_string()].into()),
            pixi_sync_code: RefCell::new(0),
            apt_manual: RefCell::default(),
            apt_repo: RefCell::default(),
            uid: RefCell::new(1000),
            sudo_ok: RefCell::new(true),
            dnf: RefCell::default(),
            dnf5: RefCell::new(true),
            brew: RefCell::default(),
            log: RefCell::default(),
        };
        let remote = base.join("remote.git");
        let repo = paths.home.join(".local/share/chezmoi");
        let env = Env {
            dir,
            paths,
            sys,
            remote,
            repo,
        };
        env.git(
            &base,
            &["init", "--quiet", "--bare", env.remote.to_str().unwrap()],
        );
        let seed = base.join("seed");
        env.git(
            &base,
            &[
                "clone",
                "--quiet",
                env.remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        fs::write(seed.join("dot_zshrc"), "export EDITOR=vim\n").unwrap();
        env.git(&seed, &["add", "."]);
        env.git(&seed, &["commit", "--quiet", "-m", "seed"]);
        env.git(&seed, &["push", "--quiet", "origin", "HEAD"]);
        env.git(
            &base,
            &[
                "clone",
                "--quiet",
                env.remote.to_str().unwrap(),
                env.repo.to_str().unwrap(),
            ],
        );
        env
    }

    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let cmd = Cmd::new("git")
            .arg("-C")
            .arg(cwd.display().to_string())
            .args(args.iter().copied());
        let out = self.sys.run(&cmd).unwrap();
        assert!(out.success(), "git {args:?} failed: {}", out.stderr);
        out.stdout
    }

    /// Subjects on the remote's main branch, newest first.
    pub fn remote_log(&self) -> Vec<String> {
        self.git(&self.remote, &["log", "--format=%s", "main"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Another machine's checkout, for pushing competing commits.
    pub fn other_clone(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        self.git(
            self.dir.path(),
            &[
                "clone",
                "--quiet",
                self.remote.to_str().unwrap(),
                p.to_str().unwrap(),
            ],
        );
        p
    }

    /// Make this a Debian-like machine with `manual` already installed.
    pub fn with_apt(&self, manual: &[&str], repo: &[&str]) {
        let mut p = self.sys.programs.borrow_mut();
        p.extend(["apt-get", "apt-mark", "sudo"].map(String::from));
        *self.sys.apt_manual.borrow_mut() = manual.iter().map(|s| s.to_string()).collect();
        *self.sys.apt_repo.borrow_mut() =
            repo.iter().chain(manual).map(|s| s.to_string()).collect();
        drop(p);
        self.sys.touch_dpkg();
    }

    /// Make this a Fedora-like machine (dnf5 unless `v5` is false).
    pub fn with_dnf(&self, installed: &[&str], repo: &[&str], v5: bool) {
        self.sys
            .programs
            .borrow_mut()
            .extend(["dnf", "sudo"].map(String::from));
        *self.sys.dnf5.borrow_mut() = v5;
        let mut d = self.sys.dnf.borrow_mut();
        d.installed = installed.iter().map(|s| s.to_string()).collect();
        d.repo = repo
            .iter()
            .chain(installed)
            .map(|s| s.to_string())
            .collect();
        drop(d);
        self.sys.touch_rpm();
    }

    /// The user runs `dnf install <name>` by hand.
    pub fn dnf_add(&self, name: &str) {
        self.sys.dnf.borrow_mut().installed.insert(name.into());
        self.sys.touch_rpm();
    }

    pub fn dnf_installed(&self, name: &str) -> bool {
        self.sys.dnf.borrow().installed.contains(name)
    }

    /// Give this machine Homebrew with `leaves` installed.
    pub fn with_brew(&self, leaves: &[&str], repo: &[&str]) {
        self.sys.programs.borrow_mut().insert("brew".into());
        let mut b = self.sys.brew.borrow_mut();
        b.installed = leaves.iter().map(|s| s.to_string()).collect();
        b.repo = repo.iter().chain(leaves).map(|s| s.to_string()).collect();
    }

    pub fn brew_installed(&self, name: &str) -> bool {
        self.sys.brew.borrow().installed.contains(name)
    }

    /// The user runs `apt install <name>` by hand.
    pub fn apt_add(&self, name: &str) {
        self.sys.apt_manual.borrow_mut().insert(name.into());
        self.sys.touch_dpkg();
    }

    /// The user runs `apt remove <name>` by hand.
    pub fn apt_remove(&self, name: &str) {
        self.sys.apt_manual.borrow_mut().remove(name);
        self.sys.touch_dpkg();
    }

    pub fn apt_installed(&self, name: &str) -> bool {
        self.sys.apt_manual.borrow().contains(name)
    }

    /// An `App` wired to this machine, for driving library functions directly.
    pub fn with_app<R>(
        &self,
        script: &Script,
        interactive: bool,
        f: impl FnOnce(&App) -> R,
    ) -> (R, String) {
        let (ui, buf): (Ui, SharedBuf) = Ui::capture();
        let app = App {
            paths: self.paths.clone(),
            runner: &self.sys,
            prompter: script,
            ui: &ui,
            dry_run: false,
            interactive,
            hostname: Some(HOST.into()),
            privilege: Default::default(),
        };
        let r = f(&app);
        (r, buf.contents())
    }

    pub fn write_pixi(&self, text: &str) {
        fs::create_dir_all(self.paths.pixi_manifest.parent().unwrap()).unwrap();
        fs::write(&self.paths.pixi_manifest, text).unwrap();
    }

    pub fn read(&self, p: &Path) -> String {
        fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    pub fn packages_toml(&self) -> String {
        self.read(&self.repo.join("packages.toml"))
    }

    pub fn local_toml(&self) -> String {
        self.read(&self.paths.local_toml())
    }

    /// Run `adopt <args>` against this machine. Returns the result and captured output.
    pub fn run(&self, args: &[&str], script: &Script) -> (Result<()>, String) {
        self.run_as(args, script, true)
    }

    /// Like [`Env::run`], choosing whether a terminal is attached.
    pub fn run_as(
        &self,
        args: &[&str],
        script: &Script,
        interactive: bool,
    ) -> (Result<()>, String) {
        let cli =
            Cli::try_parse_from(std::iter::once("adopt").chain(args.iter().copied())).unwrap();
        let (ui, buf): (Ui, SharedBuf) = Ui::capture();
        let app = App {
            paths: self.paths.clone(),
            runner: &self.sys,
            prompter: script,
            ui: &ui,
            dry_run: cli.dry_run,
            interactive,
            hostname: Some(HOST.into()),
            privilege: Default::default(),
        };
        let r = app.run(&cli);
        (r, buf.contents())
    }

    /// `adopt <args>` that must succeed and use up exactly `answers`.
    pub fn ok(&self, args: &[&str], answers: impl IntoIterator<Item = Answer>) -> String {
        let script = Script::new(answers);
        let (r, out) = self.run(args, &script);
        if let Err(e) = r {
            panic!("adopt {args:?} failed: {e:#}\n--- output ---\n{out}");
        }
        script.assert_done();
        out
    }

    /// `adopt <args>` with no terminal (systemd timer, cloud-init). Must succeed.
    pub fn batch(&self, args: &[&str]) -> String {
        let script = Script::default();
        let (r, out) = self.run_as(args, &script, false);
        if let Err(e) = r {
            panic!("adopt {args:?} failed: {e:#}\n--- output ---\n{out}");
        }
        out
    }

    /// Another machine replaces packages.toml and pushes it.
    pub fn publish(&self, packages_toml: &str) {
        let laptop = self.dir.path().join("laptop");
        if laptop.exists() {
            self.git(&laptop, &["pull", "--quiet", "--rebase"]);
        } else {
            self.other_clone("laptop");
        }
        fs::write(laptop.join("packages.toml"), packages_toml).unwrap();
        self.git(&laptop, &["add", "packages.toml"]);
        self.git(
            &laptop,
            &["commit", "--quiet", "-m", "laptop: edit packages"],
        );
        self.git(&laptop, &["push", "--quiet"]);
    }

    /// Number of logged commands containing `needle`.
    pub fn count_runs(&self, needle: &str) -> usize {
        self.sys
            .log
            .borrow()
            .iter()
            .filter(|l| l.contains(needle))
            .count()
    }

    pub fn init(&self, groups: &str) {
        self.ok(&["init", "--yes", "--groups", groups], []);
    }
}

pub const PIXI_TWO_ENVS: &str = r#"version = 1

[envs.setproxy]
channels = ["https://chenghongxu.github.io/packages/conda", "conda-forge"]
dependencies = { setproxy = "*" }
exposed = { setproxy = "setproxy" }

[envs.ripgrep]
channels = ["conda-forge"]
dependencies = { ripgrep = "*" }
exposed = { rg = "rg" }
"#;
