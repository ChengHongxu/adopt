//! Identifiers shared by every layer: manifest sections and package managers.

use std::fmt;

/// A namespace in packages.toml / local.toml. Every adapter belongs to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// OS package managers (apt, dnf, brew) share one logical namespace.
    System,
    /// pixi global environments.
    Pixi,
    /// npm global packages (`npm install -g`).
    Npm,
    /// Software installed by a command adopt runs (`curl ... | sh`), because no
    /// package manager has it.
    Script,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::System,
        Section::Pixi,
        Section::Npm,
        Section::Script,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Section::System => "system",
            Section::Pixi => "pixi",
            Section::Npm => "npm",
            Section::Script => "script",
        }
    }

    pub fn parse(s: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|x| x.key() == s)
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManagerId {
    Apt,
    Dnf,
    Brew,
    Pixi,
    Npm,
    Script,
}

impl ManagerId {
    pub const ALL: [ManagerId; 6] = [
        ManagerId::Apt,
        ManagerId::Dnf,
        ManagerId::Brew,
        ManagerId::Pixi,
        ManagerId::Npm,
        ManagerId::Script,
    ];
    /// Managers whose per-package name overrides may appear in a `[system.*]` entry.
    pub const SYSTEM: [ManagerId; 3] = [ManagerId::Apt, ManagerId::Dnf, ManagerId::Brew];

    pub fn key(self) -> &'static str {
        match self {
            ManagerId::Apt => "apt",
            ManagerId::Dnf => "dnf",
            ManagerId::Brew => "brew",
            ManagerId::Pixi => "pixi",
            ManagerId::Npm => "npm",
            ManagerId::Script => "script",
        }
    }

    pub fn parse(s: &str) -> Option<ManagerId> {
        ManagerId::ALL.into_iter().find(|x| x.key() == s)
    }

    pub fn section(self) -> Section {
        match self {
            ManagerId::Apt | ManagerId::Dnf | ManagerId::Brew => Section::System,
            ManagerId::Pixi => Section::Pixi,
            ManagerId::Npm => Section::Npm,
            ManagerId::Script => Section::Script,
        }
    }
}

impl fmt::Display for ManagerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}
