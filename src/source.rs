//! Where package lists come from.
//!
//! A `Source` is anything that can produce a `Vec<Package>`: the local machine
//! (`pacman -Qe`), a committed state file, or another machine over SSH. Putting
//! them behind one trait means the rest of the app doesn't care *how* a list
//! was obtained — and lets us add the live-SSH-with-file-fallback behavior as a
//! tiny generic helper.

use crate::package::{parse_package_list, Package, ParseError};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Anything that can yield a list of packages.
pub trait Source {
    /// A human label for status messages (e.g. "ssh: office").
    fn name(&self) -> String;
    /// Produce the package list, or explain why it couldn't.
    fn fetch(&self) -> Result<Vec<Package>, SourceError>;
}

/// Everything that can go wrong getting a package list.
#[derive(Debug)]
pub enum SourceError {
    /// The process couldn't even be started (e.g. `ssh` not installed).
    Spawn { program: String, error: std::io::Error },
    /// The process ran but exited non-zero; we keep its stderr to show why.
    CommandFailed {
        program: String,
        code: Option<i32>,
        stderr: String,
    },
    /// A state file couldn't be read.
    ReadFile { path: String, error: std::io::Error },
    /// The bytes were read but didn't parse as `<name> <version>` lines.
    Parse(ParseError),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Spawn { program, error } => {
                write!(f, "could not run `{program}`: {error}")
            }
            SourceError::CommandFailed { program, code, stderr } => {
                let code = code.map_or("signal".to_string(), |c| c.to_string());
                write!(f, "`{program}` failed (exit {code}): {stderr}")
            }
            SourceError::ReadFile { path, error } => {
                write!(f, "could not read {path}: {error}")
            }
            SourceError::Parse(e) => write!(f, "parse error: {e}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// Run a command, capture stdout, and turn anything but a clean exit into a
/// `SourceError`. Shared by the local and SSH sources.
fn run_command(program: &str, args: &[&str]) -> Result<String, SourceError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| SourceError::Spawn {
            program: program.to_string(),
            error,
        })?;

    if !output.status.success() {
        return Err(SourceError::CommandFailed {
            program: program.to_string(),
            code: output.status.code(),
            // stderr is bytes; pacman/ssh emit UTF-8, but `from_utf8_lossy`
            // never panics on stray bytes.
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// This machine, via `pacman -Qe`.
pub struct LocalSource;

impl Source for LocalSource {
    fn name(&self) -> String {
        "local (pacman -Qe)".to_string()
    }
    fn fetch(&self) -> Result<Vec<Package>, SourceError> {
        let raw = run_command("pacman", &["-Qe"])?;
        parse_package_list(&raw).map_err(SourceError::Parse)
    }
}

/// A committed state file (e.g. `state/office.pkgs`), in `pacman -Qe` format.
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for FileSource {
    fn name(&self) -> String {
        format!("file: {}", self.path.display())
    }
    fn fetch(&self) -> Result<Vec<Package>, SourceError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|error| SourceError::ReadFile {
            path: self.path.display().to_string(),
            error,
        })?;
        parse_package_list(&raw).map_err(SourceError::Parse)
    }
}

/// Another machine over SSH: `ssh <host> pacman -Qe`. Relies on your existing
/// SSH config / keys (and reachability — Tailscale, LAN, whatever).
pub struct SshSource {
    host: String,
}

impl SshSource {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }
}

impl Source for SshSource {
    fn name(&self) -> String {
        format!("ssh: {}", self.host)
    }
    fn fetch(&self) -> Result<Vec<Package>, SourceError> {
        // Fail fast instead of hanging on an unreachable host or a password
        // prompt: disable interactive auth and cap the connect time.
        let raw = run_command(
            "ssh",
            &[
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                &self.host,
                "pacman",
                "-Qe",
            ],
        )?;
        parse_package_list(&raw).map_err(SourceError::Parse)
    }
}

/// Which source actually produced the data — so the UI can tell the user
/// whether it's live or a (possibly stale) fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Primary,
    Fallback,
}

/// Try `primary`; if it errors, fall back to `fallback`. This is the
/// "live SSH, else committed state file" behavior. We only surface the
/// fallback's error if it *also* fails.
pub fn fetch_with_fallback(
    primary: &dyn Source,
    fallback: &dyn Source,
) -> Result<(Vec<Package>, Origin), SourceError> {
    match primary.fetch() {
        Ok(packages) => Ok((packages, Origin::Primary)),
        Err(_) => Ok((fallback.fetch()?, Origin::Fallback)),
    }
}

/// A lightweight, cloneable description of *where* to fetch from.
///
/// Unlike the `Box<dyn Source>` trait objects, a `SourceSpec` is plain data
/// (`Clone + Send + 'static`), so it can be moved into a worker thread to do the
/// (possibly slow) fetch off the UI thread. It builds the real `Source` on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    Local,
    File(PathBuf),
    Ssh(String),
}

impl SourceSpec {
    /// Human label for the picker list.
    pub fn label(&self) -> String {
        match self {
            SourceSpec::Local => "local (pacman -Qe)".to_string(),
            SourceSpec::File(path) => format!("file: {}", path.display()),
            SourceSpec::Ssh(host) => format!("ssh: {host}"),
        }
    }

    fn build(&self) -> Box<dyn Source> {
        match self {
            SourceSpec::Local => Box::new(LocalSource),
            SourceSpec::File(path) => Box::new(FileSource::new(path.clone())),
            SourceSpec::Ssh(host) => Box::new(SshSource::new(host.clone())),
        }
    }

    /// Fetch the package list this spec points at.
    pub fn fetch(&self) -> Result<Vec<Package>, SourceError> {
        self.build().fetch()
    }
}

/// Turn CLI arguments into a list of comparison targets:
/// * an existing **directory** is scanned for `*.pkgs` files (each becomes a file source),
/// * an existing **file** becomes a file source,
/// * anything else is treated as an **SSH host**.
pub fn discover(args: &[String]) -> Vec<SourceSpec> {
    let mut specs = Vec::new();
    for arg in args {
        let path = Path::new(arg);
        if path.is_dir() {
            if let Ok(read_dir) = std::fs::read_dir(path) {
                let mut files: Vec<PathBuf> = read_dir
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "pkgs"))
                    .collect();
                files.sort(); // deterministic order in the picker
                specs.extend(files.into_iter().map(SourceSpec::File));
            }
        } else if path.is_file() {
            specs.push(SourceSpec::File(path.to_path_buf()));
        } else {
            specs.push(SourceSpec::Ssh(arg.clone()));
        }
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_source_reads_and_parses() {
        // Write a throwaway state file to the temp dir, then read it back.
        let path = std::env::temp_dir().join("pkgsync_test_source.pkgs");
        std::fs::write(&path, "alacritty 0.17.0-1\nhyprland 0.46.2-1\n").unwrap();

        let source = FileSource::new(&path);
        let packages = source.fetch().unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "alacritty");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn file_source_missing_file_is_readfile_error() {
        let source = FileSource::new("/definitely/not/here.pkgs");
        let err = source.fetch().unwrap_err();
        assert!(matches!(err, SourceError::ReadFile { .. }));
    }

    // A stand-in source so we can test the fallback logic deterministically,
    // with no real processes or files involved.
    struct Fake {
        result: Result<Vec<Package>, ()>,
    }
    impl Source for Fake {
        fn name(&self) -> String {
            "fake".into()
        }
        fn fetch(&self) -> Result<Vec<Package>, SourceError> {
            self.result.clone().map_err(|_| SourceError::Parse(ParseError {
                line_number: 0,
                content: "boom".into(),
            }))
        }
    }

    #[test]
    fn fallback_used_only_when_primary_fails() {
        let pkg = vec![Package { name: "vim".into(), version: "9".into() }];

        // primary ok -> use primary, no fallback.
        let (_, origin) = fetch_with_fallback(
            &Fake { result: Ok(pkg.clone()) },
            &Fake { result: Err(()) },
        )
        .unwrap();
        assert_eq!(origin, Origin::Primary);

        // primary fails -> fall back.
        let (got, origin) = fetch_with_fallback(
            &Fake { result: Err(()) },
            &Fake { result: Ok(pkg.clone()) },
        )
        .unwrap();
        assert_eq!(origin, Origin::Fallback);
        assert_eq!(got, pkg);

        // both fail -> error surfaces.
        assert!(fetch_with_fallback(&Fake { result: Err(()) }, &Fake { result: Err(()) }).is_err());
    }

    #[test]
    fn discover_classifies_args() {
        // A non-path arg is an SSH host.
        let specs = discover(&["office".to_string()]);
        assert_eq!(specs, vec![SourceSpec::Ssh("office".to_string())]);
    }

    #[test]
    fn discover_scans_directory_for_pkgs_files() {
        // Build a temp dir with two .pkgs files and one unrelated file.
        let dir = std::env::temp_dir().join("pkgsync_discover_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("office.pkgs"), "vim 9\n").unwrap();
        std::fs::write(dir.join("laptop.pkgs"), "git 2\n").unwrap();
        std::fs::write(dir.join("README.md"), "ignore me").unwrap();

        let specs = discover(&[dir.display().to_string()]);
        let files: Vec<_> = specs
            .iter()
            .filter_map(|s| match s {
                SourceSpec::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(files, vec!["laptop.pkgs", "office.pkgs"]); // sorted, .md excluded

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spec_labels_are_distinct() {
        assert!(SourceSpec::Local.label().contains("local"));
        assert!(SourceSpec::Ssh("h".into()).label().starts_with("ssh:"));
        assert!(SourceSpec::File("/x.pkgs".into()).label().starts_with("file:"));
    }
}
