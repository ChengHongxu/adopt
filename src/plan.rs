//! Pure comparison of "what is installed" against "what the manifests say".
//! No I/O here, so every rule can be unit-tested directly.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use toml_edit::Table;

use crate::adapters::{Adapter, Ctx};
use crate::model::{ManagerId, Section};
use crate::state::{Applied, Local, Shared, fingerprint};

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

// ---- reconciling manifests with the machine ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// packages.toml
    Shared,
    /// local.toml `[local.*]`
    Local,
}

/// A package this machine should have.
#[derive(Debug, Clone)]
pub struct Want {
    pub logical: String,
    pub native: String,
    /// Pixi: the env table to write.
    pub body: Option<Table>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub enum Action {
    /// Wanted, never applied here, not installed.
    Install(Want),
    /// Pixi: the shared env changed and the local copy is untouched since last sync.
    Update(Want),
    /// Gone from the manifests and adopt installed it.
    Remove {
        logical: String,
        native: String,
    },
    /// Bookkeeping only: record that this package is in place.
    MarkApplied {
        logical: String,
        applied: Applied,
    },
    /// Bookkeeping only: no longer wanted; leave the package itself alone.
    Forget {
        logical: String,
    },
    ClearFailed {
        logical: String,
    },
}

impl Action {
    /// Whether this changes the machine (as opposed to local.toml only).
    pub fn is_change(&self) -> bool {
        matches!(
            self,
            Action::Install(_) | Action::Update(_) | Action::Remove { .. }
        )
    }
}

/// Needs a human decision; `sync` only counts these.
#[derive(Debug, Clone)]
pub enum Pending {
    /// Applied before, gone now: the user uninstalled it. Never reinstalled silently.
    Missing { manager: ManagerId, want: Want },
    /// Install failed earlier with the same name; not retried automatically.
    Failed {
        manager: ManagerId,
        want: Want,
        error: String,
    },
    /// Pixi: the env here differs from the manifest and was edited locally.
    Drift {
        manager: ManagerId,
        want: Want,
        current: Table,
    },
}

#[derive(Debug, Default)]
pub struct Plan {
    pub actions: Vec<(ManagerId, Action)>,
    pub pending: Vec<Pending>,
}

impl Plan {
    pub fn changes(&self) -> usize {
        self.actions.iter().filter(|(_, a)| a.is_change()).count()
    }
}

/// What `manager` should have on this machine: shared entries for our groups minus
/// exceptions and `false` mappings, overlaid with local entries.
pub fn desired(
    shared: &Shared,
    local: &Local,
    section: Section,
    manager: ManagerId,
) -> Result<BTreeMap<String, Want>> {
    let groups = local.groups();
    let exceptions = local.exceptions(section);
    let mut out = BTreeMap::new();
    let shared_entries = shared
        .entries(section)?
        .into_iter()
        .filter(|e| e.applies_to(&groups) && !exceptions.contains(&e.name))
        .map(|e| (e, Origin::Shared));
    let local_entries = local
        .entries(section)?
        .into_iter()
        .map(|e| (e, Origin::Local));
    for (e, origin) in shared_entries.chain(local_entries) {
        if let Some(native) = e.native_for(manager) {
            out.insert(
                e.name.clone(),
                Want {
                    logical: e.name,
                    native,
                    body: e.body,
                    origin,
                },
            );
        }
    }
    Ok(out)
}

/// The sync rules. Pure: reads state, returns what to do.
///
/// - wanted, never applied, not installed → install
/// - applied before, not installed now → pending (the user removed it; ask, never reinstall)
/// - applied, no longer wanted → remove only if adopt installed it, else just forget it
pub fn reconcile(shared: &Shared, local: &Local, inv: &Inventory) -> Result<Plan> {
    let mut plan = Plan::default();
    for (section, (manager, installed)) in &inv.sections {
        let (section, manager) = (*section, *manager);
        let want = desired(shared, local, section, manager)?;
        let applied = local.applied(section);
        let failed = local.failed(section);
        let mut act = |a: Action| plan.actions.push((manager, a));

        for (logical, w) in &want {
            let prev = applied.get(logical).filter(|a| a.manager == manager);
            let present = installed.names.contains(&w.native)
                || (untracked(&w.native) && prev.is_some_and(|a| a.native == w.native));
            let current = installed.tables.get(&w.native);
            if present && failed.contains_key(logical) {
                act(Action::ClearFailed {
                    logical: logical.clone(),
                });
            }
            match (present, prev) {
                (true, prev) => {
                    let cur_fp = current.map(fingerprint);
                    let want_fp = w.body.as_ref().map(fingerprint);
                    let mark = |by_adopt: bool| Action::MarkApplied {
                        logical: logical.clone(),
                        applied: Applied {
                            manager,
                            native: w.native.clone(),
                            by_adopt,
                            fp: cur_fp.clone(),
                        },
                    };
                    if cur_fp == want_fp {
                        // In place and identical: just make sure the record is current.
                        let up_to_date =
                            prev.is_some_and(|a| a.native == w.native && a.fp == cur_fp);
                        if !up_to_date {
                            act(mark(prev.is_some_and(|a| a.by_adopt)));
                        }
                    } else if prev.is_some_and(|a| a.fp.is_some() && a.fp == cur_fp) {
                        // Untouched here since we last wrote it: take the new shared version.
                        act(Action::Update(w.clone()));
                    } else {
                        plan.pending.push(Pending::Drift {
                            manager,
                            want: w.clone(),
                            current: current.cloned().unwrap_or_default(),
                        });
                    }
                }
                (false, Some(_)) => plan.pending.push(Pending::Missing {
                    manager,
                    want: w.clone(),
                }),
                (false, None) => match failed.get(logical) {
                    Some(f) if f.native == w.native => plan.pending.push(Pending::Failed {
                        manager,
                        want: w.clone(),
                        error: f.error.clone(),
                    }),
                    // Never failed, or the name was fixed since: try (again).
                    _ => act(Action::Install(w.clone())),
                },
            }
        }

        for (logical, a) in &applied {
            if want.contains_key(logical) || a.manager != manager {
                continue;
            }
            if a.by_adopt && (installed.names.contains(&a.native) || untracked(&a.native)) {
                act(Action::Remove {
                    logical: logical.clone(),
                    native: a.native.clone(),
                });
            } else {
                act(Action::Forget {
                    logical: logical.clone(),
                });
            }
        }
        for (logical, f) in &failed {
            if f.manager == manager && !want.contains_key(logical) {
                act(Action::ClearFailed {
                    logical: logical.clone(),
                });
            }
        }
    }
    Ok(plan)
}

/// dnf groups (`@development-tools`) never show up in the user-installed list, so for
/// them "installed" can only mean "adopt applied it".
fn untracked(native: &str) -> bool {
    native.starts_with('@')
}

/// Everything waiting for the user: new packages, pending items, and changes `sync`
/// has not applied yet. This is the number `adopt status` shows.
pub fn pending_count(shared: &Shared, local: &Local, inv: &Inventory) -> Result<usize> {
    let plan = reconcile(shared, local, inv)?;
    Ok(find_new(shared, local, inv)?.len() + plan.pending.len() + plan.changes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Entry, Failed, TomlFile};

    fn shared(text: &str) -> Shared {
        let dir = tempfile::tempdir().unwrap();
        let mut file = TomlFile::load_or(&dir.path().join("p.toml"), "").unwrap();
        file.doc = text.parse().unwrap();
        Shared { file }
    }

    fn local(text: &str) -> Local {
        let dir = tempfile::tempdir().unwrap();
        let mut file = TomlFile::load_or(&dir.path().join("l.toml"), "").unwrap();
        file.doc = text.parse().unwrap();
        Local { file }
    }

    fn apt(installed: &[&str]) -> Inventory {
        let mut inv = Inventory::default();
        inv.sections.insert(
            Section::System,
            (
                ManagerId::Apt,
                Installed {
                    names: installed.iter().map(|s| s.to_string()).collect(),
                    tables: BTreeMap::new(),
                },
            ),
        );
        inv
    }

    fn applied(l: &mut Local, name: &str, by_adopt: bool) {
        l.set_applied(
            Section::System,
            name,
            &Applied {
                manager: ManagerId::Apt,
                native: name.into(),
                by_adopt,
                fp: None,
            },
        );
    }

    fn summary(p: &Plan) -> Vec<String> {
        let mut out: Vec<String> = p
            .actions
            .iter()
            .map(|(_, a)| match a {
                Action::Install(w) => format!("install {}", w.native),
                Action::Update(w) => format!("update {}", w.native),
                Action::Remove { native, .. } => format!("remove {native}"),
                Action::MarkApplied { logical, applied } => {
                    format!("mark {logical} by_adopt={}", applied.by_adopt)
                }
                Action::Forget { logical } => format!("forget {logical}"),
                Action::ClearFailed { logical } => format!("clear {logical}"),
            })
            .collect();
        out.extend(p.pending.iter().map(|x| match x {
            Pending::Missing { want, .. } => format!("missing {}", want.native),
            Pending::Failed { want, .. } => format!("failed {}", want.native),
            Pending::Drift { want, .. } => format!("drift {}", want.native),
        }));
        out
    }

    #[test]
    fn new_shared_entry_is_installed() {
        let s = shared("[system.htop]\n");
        let p = reconcile(&s, &local(""), &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["install htop"]);
    }

    #[test]
    fn already_installed_is_only_recorded_not_owned() {
        let s = shared("[system.htop]\n");
        let p = reconcile(&s, &local(""), &apt(&["htop"])).unwrap();
        assert_eq!(summary(&p), ["mark htop by_adopt=false"]);
    }

    #[test]
    fn uninstalled_by_user_is_pending_not_reinstalled() {
        let s = shared("[system.htop]\n");
        let mut l = local("");
        applied(&mut l, "htop", true);
        let p = reconcile(&s, &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["missing htop"]);
    }

    #[test]
    fn removed_from_manifest_uninstalls_only_what_adopt_installed() {
        let s = shared("");
        let mut l = local("");
        applied(&mut l, "htop", true);
        applied(&mut l, "tmux", false);
        let p = reconcile(&s, &l, &apt(&["htop", "tmux"])).unwrap();
        assert_eq!(summary(&p), ["remove htop", "forget tmux"]);
    }

    #[test]
    fn removed_and_already_gone_is_forgotten() {
        let mut l = local("");
        applied(&mut l, "htop", true);
        let p = reconcile(&shared(""), &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["forget htop"]);
    }

    #[test]
    fn groups_exceptions_and_skips() {
        let s = shared(
            "[system.a]\ngroups = [\"work\"]\n[system.b]\ngroups = [\"homelab\"]\n\
             [system.c]\napt = false\n[system.d]\napt = \"d-renamed\"\n[system.e]\n",
        );
        let l = local("groups = [\"work\"]\n[exceptions]\nsystem = [\"e\"]\n");
        let p = reconcile(&s, &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["install a", "install d-renamed"]);
    }

    #[test]
    fn local_entries_are_wanted_too() {
        let mut l = local("");
        l.upsert_entry(&Entry::system("libssl-dev"));
        let p = reconcile(&shared(""), &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["install libssl-dev"]);
    }

    #[test]
    fn failed_is_not_retried_unless_the_name_changed() {
        let mut l = local("");
        let f = |native: &str| Failed {
            manager: ManagerId::Apt,
            native: native.into(),
            error: "E".into(),
        };
        l.set_failed(Section::System, "fd", &f("fd"));
        let p = reconcile(&shared("[system.fd]\n"), &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["failed fd"]);
        let p = reconcile(&shared("[system.fd]\napt = \"fd-find\"\n"), &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["install fd-find"]);
        let p = reconcile(&shared(""), &l, &apt(&[])).unwrap();
        assert_eq!(summary(&p), ["clear fd"]);
    }

    fn pixi(envs: &[(&str, &str)]) -> Inventory {
        let mut inv = Inventory::default();
        let tables: BTreeMap<String, Table> = envs
            .iter()
            .map(|(n, t)| {
                (
                    n.to_string(),
                    t.parse::<toml_edit::DocumentMut>()
                        .unwrap()
                        .as_table()
                        .clone(),
                )
            })
            .collect();
        inv.sections.insert(
            Section::Pixi,
            (
                ManagerId::Pixi,
                Installed {
                    names: tables.keys().cloned().collect(),
                    tables,
                },
            ),
        );
        inv
    }

    const V1: &str = "channels = [\"conda-forge\"]\ndependencies = { rg = \"*\" }\n";
    const V2: &str = "channels = [\"conda-forge\"]\ndependencies = { rg = \">=14\" }\n";
    const MINE: &str = "channels = [\"mine\"]\ndependencies = { rg = \"*\" }\n";

    fn pixi_applied(l: &mut Local, body: &str) {
        let t = body
            .parse::<toml_edit::DocumentMut>()
            .unwrap()
            .as_table()
            .clone();
        l.set_applied(
            Section::Pixi,
            "rg",
            &Applied {
                manager: ManagerId::Pixi,
                native: "rg".into(),
                by_adopt: true,
                fp: Some(fingerprint(&t)),
            },
        );
    }

    #[test]
    fn pixi_shared_change_updates_untouched_env() {
        let s = shared(&format!("[pixi.rg]\n{V2}"));
        let mut l = local("");
        pixi_applied(&mut l, V1);
        let p = reconcile(&s, &l, &pixi(&[("rg", V1)])).unwrap();
        assert_eq!(summary(&p), ["update rg"]);
    }

    #[test]
    fn pixi_local_edit_is_drift() {
        let s = shared(&format!("[pixi.rg]\n{V2}"));
        let mut l = local("");
        pixi_applied(&mut l, V1);
        let p = reconcile(&s, &l, &pixi(&[("rg", MINE)])).unwrap();
        assert_eq!(summary(&p), ["drift rg"]);
        // Same content written differently is not drift.
        let s = shared(&format!("[pixi.rg]\n{V1}"));
        let p = reconcile(
            &s,
            &l,
            &pixi(&[("rg", "dependencies={rg='*'}\nchannels=['conda-forge']\n")]),
        )
        .unwrap();
        assert!(summary(&p).is_empty(), "{:?}", summary(&p));
    }
}
