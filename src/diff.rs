//! Comparing two package lists.
//!
//! Everything here is framed from the perspective of the LOCAL machine (the one
//! you're running pkgsync on) relative to a REMOTE machine (the other box):
//!
//! * a package on remote but not local  -> you might want to **install** it
//! * a package on local but not remote   -> you might want to **remove** it
//! * a package on both at different versions -> you might want to **upgrade**
//!
//! Like the parser, this is pure: lists in, diff out. No pacman, no UI.

use crate::package::Package;
use std::collections::HashMap;

/// What kind of difference a package represents, carrying the version context
/// the UI needs to display it. Each variant stores exactly the data that's
/// meaningful for that case — e.g. a `Missing` package has no local version, so
/// the variant simply doesn't have that field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// On remote, absent locally. The string is the remote's version.
    Missing { remote_version: String },
    /// Present locally, absent on remote. The string is the local version.
    Extra { local_version: String },
    /// Present on both, but the versions differ.
    VersionSkew {
        local_version: String,
        remote_version: String,
    },
}

/// The broad action bucket a diff falls into. Handy for grouping/filtering and
/// for coloring in the UI later (Install=green, Remove=red, Upgrade=yellow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Install,
    Remove,
    Upgrade,
}

impl DiffKind {
    /// Collapse a (data-carrying) `DiffKind` into its plain `Category`.
    /// `..` ignores the variant's fields — we only care which variant it is.
    pub fn category(&self) -> Category {
        match self {
            DiffKind::Missing { .. } => Category::Install,
            DiffKind::Extra { .. } => Category::Remove,
            DiffKind::VersionSkew { .. } => Category::Upgrade,
        }
    }
}

/// One package that differs between the two machines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub name: String,
    pub kind: DiffKind,
}

/// Compute the difference of `local` relative to `remote`.
///
/// The result is sorted by package name so the output is deterministic (good
/// for tests, and stable for the UI). Packages present on both machines at the
/// same version produce no entry — they're already in sync.
pub fn diff(local: &[Package], remote: &[Package]) -> Vec<DiffEntry> {
    // Index both sides by name for O(1) lookups. The maps borrow from the input
    // slices (`&str` keys/values), so no extra String allocation here — the
    // borrow checker guarantees the slices outlive the maps.
    let local_versions: HashMap<&str, &str> = local
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    let remote_versions: HashMap<&str, &str> = remote
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();

    let mut entries = Vec::new();

    // Walk remote: each package is either missing locally, version-skewed, or
    // already identical (no entry).
    for pkg in remote {
        match local_versions.get(pkg.name.as_str()) {
            None => entries.push(DiffEntry {
                name: pkg.name.clone(),
                kind: DiffKind::Missing {
                    remote_version: pkg.version.clone(),
                },
            }),
            Some(&local_version) if local_version != pkg.version => entries.push(DiffEntry {
                name: pkg.name.clone(),
                kind: DiffKind::VersionSkew {
                    local_version: local_version.to_string(),
                    remote_version: pkg.version.clone(),
                },
            }),
            Some(_) => {} // same version on both -> in sync, nothing to do
        }
    }

    // Walk local: anything not on remote is "extra" here.
    for pkg in local {
        if !remote_versions.contains_key(pkg.name.as_str()) {
            entries.push(DiffEntry {
                name: pkg.name.clone(),
                kind: DiffKind::Extra {
                    local_version: pkg.version.clone(),
                },
            });
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn identical_lists_produce_no_diff() {
        let a = vec![pkg("vim", "9.1"), pkg("git", "2.45")];
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn detects_missing_package() {
        let local = vec![pkg("vim", "9.1")];
        let remote = vec![pkg("vim", "9.1"), pkg("btop", "1.4.0")];
        let result = diff(&local, &remote);
        assert_eq!(
            result,
            vec![DiffEntry {
                name: "btop".to_string(),
                kind: DiffKind::Missing {
                    remote_version: "1.4.0".to_string()
                },
            }]
        );
    }

    #[test]
    fn detects_extra_package() {
        let local = vec![pkg("vim", "9.1"), pkg("discord", "0.0.49")];
        let remote = vec![pkg("vim", "9.1")];
        let result = diff(&local, &remote);
        assert_eq!(
            result,
            vec![DiffEntry {
                name: "discord".to_string(),
                kind: DiffKind::Extra {
                    local_version: "0.0.49".to_string()
                },
            }]
        );
    }

    #[test]
    fn detects_version_skew() {
        let local = vec![pkg("hyprland", "0.45")];
        let remote = vec![pkg("hyprland", "0.46")];
        let result = diff(&local, &remote);
        assert_eq!(
            result,
            vec![DiffEntry {
                name: "hyprland".to_string(),
                kind: DiffKind::VersionSkew {
                    local_version: "0.45".to_string(),
                    remote_version: "0.46".to_string(),
                },
            }]
        );
    }

    #[test]
    fn mixed_scenario_is_sorted_by_name() {
        let local = vec![pkg("vim", "9.1"), pkg("zsh", "5.9"), pkg("hyprland", "0.45")];
        let remote = vec![pkg("vim", "9.1"), pkg("btop", "1.4.0"), pkg("hyprland", "0.46")];
        let result = diff(&local, &remote);

        // btop (install), hyprland (upgrade), zsh (remove) — alphabetical.
        let names: Vec<&str> = result.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["btop", "hyprland", "zsh"]);

        let categories: Vec<Category> = result.iter().map(|e| e.kind.category()).collect();
        assert_eq!(
            categories,
            vec![Category::Install, Category::Upgrade, Category::Remove]
        );
    }

    #[test]
    fn category_maps_each_variant() {
        assert_eq!(
            DiffKind::Missing { remote_version: "1".into() }.category(),
            Category::Install
        );
        assert_eq!(
            DiffKind::Extra { local_version: "1".into() }.category(),
            Category::Remove
        );
        assert_eq!(
            DiffKind::VersionSkew { local_version: "1".into(), remote_version: "2".into() }.category(),
            Category::Upgrade
        );
    }
}
