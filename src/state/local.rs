//! `~/.config/adopt/local.toml`: this machine's state. Never committed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;
use toml_edit::{InlineTable, Item, Table, Value};

use super::entry::{self, Entry};
use super::{TomlFile, add_to_list, remove_from_list, str_array, str_list, table_mut};
use crate::model::{ManagerId, Section};
use crate::paths::Paths;

const TEMPLATE: &str = "\
# adopt 的本机状态，不进 git。可以手动编辑。
version = 1
";

/// A package adopt has reconciled on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub manager: ManagerId,
    pub native: String,
    /// adopt ran the install itself. Only these are removed when the shared entry goes away.
    pub by_adopt: bool,
    /// Fingerprint of the entry's body as last applied: pixi env table, or npm `version`.
    pub fp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub manager: ManagerId,
    pub native: String,
    pub error: String,
}

pub struct Local {
    pub file: TomlFile,
}

impl Local {
    pub fn load(paths: &Paths) -> Result<Local> {
        Ok(Local {
            file: TomlFile::load_or(&paths.local_toml(), TEMPLATE)?,
        })
    }

    fn root(&self) -> &Table {
        self.file.doc.as_table()
    }

    fn root_mut(&mut self) -> &mut Table {
        self.file.doc.as_table_mut()
    }

    pub fn repo(&self) -> Option<PathBuf> {
        self.root()
            .get("repo")
            .and_then(Item::as_str)
            .map(PathBuf::from)
    }

    pub fn set_repo(&mut self, p: &std::path::Path) {
        self.root_mut()
            .insert("repo", toml_edit::value(p.display().to_string()));
    }

    pub fn groups(&self) -> Vec<String> {
        str_list(self.root().get("groups"))
    }

    pub fn set_groups(&mut self, groups: &[String]) {
        self.root_mut()
            .insert("groups", Item::Value(Value::Array(str_array(groups))));
    }

    // ---- packages only this machine has ----

    pub fn entries(&self, section: Section) -> Result<Vec<Entry>> {
        match self.root().get("local").and_then(Item::as_table) {
            Some(t) => entry::parse_section(t, section),
            None => Ok(Vec::new()),
        }
    }

    pub fn upsert_entry(&mut self, e: &Entry) {
        let mut e = e.clone();
        e.groups = None;
        entry::upsert(table_mut(self.root_mut(), "local"), &e);
    }

    pub fn remove_entry(&mut self, section: Section, name: &str) -> bool {
        entry::remove(table_mut(self.root_mut(), "local"), section, name)
    }

    // ---- shared entries this machine opts out of (logical names) ----

    pub fn exceptions(&self, section: Section) -> BTreeSet<String> {
        self.list("exceptions", section.key())
    }

    pub fn add_exception(&mut self, section: Section, name: &str) -> bool {
        add_to_list(
            table_mut(self.root_mut(), "exceptions"),
            section.key(),
            name,
        )
    }

    pub fn remove_exception(&mut self, section: Section, name: &str) -> bool {
        remove_from_list(
            table_mut(self.root_mut(), "exceptions"),
            section.key(),
            name,
        )
    }

    // ---- installed packages never to ask about (native names) ----

    pub fn ignored(&self, manager: ManagerId) -> BTreeSet<String> {
        self.list("ignore", manager.key())
    }

    pub fn add_ignore(&mut self, manager: ManagerId, native: &str) -> bool {
        add_to_list(table_mut(self.root_mut(), "ignore"), manager.key(), native)
    }

    // ---- what was installed before adopt (native names) ----

    /// `None` if no baseline was ever taken for this manager.
    pub fn baseline(&self, manager: ManagerId) -> Option<BTreeSet<String>> {
        let t = self.root().get("baseline").and_then(Item::as_table)?;
        t.contains_key(manager.key())
            .then(|| str_list(t.get(manager.key())).into_iter().collect())
    }

    pub fn set_baseline(&mut self, manager: ManagerId, names: &BTreeSet<String>) {
        table_mut(self.root_mut(), "baseline")
            .insert(manager.key(), Item::Value(Value::Array(str_array(names))));
    }

    // ---- reconciled packages ----

    pub fn applied(&self, section: Section) -> BTreeMap<String, Applied> {
        self.inline_map("applied", section, |t| {
            Some(Applied {
                manager: ManagerId::parse(t.get("manager")?.as_str()?)?,
                native: t.get("native")?.as_str()?.to_string(),
                by_adopt: t.get("by_adopt").and_then(Item::as_bool).unwrap_or(false),
                fp: t.get("fp").and_then(Item::as_str).map(str::to_string),
            })
        })
    }

    pub fn set_applied(&mut self, section: Section, name: &str, a: &Applied) {
        let mut t = InlineTable::new();
        t.insert("manager", a.manager.key().into());
        t.insert("native", a.native.as_str().into());
        t.insert("by_adopt", a.by_adopt.into());
        if let Some(fp) = &a.fp {
            t.insert("fp", fp.as_str().into());
        }
        self.section_table("applied", section)
            .insert(name, toml_edit::value(t));
    }

    pub fn remove_applied(&mut self, section: Section, name: &str) -> bool {
        self.section_table("applied", section)
            .remove(name)
            .is_some()
    }

    pub fn failed(&self, section: Section) -> BTreeMap<String, Failed> {
        self.inline_map("failed", section, |t| {
            Some(Failed {
                manager: ManagerId::parse(t.get("manager")?.as_str()?)?,
                native: t.get("native")?.as_str()?.to_string(),
                error: t
                    .get("error")
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
    }

    pub fn set_failed(&mut self, section: Section, name: &str, f: &Failed) {
        let mut t = InlineTable::new();
        t.insert("manager", f.manager.key().into());
        t.insert("native", f.native.as_str().into());
        t.insert("error", f.error.as_str().into());
        self.section_table("failed", section)
            .insert(name, toml_edit::value(t));
    }

    pub fn clear_failed(&mut self, section: Section, name: &str) -> bool {
        self.section_table("failed", section).remove(name).is_some()
    }

    // ---- helpers ----

    fn list(&self, table: &str, key: &str) -> BTreeSet<String> {
        let t = self.root().get(table).and_then(Item::as_table_like);
        str_list(t.and_then(|t| t.get(key))).into_iter().collect()
    }

    fn section_table(&mut self, table: &str, section: Section) -> &mut Table {
        let outer = table_mut(self.root_mut(), table);
        let inner = table_mut(outer, section.key());
        inner.set_implicit(false);
        inner
    }

    fn inline_map<T>(
        &self,
        table: &str,
        section: Section,
        parse: impl Fn(&dyn toml_edit::TableLike) -> Option<T>,
    ) -> BTreeMap<String, T> {
        let Some(t) = self
            .root()
            .get(table)
            .and_then(Item::as_table_like)
            .and_then(|t| t.get(section.key()))
            .and_then(Item::as_table_like)
        else {
            return BTreeMap::new();
        };
        t.iter()
            .filter_map(|(k, v)| Some((k.to_string(), parse(v.as_table_like()?)?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_table() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::sandbox(dir.path());
        let mut l = Local::load(&paths).unwrap();
        l.set_repo(&paths.home.join("dots"));
        l.set_groups(&["work".into()]);
        l.upsert_entry(&Entry::system("libssl-dev").with_groups(Some(vec!["x".into()])));
        l.add_exception(Section::System, "tmux");
        l.add_ignore(ManagerId::Apt, "linux-image-generic");
        l.set_baseline(
            ManagerId::Apt,
            &["bash".to_string(), "coreutils".to_string()].into(),
        );
        let a = Applied {
            manager: ManagerId::Apt,
            native: "htop".into(),
            by_adopt: true,
            fp: None,
        };
        l.set_applied(Section::System, "htop", &a);
        let f = Failed {
            manager: ManagerId::Apt,
            native: "nope".into(),
            error: "E: Unable".into(),
        };
        l.set_failed(Section::System, "nope", &f);
        l.file.save().unwrap();

        let l = Local::load(&paths).unwrap();
        assert_eq!(l.repo(), Some(paths.home.join("dots")));
        assert_eq!(l.groups(), ["work"]);
        let e = &l.entries(Section::System).unwrap()[0];
        assert_eq!(e.name, "libssl-dev");
        assert_eq!(e.groups, None, "local entries never carry groups");
        assert!(l.exceptions(Section::System).contains("tmux"));
        assert!(l.ignored(ManagerId::Apt).contains("linux-image-generic"));
        assert_eq!(l.baseline(ManagerId::Apt).unwrap().len(), 2);
        assert_eq!(l.baseline(ManagerId::Dnf), None);
        assert_eq!(l.applied(Section::System)["htop"], a);
        assert_eq!(l.failed(Section::System)["nope"], f);
        let text = l.file.doc.to_string();
        assert!(text.contains("[applied.system]\nhtop = {"), "{text}");
    }
}
