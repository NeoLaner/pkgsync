//! Sources we can offer without the user typing them: recently-used targets
//! (persisted across runs) and `Host` entries from `~/.ssh/config`.
//!
//! The parsing/serialization is pure and tested; the filesystem wrappers around
//! it are thin. Recents live in `$XDG_STATE_HOME/pkgsync/recent` (falling back
//! to `~/.local/state/pkgsync/recent`).

use crate::source::SourceSpec;
use std::path::PathBuf;

const MAX_RECENT: usize = 10;

// --- pure helpers (tested) --------------------------------------------------

/// Serialize a spec to one line, or `None` for sources we don't persist (Local).
fn spec_to_line(spec: &SourceSpec) -> Option<String> {
    match spec {
        SourceSpec::Ssh(host) => Some(format!("ssh {host}")),
        SourceSpec::File(path) => Some(format!("file {}", path.display())),
        SourceSpec::Local => None,
    }
}

/// Parse one line back into a spec. `None` for blank/unknown lines.
fn line_to_spec(line: &str) -> Option<SourceSpec> {
    let (kind, rest) = line.trim().split_once(' ')?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    match kind {
        "ssh" => Some(SourceSpec::Ssh(rest.to_string())),
        "file" => Some(SourceSpec::File(PathBuf::from(rest))),
        _ => None,
    }
}

fn parse_recent(text: &str) -> Vec<SourceSpec> {
    text.lines().filter_map(line_to_spec).collect()
}

fn serialize_recent(specs: &[SourceSpec]) -> String {
    specs
        .iter()
        .filter_map(spec_to_line)
        .map(|line| line + "\n")
        .collect()
}

/// Extract concrete host names from an ssh_config body. Skips wildcard/negated
/// patterns (`*`, `?`, `!`), which aren't real hosts you can connect to.
fn parse_ssh_config_hosts(text: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in text.lines() {
        let mut tokens = line.split_whitespace();
        let Some(keyword) = tokens.next() else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for pattern in tokens {
            if !pattern.contains(['*', '?', '!']) && !hosts.contains(&pattern.to_string()) {
                hosts.push(pattern.to_string());
            }
        }
    }
    hosts
}

// --- filesystem wrappers ----------------------------------------------------

fn state_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir).join("pkgsync"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local/state/pkgsync"))
}

fn recent_file() -> Option<PathBuf> {
    Some(state_dir()?.join("recent"))
}

/// Recently-used sources, most-recent first.
pub fn load_recent() -> Vec<SourceSpec> {
    let Some(path) = recent_file() else {
        return Vec::new();
    };
    std::fs::read_to_string(&path)
        .map(|text| parse_recent(&text))
        .unwrap_or_default()
}

/// Record a source as most-recently-used (move-to-front, deduped, capped).
/// Silently does nothing on IO errors — history is a convenience, not critical.
pub fn record(spec: &SourceSpec) {
    if spec_to_line(spec).is_none() {
        return; // not a persistable source (Local)
    }
    let mut recent = load_recent();
    recent.retain(|s| s != spec);
    recent.insert(0, spec.clone());
    recent.truncate(MAX_RECENT);

    if let Some(path) = recent_file()
        && let Some(dir) = path.parent()
    {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, serialize_recent(&recent));
    }
}

/// Host aliases declared in `~/.ssh/config`.
pub fn ssh_config_hosts() -> Vec<String> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let path = PathBuf::from(home).join(".ssh/config");
    std::fs::read_to_string(&path)
        .map(|text| parse_ssh_config_hosts(&text))
        .unwrap_or_default()
}

/// The sources to pre-populate the entry menu with: recents first, then any
/// ssh_config hosts not already among them.
pub fn menu_sources() -> Vec<SourceSpec> {
    let mut sources = load_recent();
    for host in ssh_config_hosts() {
        let spec = SourceSpec::Ssh(host);
        if !sources.contains(&spec) {
            sources.push(spec);
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_line_round_trips() {
        let ssh = SourceSpec::Ssh("office".into());
        assert_eq!(line_to_spec(&spec_to_line(&ssh).unwrap()), Some(ssh));

        let file = SourceSpec::File("/state/office.pkgs".into());
        assert_eq!(line_to_spec(&spec_to_line(&file).unwrap()), Some(file));

        // Local is not persisted.
        assert_eq!(spec_to_line(&SourceSpec::Local), None);
    }

    #[test]
    fn parse_recent_skips_garbage() {
        let text = "ssh office\nnonsense\nfile /x.pkgs\n\nssh \n";
        assert_eq!(
            parse_recent(text),
            vec![
                SourceSpec::Ssh("office".into()),
                SourceSpec::File("/x.pkgs".into()),
            ]
        );
    }

    #[test]
    fn serialize_then_parse_is_identity() {
        let specs = vec![
            SourceSpec::Ssh("a".into()),
            SourceSpec::File("/b.pkgs".into()),
        ];
        assert_eq!(parse_recent(&serialize_recent(&specs)), specs);
    }

    #[test]
    fn ssh_config_parsing_extracts_hosts_and_skips_wildcards() {
        let text = "\
Host office laptop
    HostName 10.0.0.5
host server
Host *
    ForwardAgent yes
Host pi-?
Host box
";
        assert_eq!(
            parse_ssh_config_hosts(text),
            vec!["office", "laptop", "server", "box"]
        );
    }
}
