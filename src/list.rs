//! `adopt list`: the shared manifest and this machine's own entries.

use anyhow::Result;

use crate::App;
use crate::model::Section;
use crate::plan::Inventory;
use crate::state::Entry;

pub fn run(app: &App) -> Result<()> {
    let (local, repo) = app.open()?;
    let shared = app.load_shared(&repo)?;
    let groups = local.groups();
    let ui = app.ui;
    let inv = Inventory::collect(&app.adapters(), &app.ctx())?;

    ui.line(format!(
        "本机分组：{}",
        if groups.is_empty() {
            "（无）".to_string()
        } else {
            groups.join(", ")
        }
    ));
    for section in Section::ALL {
        let manager = inv.manager(section);
        let exceptions = local.exceptions(section);
        let shared_entries = shared.entries(section)?;
        let local_entries = local.entries(section)?;
        if shared_entries.is_empty() && local_entries.is_empty() {
            continue;
        }
        ui.line("");
        ui.line(ui.bold(format!("[{}]", section.key())));
        for e in &shared_entries {
            let note = if exceptions.contains(&e.name) {
                "本机例外".to_string()
            } else if !e.applies_to(&groups) {
                "不属于本机分组".to_string()
            } else {
                String::new()
            };
            ui.line(row(e, manager, "共享", &note));
        }
        for e in &local_entries {
            ui.line(row(e, manager, "仅本机", ""));
        }
    }
    Ok(())
}

fn row(e: &Entry, manager: Option<crate::model::ManagerId>, origin: &str, note: &str) -> String {
    let mut cols = vec![format!("  {:<24}", e.name), format!("{origin:<6}")];
    if let Some(gs) = &e.groups {
        cols.push(format!("@{}", gs.join(",")));
    }
    if let Some(m) = manager {
        match e.native_for(m) {
            Some(n) if n != e.name => cols.push(format!("{m}: {n}")),
            None => cols.push(format!("{m}: 不装")),
            _ => {}
        }
    }
    if !note.is_empty() {
        cols.push(format!("（{note}）"));
    }
    cols.join(" ").trim_end().to_string()
}
