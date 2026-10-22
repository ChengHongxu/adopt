//! `~/.pixi/manifests/pixi-global.toml`. adopt adds and removes `[envs.<name>]`
//! tables in place, so envs nobody has claimed yet (and comments) are left alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use toml_edit::{Item, Table};

use super::{TomlFile, detach, owned_table, table_mut};

const DEFAULT: &str = "version = 1\n";

pub struct PixiManifest {
    pub file: TomlFile,
}

impl PixiManifest {
    pub fn load(path: &Path) -> Result<PixiManifest> {
        Ok(PixiManifest {
            file: TomlFile::load_or(path, DEFAULT)?,
        })
    }

    pub fn envs(&self) -> BTreeMap<String, Table> {
        let Some(envs) = self.file.doc.get("envs").and_then(Item::as_table_like) else {
            return BTreeMap::new();
        };
        envs.iter()
            .filter_map(|(name, item)| {
                let mut t = owned_table(item)?;
                detach(&mut t);
                Some((name.to_string(), t))
            })
            .collect()
    }

    pub fn env_names(&self) -> BTreeSet<String> {
        self.envs().into_keys().collect()
    }

    pub fn set_env(&mut self, name: &str, body: &Table) {
        let mut t = body.clone();
        detach(&mut t);
        table_mut(self.file.doc.as_table_mut(), "envs").insert(name, Item::Table(t));
    }

    pub fn remove_env(&mut self, name: &str) -> bool {
        self.file
            .doc
            .get_mut("envs")
            .and_then(Item::as_table_like_mut)
            .is_some_and(|t| t.remove(name).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_preserve_unclaimed_envs_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixi-global.toml");
        std::fs::write(
            &path,
            "version = 1\n\n# mine\n[envs.setproxy]\nchannels = [\"conda-forge\"]\ndependencies = { setproxy = \"*\" }\nexposed = { setproxy = \"setproxy\" }\n\n[envs.bat]\nchannels = [\"conda-forge\"]\ndependencies = { bat = \"*\" }\nexposed = { bat = \"bat\" }\n",
        )
        .unwrap();
        let mut m = PixiManifest::load(&path).unwrap();
        assert_eq!(
            m.env_names().into_iter().collect::<Vec<_>>(),
            ["bat", "setproxy"]
        );

        let rg: Table = "channels = [\"conda-forge\"]\ndependencies = { ripgrep = \"*\" }\nexposed = { rg = \"rg\" }\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap()
            .as_table()
            .clone();
        m.set_env("ripgrep", &rg);
        assert!(m.remove_env("bat"));
        let text = m.file.doc.to_string();
        assert!(
            text.starts_with("version = 1\n\n# mine\n[envs.setproxy]"),
            "{text}"
        );
        assert!(
            text.contains("[envs.ripgrep]\nchannels = [\"conda-forge\"]"),
            "{text}"
        );
        assert!(!text.contains("bat"), "{text}");
        assert!(!text.contains("[envs]\n"), "{text}");
    }

    #[test]
    fn missing_manifest_starts_with_version() {
        let dir = tempfile::tempdir().unwrap();
        let m = PixiManifest::load(&dir.path().join("nope.toml")).unwrap();
        assert!(m.env_names().is_empty());
        assert_eq!(m.file.doc.to_string(), "version = 1\n");
    }
}
