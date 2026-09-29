//! Data model returned by the offline module discoverer.

use std::path::PathBuf;

/// Index of a module node in [`crate::DiscoveredCrate::modules`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ModuleId(pub usize);

/// Index of a source file in [`crate::DiscoveredCrate::files`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FileId(pub usize);

/// A discovered module node.
#[derive(Clone, Debug)]
pub struct Module {
    pub id: ModuleId,
    pub name: String,
    pub qualname: String,
    pub parent: Option<ModuleId>,
    pub children: Vec<ModuleId>,
    pub origin: ModuleOrigin,
    /// Conjunction of `#[cfg(...)]` attributes on this declaration and its
    /// ancestors. Never evaluated by the discoverer.
    pub cfg: Vec<CfgExpr>,
    /// Declaration chain from the crate root down to this node.
    pub chain: Vec<ChainHop>,
}

impl Module {
    pub fn is_inline(&self) -> bool {
        self.origin.is_inline()
    }
}

/// How a module node was resolved.
#[derive(Clone, Debug)]
pub enum ModuleOrigin {
    /// The crate root file.
    Root,
    /// An inline module; its source is the parent's source file.
    Inline { declared_in: FileId },
    /// A file module located at the given path.
    File {
        declared_in: FileId,
        file: FileId,
        path: PathBuf,
        /// `foo/mod.rs` style (`true`) versus `foo.rs` style (`false`).
        mod_rs: bool,
    },
    /// More than one candidate file existed (`foo.rs` and `foo/mod.rs`).
    Ambiguous {
        declared_in: FileId,
        candidates: Vec<PathBuf>,
    },
    /// No candidate file could be found for a `mod foo;` declaration.
    Missing {
        declared_in: FileId,
        searched: Vec<PathBuf>,
    },
    /// The resolved path escapes the crate root through a symlink.
    OutsideRoot {
        declared_in: FileId,
        path: PathBuf,
        resolved: PathBuf,
    },
    /// Following this declaration revisits a file already on the stack.
    Cycle {
        declared_in: FileId,
        path: PathBuf,
        /// Index into the ancestor chain of the revisited hop.
        back_to: usize,
    },
    /// The module file existed but could not be read.
    Unreadable {
        declared_in: FileId,
        path: PathBuf,
        error: String,
    },
    /// The module file existed but was not valid UTF-8.
    NotUtf8 { declared_in: FileId, path: PathBuf },
    /// The module file existed but could not be parsed as Rust source.
    ParseError {
        declared_in: FileId,
        file: FileId,
        path: PathBuf,
        error: String,
    },
    /// A `#[path]` attribute was present but not a string literal.
    UnsupportedPath { declared_in: FileId },
}

impl ModuleOrigin {
    pub fn is_inline(&self) -> bool {
        matches!(self, ModuleOrigin::Inline { .. })
    }

    /// The file in which this module's declaration appears, if known.
    pub fn declared_in(&self) -> Option<FileId> {
        match self {
            ModuleOrigin::Root => None,
            ModuleOrigin::Inline { declared_in }
            | ModuleOrigin::File { declared_in, .. }
            | ModuleOrigin::Ambiguous { declared_in, .. }
            | ModuleOrigin::Missing { declared_in, .. }
            | ModuleOrigin::OutsideRoot { declared_in, .. }
            | ModuleOrigin::Cycle { declared_in, .. }
            | ModuleOrigin::Unreadable { declared_in, .. }
            | ModuleOrigin::NotUtf8 { declared_in, .. }
            | ModuleOrigin::ParseError { declared_in, .. }
            | ModuleOrigin::UnsupportedPath { declared_in } => Some(*declared_in),
        }
    }

    /// The file holding the module's items, if one was successfully loaded.
    pub fn source_file(&self) -> Option<FileId> {
        match self {
            ModuleOrigin::File { file, .. } | ModuleOrigin::ParseError { file, .. } => Some(*file),
            _ => None,
        }
    }
}

/// One hop in the chain of declarations that reaches a module.
#[derive(Clone, Debug)]
pub struct ChainHop {
    pub name: String,
    /// Where the `mod` declaration was written.
    pub declared: SourceRef,
    /// The file chosen for the module body, if any.
    pub resolution: Option<HopResolution>,
}

#[derive(Clone, Debug)]
pub enum HopResolution {
    Inline,
    File(PathBuf),
    Ambiguous(Vec<PathBuf>),
    Missing,
    OutsideRoot { path: PathBuf, resolved: PathBuf },
    Cycle(PathBuf),
    Unreadable(PathBuf),
    NotUtf8(PathBuf),
    ParseError(PathBuf),
    UnsupportedPath,
}

/// A reference to a region of a source file.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SourceRef {
    pub file: FileId,
    pub range: SpanRange,
}

/// Byte range of a declaration inside a source file.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SpanRange {
    pub start: usize,
    pub end: usize,
}

/// A source file loaded during discovery.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    /// Byte length of the file contents.
    pub byte_len: usize,
    /// Range of every `mod` declaration in the file (attributes included).
    pub declarations: Vec<SourceRef>,
}

/// A directed edge from a containing module to one of its child modules.
#[derive(Clone, Debug)]
pub struct Edge {
    pub parent: ModuleId,
    pub child: ModuleId,
    pub kind: EdgeKind,
    /// The `mod` declaration responsible for the edge.
    pub site: SourceRef,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EdgeKind {
    /// `mod m { ... }` in the parent's source file.
    Inline,
    /// `mod m;` resolved to another source file.
    File,
    /// `mod m;` that could not be resolved to a readable file.
    Broken,
}

/// A cfg expression as found in a `#[cfg(...)]` attribute.
///
/// The discoverer never evaluates these; predicates it does not assign
/// meaning to are preserved so callers can make their own decisions.
#[derive(Clone, Debug)]
pub enum CfgExpr {
    Any(Vec<CfgExpr>),
    All(Vec<CfgExpr>),
    Not(Box<CfgExpr>),
    Bool(bool),
    /// A predicate the discoverer does not assign meaning to, e.g. `unix`,
    /// `target_os = "linux"`, `feature = "x"`.
    Predicate(CfgPredicate),
    /// Tokens the parser could not reduce to the forms above.
    Unknown(String),
}

#[derive(Clone, Debug)]
pub enum CfgPredicate {
    /// A single identifier, e.g. `unix`.
    Flag(String),
    /// `key = "value"`, e.g. `target_os = "linux"`.
    KeyValue { key: String, value: String },
}
