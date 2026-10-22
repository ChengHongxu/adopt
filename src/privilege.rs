//! How to get root for system package managers: already root, via sudo, or not at all.
//! Detected lazily so commands that never touch system packages (`status`) never probe.

use std::cell::RefCell;

use crate::runner::{Cmd, CommandRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Privilege {
    Root,
    /// `batch`: use `sudo -n` (no password prompt possible).
    Sudo {
        batch: bool,
    },
    /// Why system packages have to be skipped.
    Unavailable(String),
}

/// Cached privilege state for one run.
#[derive(Debug, Default)]
pub struct PrivilegeCell {
    state: RefCell<Option<Privilege>>,
    /// Interactive sudo already validated with `sudo -v`.
    validated: RefCell<bool>,
}

impl PrivilegeCell {
    pub fn get(&self, runner: &dyn CommandRunner, interactive: bool) -> Privilege {
        if let Some(p) = self.state.borrow().as_ref() {
            return p.clone();
        }
        let p = detect(runner, interactive);
        *self.state.borrow_mut() = Some(p.clone());
        p
    }

    /// Make sure root is usable right now, prompting for the sudo password once if
    /// needed. Returns the reason when it is not.
    pub fn ensure(
        &self,
        runner: &dyn CommandRunner,
        interactive: bool,
    ) -> std::result::Result<(), String> {
        match self.get(runner, interactive) {
            Privilege::Root | Privilege::Sudo { batch: true } => Ok(()),
            Privilege::Unavailable(why) => Err(why),
            Privilege::Sudo { batch: false } => {
                if *self.validated.borrow() {
                    return Ok(());
                }
                let ok = runner
                    .run_attached(&Cmd::new("sudo").arg("-v"))
                    .is_ok_and(|code| code == 0);
                if ok {
                    *self.validated.borrow_mut() = true;
                    Ok(())
                } else {
                    let why = "sudo 验证失败".to_string();
                    *self.state.borrow_mut() = Some(Privilege::Unavailable(why.clone()));
                    Err(why)
                }
            }
        }
    }

    /// `cmd` as it must be run to get root, or why it cannot be.
    pub fn elevate(
        &self,
        runner: &dyn CommandRunner,
        interactive: bool,
        cmd: Cmd,
    ) -> std::result::Result<Cmd, String> {
        match self.get(runner, interactive) {
            Privilege::Root => Ok(cmd),
            Privilege::Sudo { batch } => Ok(cmd.sudo(batch)),
            Privilege::Unavailable(why) => Err(why),
        }
    }
}

fn detect(runner: &dyn CommandRunner, interactive: bool) -> Privilege {
    let uid = runner
        .run(&Cmd::new("id").arg("-u"))
        .ok()
        .filter(|o| o.success())
        .map(|o| o.stdout.trim().to_string());
    if uid.as_deref() == Some("0") {
        return Privilege::Root;
    }
    if !runner.which("sudo") {
        return Privilege::Unavailable("不是 root，也没有 sudo".into());
    }
    if interactive {
        return Privilege::Sudo { batch: false };
    }
    let passwordless = runner
        .run(&Cmd::new("sudo").args(["-n", "true"]))
        .is_ok_and(|o| o.success());
    if passwordless {
        Privilege::Sudo { batch: true }
    } else {
        Privilege::Unavailable("sudo 需要密码，非交互模式下无法使用".into())
    }
}
