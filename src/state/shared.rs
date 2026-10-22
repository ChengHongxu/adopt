//! `<repo>/packages.toml`: the manifest every machine syncs from. The only state in git.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::entry::{self, Entry, Mapping};
use super::{TomlFile, str_list};
use crate::model::{ManagerId, Section};

pub const FILE_NAME: &str = "packages.toml";
pub const VERSION: i64 = 1;

const TEMPLATE: &str = "\
# 共享软件清单，由 adopt 维护（https://github.com/ChengHongxu/adopt）。
# 可以手动编辑，注释会保留。
#
# [system.<名字>]      系统包；默认在 apt/dnf/brew 里同名
#   dnf = \"别名\"        某个管理器里名字不同
#   brew = false        这个管理器不装
#   groups = [\"work\"]   只装到这些分组的机器（不写就是所有机器）
# [pixi.<env>]         pixi global 环境，原样保存 pixi-global.toml 里的表

version = 1
";

pub struct Shared {
    pub file: TomlFile,
}

impl Shared {
    pub fn path(repo: &Path) -> PathBuf {
        repo.join(FILE_NAME)
    }

    pub fn load(repo: &Path) -> Result<Shared> {
        let file = TomlFile::load_or(&Self::path(repo), TEMPLATE)?;
        if let Some(v) = file.doc.get("version").and_then(|v| v.as_integer())
            && v > VERSION
        {
            bail!("{} 的 version = {v}，需要更新 adopt", file.path.display());
        }
        Ok(Shared { file })
    }

    pub fn entries(&self, section: Section) -> Result<Vec<Entry>> {
        entry::parse_section(self.file.doc.as_table(), section)
    }

    pub fn get(&self, section: Section, name: &str) -> Result<Option<Entry>> {
        Ok(self.entries(section)?.into_iter().find(|e| e.name == name))
    }

    pub fn upsert(&mut self, e: &Entry) {
        entry::upsert(self.file.doc.as_table_mut(), e);
    }

    pub fn remove(&mut self, section: Section, name: &str) -> bool {
        entry::remove(self.file.doc.as_table_mut(), section, name)
    }

    pub fn set_mapping(&mut self, name: &str, manager: ManagerId, mapping: &Mapping) -> Result<()> {
        entry::set_mapping(self.file.doc.as_table_mut(), name, manager, mapping)
    }

    /// Every group name used anywhere in the manifest.
    pub fn groups(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for s in Section::ALL {
            if let Some(t) = self.file.doc.get(s.key()).and_then(|i| i.as_table_like()) {
                for (_, item) in t.iter() {
                    if let Some(e) = item.as_table_like() {
                        out.extend(str_list(e.get(entry::GROUPS)));
                    }
                }
            }
        }
        out
    }
}
