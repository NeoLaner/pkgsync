//! Turning ticked packages into actual package-manager commands.
//!
//! A `Plan` is the set of selected packages bucketed by what we'll do to them.
//! Building a plan is pure and tested; `execute` is the side-effecting part that
//! shells out — it deliberately inherits the terminal so you see yay's output
//! and its sudo prompt directly.
//!
//! We route everything through `yay`: it handles both official-repo and AUR
//! packages, and escalates to sudo itself, so we don't have to detect repos or
//! manage `sudo` ourselves.

use crate::diff::{DiffEntry, DiffKind};
use std::collections::HashSet;
use std::process::Command;

/// The selected packages, grouped by action.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub install: Vec<String>, // were Missing locally
    pub upgrade: Vec<String>, // were VersionSkew
    pub remove: Vec<String>,  // were Extra locally
}

/// One concrete command we intend to run, kept as data so it can be both shown
/// (in the confirm screen) and executed.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedCommand {
    pub description: String,
    pub program: String,
    pub args: Vec<String>,
}

impl PlannedCommand {
    /// How this command reads on a shell line, for the confirm screen.
    pub fn display(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

impl Plan {
    /// Build a plan from the full diff and the set of ticked package names.
    /// Entries that aren't selected are ignored.
    pub fn from_selection(entries: &[DiffEntry], selected: &HashSet<String>) -> Plan {
        let mut plan = Plan::default();
        for entry in entries {
            if !selected.contains(&entry.name) {
                continue;
            }
            match entry.kind {
                DiffKind::Missing { .. } => plan.install.push(entry.name.clone()),
                DiffKind::VersionSkew { .. } => plan.upgrade.push(entry.name.clone()),
                DiffKind::Extra { .. } => plan.remove.push(entry.name.clone()),
            }
        }
        plan
    }

    pub fn is_empty(&self) -> bool {
        self.install.is_empty() && self.upgrade.is_empty() && self.remove.is_empty()
    }

    pub fn total(&self) -> usize {
        self.install.len() + self.upgrade.len() + self.remove.len()
    }

    /// The commands this plan expands to, in execution order.
    pub fn commands(&self) -> Vec<PlannedCommand> {
        let mut cmds = Vec::new();

        // Install: `--needed` skips anything already present (harmless re-runs).
        if !self.install.is_empty() {
            cmds.push(PlannedCommand {
                description: format!("install {} package(s)", self.install.len()),
                program: "yay".to_string(),
                args: [
                    vec!["-S".to_string(), "--needed".to_string()],
                    self.install.clone(),
                ]
                .concat(),
            });
        }

        // Upgrade: NO `--needed` — these are already installed, and `--needed`
        // would skip them, defeating the upgrade.
        if !self.upgrade.is_empty() {
            cmds.push(PlannedCommand {
                description: format!("upgrade {} package(s)", self.upgrade.len()),
                program: "yay".to_string(),
                args: [vec!["-S".to_string()], self.upgrade.clone()].concat(),
            });
        }

        // Remove with `-Rns`: also removes now-orphaned deps and config files.
        if !self.remove.is_empty() {
            cmds.push(PlannedCommand {
                description: format!("remove {} package(s)", self.remove.len()),
                program: "yay".to_string(),
                args: [vec!["-Rns".to_string()], self.remove.clone()].concat(),
            });
        }

        cmds
    }

    /// Run the plan. Inherits the parent terminal (stdin/stdout/stderr), so the
    /// user sees yay's progress and types into its sudo prompt directly. Stops
    /// at the first command that fails. Caller must have left the TUI first.
    pub fn execute(&self) -> std::io::Result<()> {
        for cmd in self.commands() {
            println!("\n[pkgsync] {} → {}", cmd.description, cmd.display());
            // `.status()` (not `.output()`) inherits the terminal by default.
            let status = Command::new(&cmd.program).args(&cmd.args).status()?;
            if !status.success() {
                eprintln!("[pkgsync] `{}` failed — stopping.", cmd.display());
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<DiffEntry> {
        vec![
            DiffEntry {
                name: "btop".into(),
                kind: DiffKind::Missing { remote_version: "1.4".into() },
            },
            DiffEntry {
                name: "hyprland".into(),
                kind: DiffKind::VersionSkew {
                    local_version: "0.45".into(),
                    remote_version: "0.46".into(),
                },
            },
            DiffEntry {
                name: "discord".into(),
                kind: DiffKind::Extra { local_version: "0.1".into() },
            },
        ]
    }

    fn names(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn buckets_only_selected_entries() {
        let plan = Plan::from_selection(&entries(), &names(&["btop", "discord"]));
        assert_eq!(plan.install, vec!["btop"]);
        assert_eq!(plan.remove, vec!["discord"]);
        assert!(plan.upgrade.is_empty()); // hyprland wasn't selected
        assert_eq!(plan.total(), 2);
    }

    #[test]
    fn empty_selection_is_empty_plan() {
        let plan = Plan::from_selection(&entries(), &names(&[]));
        assert!(plan.is_empty());
        assert!(plan.commands().is_empty());
    }

    #[test]
    fn install_uses_needed_but_upgrade_does_not() {
        let plan = Plan::from_selection(&entries(), &names(&["btop", "hyprland"]));
        let cmds = plan.commands();

        let install = cmds.iter().find(|c| c.description.starts_with("install")).unwrap();
        assert_eq!(install.args, vec!["-S", "--needed", "btop"]);

        let upgrade = cmds.iter().find(|c| c.description.starts_with("upgrade")).unwrap();
        assert_eq!(upgrade.args, vec!["-S", "hyprland"]);
    }

    #[test]
    fn remove_uses_rns() {
        let plan = Plan::from_selection(&entries(), &names(&["discord"]));
        let cmds = plan.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].program, "yay");
        assert_eq!(cmds[0].args, vec!["-Rns", "discord"]);
    }
}
