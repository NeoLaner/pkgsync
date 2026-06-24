//! The package data model and a parser for `pacman -Qe` output.
//!
//! `pacman -Qe` prints one explicitly-installed package per line, as
//! `<name> <version>`, e.g.:
//!
//! ```text
//! alacritty 0.17.0-1
//! hyprland 0.46.2-1
//! ```
//!
//! This module turns that text into `Vec<Package>`. It does NOT run pacman —
//! shelling out to commands and SSH is a later stage. Keeping parsing pure
//! (text in, data out) is what makes it trivially testable.

/// One installed package: its name and exact version string.
///
/// We keep `version` as a plain `String` rather than trying to parse it into
/// numbers — pacman versions like `1:2.4.0-3` (epoch:ver-rel) are fiddly, and
/// for diffing we only need to know whether two versions are *equal*, which a
/// string compare handles perfectly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// Something went wrong parsing a line of package output.
///
/// We carry the 1-based line number and the offending text so error messages
/// can point the user at exactly what was malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line_number: usize,
    pub content: String,
}

// Implementing `Display` gives a human-readable message. Implementing the
// `std::error::Error` marker trait lets this type flow through `Box<dyn Error>`
// and the `?` operator like any other error. (Crates like `thiserror` generate
// exactly this boilerplate for you — we hand-write it once so you see what's
// underneath.)
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}: expected `<name> <version>`, got {:?}",
            self.line_number, self.content
        )
    }
}

impl std::error::Error for ParseError {}

/// Parse the full output of `pacman -Qe` into a list of packages.
///
/// Blank lines are skipped. The first malformed line aborts the whole parse
/// with an `Err` — we'd rather refuse a corrupt list than silently sync a
/// partial one.
pub fn parse_package_list(raw: &str) -> Result<Vec<Package>, ParseError> {
    raw.lines()
        .enumerate() // pair each line with its 0-based index
        .filter(|(_, line)| !line.trim().is_empty()) // ignore blank lines
        .map(|(idx, line)| {
            parse_line(line).ok_or_else(|| ParseError {
                line_number: idx + 1, // report as 1-based, like an editor
                content: line.to_string(),
            })
        })
        // Collecting an iterator of `Result`s into `Result<Vec<_>, _>` is a
        // handy std trick: it yields `Ok(vec)` if every item is `Ok`, or
        // short-circuits and returns the first `Err`.
        .collect()
}

/// Serialize packages back into `pacman -Qe` format (`<name> <version>` lines,
/// newline-terminated). The inverse of `parse_package_list`, used for snapshots.
pub fn serialize_packages(packages: &[Package]) -> String {
    packages
        .iter()
        .map(|p| format!("{} {}\n", p.name, p.version))
        .collect()
}

/// Parse a single `<name> <version>` line. Returns `None` if it doesn't have
/// exactly those two whitespace-separated fields.
fn parse_line(line: &str) -> Option<Package> {
    let mut parts = line.split_whitespace();
    let name = parts.next()?;
    let version = parts.next()?;
    // Reject extra trailing fields — a well-formed line is exactly two tokens,
    // and anything more means we misunderstood the format.
    if parts.next().is_some() {
        return None;
    }
    Some(Package {
        name: name.to_string(),
        version: version.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A small helper so tests read cleanly.
    fn pkg(name: &str, version: &str) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn parses_multiple_packages() {
        let raw = "alacritty 0.17.0-1\nhyprland 0.46.2-1\n";
        let parsed = parse_package_list(raw).unwrap();
        assert_eq!(parsed, vec![pkg("alacritty", "0.17.0-1"), pkg("hyprland", "0.46.2-1")]);
    }

    #[test]
    fn skips_blank_lines_and_trailing_newline() {
        let raw = "\nalacritty 0.17.0-1\n\n   \nhyprland 0.46.2-1\n";
        let parsed = parse_package_list(raw).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn preserves_complex_version_strings() {
        // epoch:version-release form must survive intact.
        let parsed = parse_package_list("ffmpeg 2:7.1-3\n").unwrap();
        assert_eq!(parsed, vec![pkg("ffmpeg", "2:7.1-3")]);
    }

    #[test]
    fn tolerates_extra_internal_whitespace() {
        let parsed = parse_package_list("alacritty    0.17.0-1\n").unwrap();
        assert_eq!(parsed, vec![pkg("alacritty", "0.17.0-1")]);
    }

    #[test]
    fn errors_on_line_missing_version() {
        // "vim" has no version field -> error pointing at line 2.
        let err = parse_package_list("alacritty 0.17.0-1\nvim\n").unwrap_err();
        assert_eq!(err.line_number, 2);
        assert_eq!(err.content, "vim");
    }

    #[test]
    fn errors_on_line_with_extra_field() {
        let err = parse_package_list("name 1.0 garbage\n").unwrap_err();
        assert_eq!(err.line_number, 1);
    }

    #[test]
    fn empty_input_is_empty_list() {
        assert_eq!(parse_package_list("").unwrap(), vec![]);
    }

    #[test]
    fn serialize_round_trips_with_parse() {
        let pkgs = vec![pkg("alacritty", "0.17.0-1"), pkg("ffmpeg", "2:7.1-3")];
        let text = serialize_packages(&pkgs);
        assert_eq!(parse_package_list(&text).unwrap(), pkgs);
    }
}
