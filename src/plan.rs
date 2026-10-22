//! Pure comparison of "what is installed" against "what the manifests say".
//! No I/O here, so every rule can be unit-tested directly.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use toml_edit::Table;

use crate::adapters::{Adapter, Ctx};
use crate::model::{ManagerId, Section};
use crate::state::{Local, Shared};

/// What one adapter reported.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub names: BTreeSet<String>,
    /// Pixi: env tables as currently written.
    pub tables: BTreeMap<String, Table>,
}

/// Snapshot of the machine: the active manager per section and what it has installed.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub sections: BTreeMap<Section, (ManagerId, Installed)>,
}

impl Inventory {
    pub fn collect(adapters: &[Box<dyn Adapter>], cx: &Ctx) -> Result<Inventory> {
        let mut inv = Inventory::default();
        for a in adapters {
            let installed = Installed {
                names: a.list_user_installed(cx)?,
                tables: a.tables(cx)?,
            };
            inv.sections
                .entry(a.section())
                .or_insert((a.id(), installed));
        }
        Ok(inv)
    }

    pub fn manager(&self, section: Section) -> Option<ManagerId> {
        self.sections.get(&section).map(|(m, _)| *m)
    }
}

/// Something installed here that no manifest knows about.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub manager: ManagerId,
    pub native: String,
    /// Pixi: the env table pixi wrote, copied into the manifest on claim.
    pub body: Option<Table>,
}

impl NewItem {
    pub fn section(&self) -> Section {
        self.manager.section()
    }
}

/// Native names for `manager` that are already accounted for and must not be asked about.
pub fn known_natives(
    shared: &Shared,
    local: &Local,
    manager: ManagerId,
) -> Result<BTreeSet<String>> {
    let section = manager.section();
    let mut known = local.baseline(manager).unwrap_or_default();
    known.extend(local.ignored(manager));
    // Shared entries count regardless of group, so installing something another
    // group owns does not nag. Logical names count too, so a `brew = false` entry
    // installed by hand anyway is still recognised.
    for e in shared
        .entries(section)?
        .into_iter()
        .chain(local.entries(section)?)
    {
        known.extend(e.native_for(manager));
        known.insert(e.name);
    }
    known.extend(local.exceptions(section));
    known.extend(
        local
            .applied(section)
            .into_values()
            .filter(|a| a.manager == manager)
            .map(|a| a.native),
    );
    Ok(known)
}

pub fn find_new(shared: &Shared, local: &Local, inv: &Inventory) -> Result<Vec<NewItem>> {
    let mut out = Vec::new();
    for (manager, installed) in inv.sections.values() {
        let known = known_natives(shared, local, *manager)?;
        for name in installed.names.difference(&known) {
            out.push(NewItem {
                manager: *manager,
                native: name.clone(),
                body: installed.tables.get(name).cloned(),
            });
        }
    }
    // pixi first, then system, so quick claims come before long apt lists.
    out.sort_by_key(|n| (n.section() != Section::Pixi, n.manager, n.native.clone()));
    Ok(out)
}
