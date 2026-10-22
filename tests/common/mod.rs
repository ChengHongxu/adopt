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
        let Some(pos) = args.iter().position(|a| *a == "install" || *a == "remove") else {
            return 64;
        };
        let names = &args[pos + 1..];
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
        let cli =
            Cli::try_parse_from(std::iter::once("adopt").chain(args.iter().copied())).unwrap();
        let (ui, buf): (Ui, SharedBuf) = Ui::capture();
        let app = App {
            paths: self.paths.clone(),
            runner: &self.sys,
            prompter: script,
            ui: &ui,
            dry_run: cli.dry_run,
            interactive: true,
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
