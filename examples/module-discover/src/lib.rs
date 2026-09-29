//! Offline Rust module discoverer built on top of syn.
//!
//! Starting from a crate root file, the discoverer parses every mod
//! declaration and locates subsequent files according to the Rust module
//! rules:
//!
//! - "mod foo;" next to a file resolves to "foo.rs" or "foo/mod.rs";
//! - inline "mod foo { ... }" keeps its file children under a "foo/"
//!   directory;
//! - a literal "#[path = \"...\"]" attribute relocates the search.
//!
//! The result is a DiscoveredCrate holding the module tree, every source
//! file with byte ranges for its declarations, and directed reference edges.
//! Ambiguities, missing files, out-of-root symlinks and recursive inclusion
//! are reported as diagnostics that carry the complete declaration chain.
//! Conditional (cfg) modules are always retained as conditional nodes;
//! predicates are never guessed on or evaluated.

mod cfg;
mod discover;
mod items;
mod model;
mod resolve;
mod spanmap;

pub use discover::{Diagnostic, DiscoveredCrate};
pub use model::{
    CfgExpr, CfgPredicate, ChainHop, Edge, EdgeKind, FileId, HopResolution, Module, ModuleId,
    ModuleOrigin, SourceFile, SourceRef, SpanRange,
};
pub use resolve::CandidateResolution;

use std::io;
use std::path::Path;

/// Discover the module tree reachable from a crate root file.
pub fn discover(root: impl AsRef<Path>) -> io::Result<DiscoveredCrate> {
    discover::discover(root)
}
