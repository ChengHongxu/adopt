//! The three state files. All of them are edited in place with toml_edit so
//! comments and formatting survive.

pub mod entry;
pub mod local;
pub mod pixi_manifest;
pub mod shared;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

pub use entry::{Entry, Mapping};
pub use local::{Applied, Failed, Local};
pub use pixi_manifest::PixiManifest;
pub use shared::Shared;

/// A TOML document plus the text it was loaded from, so callers can tell if it changed.
pub struct TomlFile {
    pub path: PathBuf,
    pub doc: DocumentMut,
    original: String,
}

impl TomlFile {
    /// Load `path`, or start from `default` if it does not exist (which counts as dirty).
    pub fn load_or(path: &Path, default: &str) -> Result<TomlFile> {
        let (text, original) = match fs::read_to_string(path) {
            Ok(t) => (t.clone(), t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (default.to_string(), String::new())
            }
            Err(e) => return Err(e).with_context(|| format!("无法读取 {}", path.display())),
        };
        let doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("{} 不是合法的 TOML", path.display()))?;
        Ok(TomlFile {
            path: path.to_path_buf(),
            doc,
            original,
        })
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.to_string() != self.original
    }

    /// The text currently on disk (empty if the file did not exist).
    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn save(&mut self) -> Result<()> {
        let text = self.doc.to_string();
        write_atomic(&self.path, &text)?;
        self.original = text;
        Ok(())
    }
}

pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;
    let name = path
        .file_name()
        .context("path has no file name")?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.adopt-tmp"));
    fs::write(&tmp, text).with_context(|| format!("无法写入 {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(())
}

/// `root[key]` as a mutable table, created as an implicit (header-less) table if absent.
pub fn table_mut<'a>(root: &'a mut Table, key: &str) -> &'a mut Table {
    let item = root.entry(key).or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    if !item.is_table() {
        // An inline table or scalar where we need a section: upgrade it in place.
        let upgraded = match std::mem::take(item) {
            Item::Value(Value::InlineTable(t)) => t.into_table(),
            _ => Table::new(),
        };
        *item = Item::Table(upgraded);
    }
    item.as_table_mut().expect("just ensured a table")
}

pub fn str_list(item: Option<&Item>) -> Vec<String> {
    item.and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// An array literal; long lists go one item per line so diffs stay readable.
pub fn str_array<'a>(items: impl IntoIterator<Item = &'a String>) -> Array {
    let mut arr: Array = items.into_iter().map(|s| s.as_str()).collect();
    if arr.len() > 6 {
        for v in arr.iter_mut() {
            v.decor_mut().set_prefix("\n  ");
        }
        arr.set_trailing("\n");
        arr.set_trailing_comma(true);
    }
    arr
}

/// Insert `value` into the string list at `table[key]`, keeping existing formatting.
/// Returns false if it was already there.
pub fn add_to_list(table: &mut Table, key: &str, value: &str) -> bool {
    let mut items: BTreeSet<String> = str_list(table.get(key)).into_iter().collect();
    if !items.insert(value.to_string()) {
        return false;
    }
    table.insert(key, Item::Value(Value::Array(str_array(&items))));
    true
}

pub fn remove_from_list(table: &mut Table, key: &str, value: &str) -> bool {
    let mut items = str_list(table.get(key));
    let before = items.len();
    items.retain(|s| s != value);
    if items.len() == before {
        return false;
    }
    if items.is_empty() {
        table.remove(key);
    } else {
        table.insert(key, Item::Value(Value::Array(str_array(&items))));
    }
    true
}

/// Make a table safe to graft into another document: forget its position and
/// leading comments from the source file.
pub fn detach(t: &mut Table) {
    t.set_position(None);
    t.decor_mut().clear();
    t.set_implicit(false);
    for (_, item) in t.iter_mut() {
        if let Item::Table(sub) = item {
            detach(sub);
        }
    }
}

/// Owned `Table` from either `[a.b]` or `b = { ... }`.
pub fn owned_table(item: &Item) -> Option<Table> {
    match item {
        Item::Table(t) => Some(t.clone()),
        Item::Value(Value::InlineTable(t)) => Some(t.clone().into_table()),
        _ => None,
    }
}

/// Formatting-independent hash of a table, used to notice user edits to pixi envs.
pub fn fingerprint(t: &Table) -> String {
    let mut s = String::new();
    canon_table(t, &mut s);
    format!("{:016x}", fnv1a64(s.as_bytes()))
}

fn canon_table(t: &dyn TableLike, out: &mut String) {
    let mut entries: Vec<(&str, &Item)> = t.iter().filter(|(_, i)| !i.is_none()).collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    out.push('{');
    for (k, item) in entries {
        out.push_str(&format!("{k:?}="));
        canon_item(item, out);
        out.push(',');
    }
    out.push('}');
}

fn canon_item(item: &Item, out: &mut String) {
    match item {
        Item::None => {}
        Item::Value(v) => canon_value(v, out),
        Item::Table(t) => canon_table(t, out),
        Item::ArrayOfTables(a) => {
            out.push('[');
            for t in a.iter() {
                canon_table(t, out);
                out.push(',');
            }
            out.push(']');
        }
    }
}

fn canon_value(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => out.push_str(&format!("{:?}", s.value())),
        Value::Integer(i) => out.push_str(&i.value().to_string()),
        Value::Float(f) => out.push_str(&f.value().to_string()),
        Value::Boolean(b) => out.push_str(&b.value().to_string()),
        Value::Datetime(d) => out.push_str(&d.value().to_string()),
        Value::Array(a) => {
            out.push('[');
            for v in a.iter() {
                canon_value(v, out);
                out.push(',');
            }
            out.push(']');
        }
        Value::InlineTable(t) => canon_table(t, out),
    }
}

/// FNV-1a: tiny and, unlike std's hasher, stable across Rust versions.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> Table {
        src.parse::<DocumentMut>().unwrap().as_table().clone()
    }

    #[test]
    fn fingerprint_ignores_formatting_and_key_order() {
        let a = table("channels = [\"conda-forge\"]\ndependencies = { rg = \"*\" }\n");
        let b = table("# c\ndependencies={rg='*'}\nchannels = [ 'conda-forge', ]\n");
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let c = table("channels = [\"conda-forge\"]\ndependencies = { rg = \">=14\" }\n");
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }

    #[test]
    fn long_lists_are_multiline() {
        let items: Vec<String> = (0..8).map(|i| format!("p{i}")).collect();
        let mut t = Table::new();
        t.insert("apt", Item::Value(Value::Array(str_array(&items))));
        let text = t.to_string();
        assert!(text.contains("\n  \"p0\","), "{text}");
    }
}
