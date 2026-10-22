//! The only place adopt spawns processes. Everything else builds a [`Cmd`] and hands
//! it to a [`CommandRunner`], so tests can substitute a fake system.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Cmd {
    pub fn new(program: impl Into<String>) -> Cmd {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Cmd {
        self.env.push((k.into(), v.into()));
        self
    }

    /// Wrap in `sudo [-n] [env K=V ...] program args`. Going through `env` keeps the
    /// variables even when sudoers resets the environment.
    pub fn sudo(self, non_interactive: bool) -> Cmd {
        let mut out = Cmd::new("sudo");
        if non_interactive {
            out.args.push("-n".into());
        }
        if !self.env.is_empty() {
            out.args.push("env".into());
            out.args
                .extend(self.env.iter().map(|(k, v)| format!("{k}={v}")));
        }
        out.args.push(self.program);
        out.args.extend(self.args);
        out
    }

    /// The argv after stripping a `sudo [-n] [env K=V..]` prefix; lets fakes route by program.
    pub fn unwrapped(&self) -> (&str, &[String]) {
        if self.program != "sudo" {
            return (&self.program, &self.args);
        }
        let mut rest = &self.args[..];
        if rest.first().is_some_and(|a| a == "-n") {
            rest = &rest[1..];
        }
        if rest.first().is_some_and(|a| a == "env") {
            rest = &rest[1..];
            while rest.first().is_some_and(|a| a.contains('=')) {
                rest = &rest[1..];
            }
        }
        match rest.split_first() {
            Some((p, a)) => (p, a),
            None => (&self.program, &self.args),
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let env = self.env.iter().map(|(k, v)| format!("{k}={v}"));
        for word in env
            .chain(std::iter::once(self.program.clone()))
            .chain(self.args.iter().cloned())
        {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            f.write_str(&shell_quote(&word))?;
        }
        Ok(())
    }
}

fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(stdout: impl Into<String>) -> Output {
        Output {
            code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn fail(code: i32, stderr: impl Into<String>) -> Output {
        Output {
            code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    pub fn success(&self) -> bool {
        self.code == 0
    }

    /// Last few lines of stderr (or stdout), for error messages.
    pub fn tail(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let lines: Vec<&str> = text.trim_end().lines().collect();
        lines[lines.len().saturating_sub(4)..].join("\n")
    }
}

pub trait CommandRunner {
    /// Run with stdin closed and output captured.
    fn run(&self, cmd: &Cmd) -> Result<Output>;
    /// Run attached to the terminal (sudo password prompts, progress bars).
    /// Only the exit code is returned.
    fn run_attached(&self, cmd: &Cmd) -> Result<i32>;
    /// Whether `program` resolves on PATH.
    fn which(&self, program: &str) -> bool;
}

pub struct SystemRunner;

impl SystemRunner {
    fn command(cmd: &Cmd) -> Command {
        let mut c = Command::new(&cmd.program);
        c.args(&cmd.args);
        for (k, v) in &cmd.env {
            c.env(k, v);
        }
        c
    }
}

impl CommandRunner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let out = Self::command(cmd)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("failed to run `{}`", cmd.program))?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn run_attached(&self, cmd: &Cmd) -> Result<i32> {
        let status = Self::command(cmd)
            .status()
            .with_context(|| format!("failed to run `{}`", cmd.program))?;
        Ok(status.code().unwrap_or(-1))
    }

    fn which(&self, program: &str) -> bool {
        find_on_path(program).is_some()
    }
}

pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_wraps_env_through_env_command() {
        let c = Cmd::new("apt-get")
            .args(["install", "-y", "htop"])
            .env("DEBIAN_FRONTEND", "noninteractive");
        let s = c.clone().sudo(true);
        assert_eq!(
            s.to_string(),
            "sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y htop"
        );
        assert_eq!(s.unwrapped(), ("apt-get", &c.args[..]));
    }

    #[test]
    fn display_quotes_only_when_needed() {
        let c = Cmd::new("dnf").args(["repoquery", "--qf", "%{name}\n", "it's"]);
        assert_eq!(c.to_string(), "dnf repoquery --qf '%{name}\n' 'it'\\''s'");
    }
}
