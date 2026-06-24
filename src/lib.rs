//! pkgsync library crate.
//!
//! All the real logic lives here as testable modules; `main.rs` is a thin
//! binary that wires them into a TUI. Keeping logic in the lib means we can
//! unit-test it directly (see each module's `#[cfg(test)]` block).

pub mod app;
pub mod diff;
pub mod package;
