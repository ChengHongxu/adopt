//! One package entry, as written under `[system.<name>]` or `[pixi.<name>]` in
//! packages.toml, or under `[local.system.<name>]` in local.toml.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use toml_edit::{Item, Table, Value};

use super::{detach, owned_table, str_array, str_list, table_mut};
use crate::model::{ManagerId, Section};

/// Keys adopt owns inside an entry; everything else in a pixi entry is pixi's.
pub const GROUPS: &str = "groups";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapping {
    /// Installed under a different name with this manager (`dnf = "@development-tools"`).
    Name(String),
    /// Not installed with this manager at all (`brew = false`).
    Skip,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub section: Section,
    /// Logical name: the key in the manifest.
    pub name: String,
    /// `None` means every machine.
    pub groups: Option<Vec<String>>,
    /// System entries only.
    pub mappings: BTreeMap<ManagerId, Mapping>,
    /// Pixi entries only: the env table exactly as pixi writes it, minus `groups`.
    pub body: Option<Table>,
}

impl Entry {
    pub fn system(name: &str) -> Entry {
        Entry {
            section: Section::System,
            name: name.into(),
            groups: None,
            mappings: BTreeMap::new(),
            body: None,
        }
    }

    pub fn pixi(name: &str, body: Table) -> Entry {
        Entry {
            section: Section::Pixi,
            name: name.into(),
            groups: None,
            mappings: BTreeMap::new(),
            body: Some(body),
        }
    }

    pub fn with_groups(mut self, groups: Option<Vec<String>>) -> Entry {
        self.groups = groups.filter(|g| !g.is_empty());
        self
    }

    /// Native package name for `manager`, or `None` if this entry is skipped there.
    pub fn native_for(&self, manager: ManagerId) -> Option<String> {
        match self.mappings.get(&manager) {
            Some(Mapping::Name(n)) => Some(n.clone()),
            Some(Mapping::Skip) => None,
            None => Some(self.name.clone()),
        }
    }

    pub fn applies_to(&self, machine_groups: &[String]) -> bool {
        match &self.groups {
            None => true,
            Some(gs) => gs.iter().any(|g| machine_groups.contains(g)),
        }
    }

    fn parse(section: Section, name: &str, item: &Item) -> Result<Entry> {
        let Some(table) = owned_table(item) else {
            bail!("[{section}.{name}] 必须是表，例如 `[{section}.{name}]` 或 `{name} = {{}}`");
        };
        let groups = table
            .contains_key(GROUPS)
            .then(|| str_list(table.get(GROUPS)));
        let mut entry = Entry {
            section,
            name: name.to_string(),
            groups: groups.filter(|g| !g.is_empty()),
            mappings: BTreeMap::new(),
            body: None,
        };
        match section {
            Section::System => {
                for m in ManagerId::SYSTEM {
                    match table.get(m.key()) {
                        None => {}
                        Some(v) if v.as_str().is_some() => {
                            entry
                                .mappings
                                .insert(m, Mapping::Name(v.as_str().unwrap().to_string()));
                        }
                        Some(v) if v.as_bool() == Some(false) => {
                            entry.mappings.insert(m, Mapping::Skip);
                        }
                        Some(v) if v.as_bool() == Some(true) => {}
                        Some(_) => bail!("[system.{name}] 的 {m} 只能是包名字符串或 false"),
                    }
                }
            }
            Section::Pixi => {
                let mut body = table;
                body.remove(GROUPS);
                detach(&mut body);
                entry.body = Some(body);
            }
        }
        Ok(entry)
    }

    fn to_table(&self) -> Table {
        let mut t = match &self.body {
            Some(b) => {
                let mut b = b.clone();
                detach(&mut b);
                b
            }
            None => Table::new(),
        };
        for (m, mapping) in &self.mappings {
            let v = match mapping {
                Mapping::Name(n) => Value::from(n.as_str()),
                Mapping::Skip => Value::from(false),
            };
            t.insert(m.key(), Item::Value(v));
        }
        if let Some(gs) = &self.groups {
            t.insert(GROUPS, Item::Value(Value::Array(str_array(gs))));
        }
        t.set_implicit(false);
        t
    }
}

/// All entries of `section` under `root` (the document root, or `[local]`).
pub fn parse_section(root: &Table, section: Section) -> Result<Vec<Entry>> {
    let Some(item) = root.get(section.key()) else {
        return Ok(Vec::new());
    };
    let Some(table) = item.as_table_like() else {
        bail!("`{section}` 必须是表")
    };
    table
        .iter()
        .map(|(name, item)| Entry::parse(section, name, item))
        .collect()
}

/// Add or replace an entry.
pub fn upsert(root: &mut Table, entry: &Entry) {
    table_mut(root, entry.section.key()).insert(&entry.name, Item::Table(entry.to_table()));
}

pub fn remove(root: &mut Table, section: Section, name: &str) -> bool {
    root.get_mut(section.key())
        .and_then(Item::as_table_like_mut)
        .is_some_and(|t| t.remove(name).is_some())
}

/// Set one manager mapping on an existing system entry without touching the rest of it.
pub fn set_mapping(
    root: &mut Table,
    name: &str,
    manager: ManagerId,
    mapping: &Mapping,
) -> Result<()> {
    let sys = table_mut(root, Section::System.key());
    let Some(item) = sys.get_mut(name) else {
        bail!("清单里没有 system.{name}")
    };
    let Some(t) = item.as_table_like_mut() else {
        bail!("system.{name} 不是表")
    };
    let v = match mapping {
        Mapping::Name(n) => Value::from(n.as_str()),
        Mapping::Skip => Value::from(false),
    };
    t.insert(manager.key(), Item::Value(v));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    const SAMPLE: &str = r#"version = 1

# build tools
[system.build-essential]
dnf = "@development-tools" # group on fedora
brew = false
groups = ["work"]

[system.htop]

[pixi.setproxy]
channels = ["conda-forge", "https://chenghongxu.github.io/packages/conda"]
dependencies = { setproxy = "*" }
exposed = { setproxy = "setproxy", _setproxy = "_setproxy" }
groups = ["homelab"]
"#;

    #[test]
    fn parses_mappings_groups_and_bodies() {
        let doc: DocumentMut = SAMPLE.parse().unwrap();
        let sys = parse_section(doc.as_table(), Section::System).unwrap();
        assert_eq!(sys.len(), 2);
        let be = &sys[0];
        assert_eq!(
            be.native_for(ManagerId::Dnf).as_deref(),
            Some("@development-tools")
        );
        assert_eq!(be.native_for(ManagerId::Brew), None);
        assert_eq!(
            be.native_for(ManagerId::Apt).as_deref(),
            Some("build-essential")
        );
        assert!(be.applies_to(&["work".into()]));
        assert!(!be.applies_to(&["homelab".into()]));
        assert!(sys[1].applies_to(&[]));

        let pixi = parse_section(doc.as_table(), Section::Pixi).unwrap();
        let body = pixi[0].body.as_ref().unwrap();
        assert!(body.contains_key("exposed"));
        assert!(
            !body.contains_key("groups"),
            "groups is adopt's, not pixi's"
        );
    }

    #[test]
    fn edits_keep_comments_and_empty_tables() {
        let mut doc: DocumentMut = SAMPLE.parse().unwrap();
        upsert(doc.as_table_mut(), &Entry::system("ripgrep"));
        set_mapping(
            doc.as_table_mut(),
            "htop",
            ManagerId::Brew,
            &Mapping::Name("htop-osx".into()),
        )
        .unwrap();
        assert!(remove(doc.as_table_mut(), Section::Pixi, "setproxy"));
        let text = doc.to_string();
        assert!(text.contains("# build tools"), "{text}");
        assert!(text.contains("# group on fedora"), "{text}");
        assert!(text.contains("[system.ripgrep]"), "{text}");
        assert!(
            text.contains("[system.htop]\nbrew = \"htop-osx\""),
            "{text}"
        );
        assert!(!text.contains("setproxy"), "{text}");
        assert!(
            !text.contains("[system]\n"),
            "parent stays implicit: {text}"
        );
    }

    #[test]
    fn rejects_bad_values() {
        let doc: DocumentMut = "[system.x]\napt = 3\n".parse().unwrap();
        assert!(parse_section(doc.as_table(), Section::System).is_err());
        let doc: DocumentMut = "[system]\nx = true\n".parse().unwrap();
        assert!(parse_section(doc.as_table(), Section::System).is_err());
    }
}
