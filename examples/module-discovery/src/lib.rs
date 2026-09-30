//! Offline module discoverer built on top of `syn`.
//!
//! Starting from a crate root file (`lib.rs` / `main.rs`), the discoverer
//! parses `mod` declarations with `syn` and resolves them following the
//! usual Rust file module rules: `foo.rs`, `foo/mod.rs`, nested directories
//! for file modules and inline modules, and literal `#[path = "..."]`
//! overrides. It never evaluates `#[cfg(...)]`; conditional modules are kept
//! in the tree with their condition recorded verbatim.
//!
//! The result is a [`ModuleTree`] with one [`Module`] node per discovered
//! module, declaration [`Edge`]s between them, byte ranges into the source
//! files, and [`Diagnostic`]s for ambiguity, missing files, recursive
//! inclusion and out-of-bounds (e.g. symlinked) files. Every diagnostic
//! carries the full chain of source files that led to the offending module.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::{Attribute, Item, ItemMod, Lit, Meta};

/// Index of a module inside [`ModuleTree::modules`].
pub type ModuleId = usize;

/// The discovered module graph of a crate.
#[derive(Debug)]
pub struct ModuleTree {
    /// Crate root file as passed to [`discover`].
    pub root: PathBuf,
    /// All discovered modules. Module `0` is the crate root.
    pub modules: Vec<Module>,
    /// Declaration edges between modules.
    pub edges: Vec<Edge>,
    /// Problems found while discovering, each with its full source chain.
    pub diagnostics: Vec<Diagnostic>,
}

impl ModuleTree {
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id]
    }

    /// Modules declared directly inside `id`.
    pub fn children(&self, id: ModuleId) -> impl Iterator<Item = ModuleId> + '_ {
        self.edges
            .iter()
            .filter(move |edge| edge.from == id && edge.kind == EdgeKind::Declares)
            .map(|edge| edge.to)
    }

    pub fn module_by_name(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|module| module.name == name)
    }
}

/// A single discovered module.
#[derive(Debug)]
pub struct Module {
    pub id: ModuleId,
    pub name: String,
    pub kind: ModuleKind,
    pub parent: Option<ModuleId>,
    /// Source file this module's body lives in. For inline modules this is
    /// the enclosing file; it is `None` only when the file could not be
    /// resolved (missing or ambiguous).
    pub file: Option<PathBuf>,
    /// Byte range of the whole file that defines this module.
    pub file_range: Option<Range<usize>>,
    /// Byte range of the `mod` item inside the parent's source file.
    /// `None` for the crate root.
    pub item_range: Option<Range<usize>>,
    /// Raw `#[cfg(...)]` conditions, recorded but never evaluated.
    pub cfg: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Root,
    File,
    Inline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: ModuleId,
    pub to: ModuleId,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// `from` contains the `mod` item that declares `to`.
    Declares,
}

/// A problem found during discovery. The `chain` field of every variant is
/// the full chain of source files (canonicalized) from the crate root down
/// to the file containing the offending `mod` item.
#[derive(Debug)]
pub enum Diagnostic {
    /// Both `foo.rs` and `foo/mod.rs` exist for the same module.
    Ambiguous {
        module: ModuleId,
        chain: Vec<PathBuf>,
        candidates: [PathBuf; 2],
    },
    /// No file was found for a `mod foo;` declaration.
    Missing {
        module: ModuleId,
        chain: Vec<PathBuf>,
        searched: Vec<PathBuf>,
    },
    /// A file includes itself, directly or through other files.
    Cycle {
        module: ModuleId,
        /// The include chain, ending with the repeated file.
        chain: Vec<PathBuf>,
    },
    /// The resolved file (after following symlinks) escapes the directory
    /// of the crate root.
    OutOfBounds {
        module: ModuleId,
        chain: Vec<PathBuf>,
        path: PathBuf,
    },
    /// A source file could not be read.
    Unreadable {
        chain: Vec<PathBuf>,
        file: PathBuf,
        error: String,
    },
    /// A source file could not be parsed.
    Parse {
        chain: Vec<PathBuf>,
        file: PathBuf,
        error: String,
    },
}

impl Diagnostic {
    pub fn chain(&self) -> &[PathBuf] {
        match self {
            Diagnostic::Ambiguous { chain, .. }
            | Diagnostic::Missing { chain, .. }
            | Diagnostic::Cycle { chain, .. }
            | Diagnostic::OutOfBounds { chain, .. }
            | Diagnostic::Unreadable { chain, .. }
            | Diagnostic::Parse { chain, .. } => chain,
        }
    }
}

/// Discover the module tree of the crate rooted at `root`.
pub fn discover(root: impl AsRef<Path>) -> ModuleTree {
    let root = root.as_ref();
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let boundary = canonical_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| canonical_root.clone());

    let mut tree = ModuleTree {
        root: root.to_path_buf(),
        modules: vec![Module {
            id: 0,
            name: "crate".to_owned(),
            kind: ModuleKind::Root,
            parent: None,
            file: Some(root.to_path_buf()),
            file_range: None,
            item_range: None,
            cfg: Vec::new(),
        }],
        edges: Vec::new(),
        diagnostics: Vec::new(),
    };

    {
        let mut discoverer = Discoverer {
            tree: &mut tree,
            boundary: boundary.clone(),
        };
        let mut chain = vec![canonical_root.clone()];
        discoverer.discover_file(0, &canonical_root, &boundary, &mut chain);
    }
    tree
}

struct Discoverer<'a> {
    tree: &'a mut ModuleTree,
    /// Canonical directory the crate's files must stay within.
    boundary: PathBuf,
}

impl Discoverer<'_> {
    /// Parse `file` and discover the modules declared in it. `dir_context`
    /// is the directory used to resolve child `mod` declarations.
    fn discover_file(
        &mut self,
        module: ModuleId,
        file: &Path,
        dir_context: &Path,
        chain: &mut Vec<PathBuf>,
    ) {
        let content = match fs::read_to_string(file) {
            Ok(content) => content,
            Err(error) => {
                self.tree.diagnostics.push(Diagnostic::Unreadable {
                    chain: chain.clone(),
                    file: file.to_path_buf(),
                    error: error.to_string(),
                });
                return;
            }
        };
        self.tree.modules[module].file_range = Some(0..content.len());

        let ast = match syn::parse_file(&content) {
            Ok(ast) => ast,
            Err(error) => {
                self.tree.diagnostics.push(Diagnostic::Parse {
                    chain: chain.clone(),
                    file: file.to_path_buf(),
                    error: error.to_string(),
                });
                return;
            }
        };
        self.visit_items(module, &ast.items, dir_context, chain);
    }

    fn visit_items(
        &mut self,
        parent: ModuleId,
        items: &[Item],
        dir_context: &Path,
        chain: &mut Vec<PathBuf>,
    ) {
        for item in items {
            if let Item::Mod(item_mod) = item {
                self.visit_mod(parent, item_mod, dir_context, chain);
            }
        }
    }

    fn visit_mod(
        &mut self,
        parent: ModuleId,
        item: &ItemMod,
        dir_context: &Path,
        chain: &mut Vec<PathBuf>,
    ) {
        let id = self.tree.modules.len();
        self.tree.modules.push(Module {
            id,
            name: item.ident.to_string(),
            kind: ModuleKind::File,
            parent: Some(parent),
            file: None,
            file_range: None,
            item_range: span_range(item),
            cfg: cfg_conditions(&item.attrs),
        });
        self.tree.edges.push(Edge {
            from: parent,
            to: id,
            kind: EdgeKind::Declares,
        });

        let path_attr = path_attribute(&item.attrs);
        match &item.content {
            Some((_, items)) => {
                // Inline module: children resolve in a subdirectory named
                // after the module, or next to the #[path] target.
                self.tree.modules[id].kind = ModuleKind::Inline;
                self.tree.modules[id].file = chain.last().cloned();
                let child_dir = match &path_attr {
                    Some(path) => stem_dir(&dir_context.join(path)),
                    None => dir_context.join(item.ident.to_string()),
                };
                self.visit_items(id, items, &child_dir, chain);
            }
            None => {
                let resolution =
                    resolve(dir_context, &item.ident.to_string(), path_attr.as_deref());
                match resolution {
                    Resolution::One(file) => {
                        self.tree.modules[id].file = Some(file.clone());
                        self.discover_child_file(id, &file, chain);
                    }
                    Resolution::Ambiguous(first, second) => {
                        self.tree.diagnostics.push(Diagnostic::Ambiguous {
                            module: id,
                            chain: chain.clone(),
                            candidates: [first, second],
                        });
                    }
                    Resolution::Missing(searched) => {
                        self.tree.diagnostics.push(Diagnostic::Missing {
                            module: id,
                            chain: chain.clone(),
                            searched,
                        });
                    }
                }
            }
        }
    }

    /// Recurse into the file backing a `mod foo;` declaration, guarding
    /// against out-of-bounds files and recursive inclusion.
    fn discover_child_file(&mut self, id: ModuleId, file: &Path, chain: &mut Vec<PathBuf>) {
        let canonical = match fs::canonicalize(file) {
            Ok(canonical) => canonical,
            Err(_) => file.to_path_buf(),
        };

        if !canonical.starts_with(&self.boundary) {
            self.tree.diagnostics.push(Diagnostic::OutOfBounds {
                module: id,
                chain: chain.clone(),
                path: file.to_path_buf(),
            });
            return;
        }

        if chain.contains(&canonical) {
            let mut full_chain = chain.clone();
            full_chain.push(canonical);
            self.tree.diagnostics.push(Diagnostic::Cycle {
                module: id,
                chain: full_chain,
            });
            return;
        }

        chain.push(canonical.clone());
        self.discover_file(id, &canonical, &stem_dir(&canonical), chain);
        chain.pop();
    }
}

enum Resolution {
    One(PathBuf),
    Ambiguous(PathBuf, PathBuf),
    Missing(Vec<PathBuf>),
}

/// Resolve a `mod foo;` declaration to its backing file.
fn resolve(dir_context: &Path, name: &str, path_attr: Option<&str>) -> Resolution {
    if let Some(path) = path_attr {
        let candidate = dir_context.join(path);
        return if candidate.is_file() {
            Resolution::One(candidate)
        } else {
            Resolution::Missing(vec![candidate])
        };
    }
    let plain = dir_context.join(format!("{name}.rs"));
    let nested = dir_context.join(name).join("mod.rs");
    match (plain.is_file(), nested.is_file()) {
        (true, true) => Resolution::Ambiguous(plain, nested),
        (true, false) => Resolution::One(plain),
        (false, true) => Resolution::One(nested),
        (false, false) => Resolution::Missing(vec![plain, nested]),
    }
}

/// Directory in which the children of a module living in `file` resolve:
/// the file's own directory for `mod.rs`, otherwise the file stem.
fn stem_dir(file: &Path) -> PathBuf {
    if file.file_name().is_some_and(|name| name == "mod.rs") {
        file.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        let mut stem = file.to_path_buf();
        stem.set_extension("");
        stem
    }
}

fn path_attribute(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        if let Meta::NameValue(name_value) = &attr.meta {
            if let syn::Expr::Lit(expr_lit) = &name_value.value {
                if let Lit::Str(lit) = &expr_lit.lit {
                    return Some(lit.value());
                }
            }
        }
    }
    None
}

fn cfg_conditions(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .filter_map(|attr| match &attr.meta {
            Meta::List(list) => Some(list.tokens.to_string()),
            _ => None,
        })
        .collect()
}

fn span_range(item: &ItemMod) -> Option<Range<usize>> {
    let range = item.span().byte_range();
    (range.end > range.start).then_some(range)
}
