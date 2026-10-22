//! pixi global. Listing reads the manifest TOML instead of parsing pixi's output;
//! install/remove edit `[envs.*]` in place and then run `pixi global sync` once.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use toml_edit::Table;

use super::{Adapter, Ctx, Op, Outcome, PkgSpec};
use crate::model::ManagerId;
use crate::paths::Paths;
use crate::runner::{Cmd, CommandRunner};
use crate::state::{PixiManifest, write_atomic};

pub struct Pixi {
    manifest: PathBuf,
}

impl Pixi {
    pub fn new(manifest: PathBuf) -> Pixi {
        Pixi { manifest }
    }

    fn sync_cmd() -> Cmd {
        Cmd::new("pixi").args(["global", "sync"])
    }

    /// Apply manifest edits, then sync. If sync fails the manifest is restored, so
    /// the next run still sees the envs as missing rather than wrongly installed.
    fn edit_and_sync(
        &self,
        cx: &Ctx,
        pkgs: &[PkgSpec],
        edit: impl Fn(&mut PixiManifest, &PkgSpec),
    ) -> Result<Vec<Outcome>> {
        if pkgs.is_empty() {
            return Ok(Vec::new());
        }
        let mut m = PixiManifest::load(&self.manifest)?;
        let existed = m.file.exists();
        let backup = m.file.original().to_string();
        for p in pkgs {
            edit(&mut m, p);
        }
        if cx.dry_run {
            cx.ui
                .would(format!("写入 {}", cx.paths.tilde(&self.manifest)));
            cx.ui.would(Self::sync_cmd().to_string());
            return Ok(ok_all(pkgs));
        }
        m.file.save()?;
        let code = cx.runner.run_attached(&Self::sync_cmd())?;
        if code == 0 {
            return Ok(ok_all(pkgs));
        }
        if existed {
            write_atomic(&self.manifest, &backup)
        } else {
            std::fs::remove_file(&self.manifest).map_err(Into::into)
        }
        .with_context(|| format!("回退 {} 失败", self.manifest.display()))?;
        let err = format!("pixi global sync 退出码 {code}，已回退清单");
        Ok(pkgs
            .iter()
            .map(|p| Outcome {
                logical: p.logical.clone(),
                result: Err(err.clone()),
            })
            .collect())
    }
}

fn ok_all(pkgs: &[PkgSpec]) -> Vec<Outcome> {
    pkgs.iter()
        .map(|p| Outcome {
            logical: p.logical.clone(),
            result: Ok(()),
        })
        .collect()
}

impl Adapter for Pixi {
    fn id(&self) -> ManagerId {
        ManagerId::Pixi
    }

    fn is_available(&self, runner: &dyn CommandRunner) -> bool {
        runner.which("pixi")
    }

    fn needs_root(&self) -> bool {
        false
    }

    fn list_user_installed(&self, _cx: &Ctx) -> Result<BTreeSet<String>> {
        Ok(PixiManifest::load(&self.manifest)?.env_names())
    }

    fn tables(&self, _cx: &Ctx) -> Result<BTreeMap<String, Table>> {
        Ok(PixiManifest::load(&self.manifest)?.envs())
    }

    fn watch_paths(&self, _paths: &Paths) -> Vec<PathBuf> {
        vec![self.manifest.clone()]
    }

    fn planned_commands(&self, pkgs: &[PkgSpec], _op: Op) -> Vec<Cmd> {
        if pkgs.is_empty() {
            Vec::new()
        } else {
            vec![Self::sync_cmd()]
        }
    }

    fn install(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        self.edit_and_sync(cx, pkgs, |m, p| {
            if let Some(body) = &p.body {
                m.set_env(&p.native, body);
            }
        })
    }

    fn remove(&self, cx: &Ctx, pkgs: &[PkgSpec]) -> Result<Vec<Outcome>> {
        self.edit_and_sync(cx, pkgs, |m, p| {
            m.remove_env(&p.native);
        })
    }
}
