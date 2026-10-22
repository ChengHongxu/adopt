//! `adopt status [--prompt]`: how many items are waiting for `adopt`, for shell
//! prompts. A cache hit costs a few `stat`s and two small file reads; the package
//! managers only run when a watched file changed.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use toml_edit::{DocumentMut, Item, value};

use crate::App;
use crate::cli::StatusArgs;
use crate::plan::{self, Inventory};
use crate::state::{Local, Shared, fnv1a64, shared, write_atomic};

pub fn run(app: &App, args: &StatusArgs) -> Result<()> {
    if args.prompt {
        // A prompt segment must never print an error or slow the shell down.
        if let Ok(n) = count(app)
            && n > 0
        {
            app.ui.line(n.to_string());
        }
        return Ok(());
    }
    let n = count(app)?;
    if n == 0 {
        app.ui.ok("没有待处理的条目");
    } else {
        app.ui.line(format!("{n} 个条目待处理，运行 `adopt` 处理"));
    }
    Ok(())
}

/// Pending count, from the cache when nothing it depends on changed.
pub fn count(app: &App) -> Result<usize> {
    let (local, repo) = app.open()?;
    let key = cache_key(app, &repo);
    if let Some(n) = read_cache(app, &key) {
        return Ok(n);
    }
    let shared = app.load_shared(&repo)?;
    compute_and_store(app, &local, &shared, &key)
}

/// Recompute and store the cache after a run changed things. Best effort.
pub fn refresh(app: &App) {
    if app.dry_run {
        return;
    }
    let Ok((local, repo)) = app.open() else {
        return;
    };
    let Ok(shared) = app.load_shared(&repo) else {
        return;
    };
    let key = cache_key(app, &repo);
    let _ = compute_and_store(app, &local, &shared, &key);
}

fn compute_and_store(app: &App, local: &Local, shared: &Shared, key: &str) -> Result<usize> {
    let adapters = app.adapters();
    let inv = Inventory::collect(&adapters, &app.ctx())?;
    let n = plan::pending_count(shared, local, &inv)?;
    let mut doc = DocumentMut::new();
    doc["key"] = value(key);
    doc["count"] = value(n as i64);
    let _ = write_atomic(&app.paths.status_cache(), &doc.to_string());
    Ok(n)
}

/// Everything the count depends on, as one string: the content of local.toml and
/// packages.toml plus mtime+size of each watched file. The adapter list itself is part of it, so
/// installing pixi or apt also invalidates the cache.
fn cache_key(app: &App, repo: &Path) -> String {
    let mut parts = vec![
        content(&app.paths.local_toml()),
        content(&repo.join(shared::FILE_NAME)),
    ];
    for a in app.adapters() {
        parts.push(a.id().key().to_string());
        for p in a.watch_paths(&app.paths) {
            parts.push(stamp(&p));
        }
    }
    format!("{:016x}", fnv1a64(parts.join("\n").as_bytes()))
}

fn content(p: &Path) -> String {
    let bytes = fs::read(p).unwrap_or_default();
    format!("{}#{:016x}", p.display(), fnv1a64(&bytes))
}

fn stamp(p: &Path) -> String {
    match fs::metadata(p) {
        Ok(m) => {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}:{}", p.display(), m.len())
        }
        Err(_) => format!("{}:-", p.display()),
    }
}

fn read_cache(app: &App, key: &str) -> Option<usize> {
    let doc: DocumentMut = fs::read_to_string(app.paths.status_cache())
        .ok()?
        .parse()
        .ok()?;
    if doc.get("key").and_then(Item::as_str) != Some(key) {
        return None;
    }
    doc.get("count")
        .and_then(Item::as_integer)
        .and_then(|n| usize::try_from(n).ok())
}
