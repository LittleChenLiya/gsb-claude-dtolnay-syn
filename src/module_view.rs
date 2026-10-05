//! Module export view: visibility and re-export analysis for inline module
//! trees.
//!
//! Given a [`File`] that contains only inline modules (`mod foo { ... }`), a
//! module export view records every named item together with the visibility
//! that governs it, resolves `pub(in ...)` restrictions along the real module
//! ancestor chain, and answers which items are visible from a given observer
//! module.
//!
//! # Scope
//!
//! - Only inline modules are analyzed. External module declarations
//!   (`mod foo;`) are recorded as unknown boundaries; their files are never
//!   read.
//! - Macros are never executed. `macro_rules!` definitions are recorded as
//!   named items, but macro invocations that would generate items are skipped.
//! - The report preserves the declaration span and the visibility source of
//!   every export.
//!
//! # Visibility semantics
//!
//! An item declared in module `M` is visible from an observer module `O` if
//! the item's visibility boundary is `O` or an ancestor of `O`:
//!
//! - `pub` — visible everywhere.
//! - `pub(crate)` — visible throughout the crate (boundary is the crate root).
//! - `pub(self)` / `pub(in self)` — visible in `M` and its descendants.
//! - `pub(super)` — visible in the parent of `M` and its descendants.
//! - `pub(in a::b)` — visible in the named ancestor module and its descendants.
//! - Private (no modifier) — visible in `M` and its descendants.
//!
//! `pub(super)` is **not** treated as crate-visible: it resolves to the real
//! parent module of the declaring item.
//!
//! # Re-exports
//!
//! `use` items are flattened and recorded as re-exports. The effective
//! visibility of a re-export is the intersection of the `use` visibility and
//! the target item's visibility. When the target's visibility cannot be
//! determined (external module, glob import, or unresolvable path), the
//! re-export is marked with an unknown boundary.
//!
//! # Example
//!
//! ```
//! use syn::module_view::ModuleView;
//! use syn::File;
//!
//! let src = r#"
//!     mod a {
//!         pub mod b {
//!             pub fn everywhere() {}
//!             pub(crate) fn crate_wide() {}
//!             pub(super) fn parent_only() {}
//!         }
//!     }
//!     mod c {
//!         pub fn call() {}
//!     }
//! "#;
//! let file: File = syn::parse_str(src).unwrap();
//! let view = ModuleView::from_file(&file);
//!
//! let visible_from_a: Vec<&str> = view
//!     .exports_visible_from(&["a".into()])
//!     .iter()
//!     .map(|e| e.name.as_str())
//!     .collect();
//! assert!(visible_from_a.contains(&"parent_only"));
//!
//! let visible_from_c: Vec<&str> = view
//!     .exports_visible_from(&["c".into()])
//!     .iter()
//!     .map(|e| e.name.as_str())
//!     .collect();
//! assert!(!visible_from_c.contains(&"parent_only"));
//! assert!(visible_from_c.contains(&"crate_wide"));
//! ```
//!
//! # Running the tests
//!
//! To run the module's unit and integration tests with caches and temporary
//! files directed to a specific drive, set `TEMP`, `TMP`, and
//! `CARGO_TARGET_DIR` before invoking cargo:
//!
//! ```sh
//! TEMP=D:/path/to/tmp TMP=D:/path/to/tmp \
//!     CARGO_TARGET_DIR=D:/path/to/target \
//!     cargo test --lib --features full,test module_view
//! ```

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use proc_macro2::Span;

use crate::file::File;
use crate::item::{Item, ItemUse, UseTree};
use crate::path::Path;
use crate::restriction::Visibility;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A path of an inline module within the crate, e.g. `["a", "b"]` for
/// `crate::a::b`. The crate root is the empty path.
pub type ModulePath = Vec<String>;

/// The kind of a named export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportKind {
    /// A `fn` item.
    Fn,
    /// A `struct` item.
    Struct,
    /// An `enum` item.
    Enum,
    /// A `union` item.
    Union,
    /// A `trait` item.
    Trait,
    /// A `trait` alias item.
    TraitAlias,
    /// A `type` alias item.
    Type,
    /// A `const` item.
    Const,
    /// A `static` item.
    Static,
    /// An inline module (`mod foo { ... }`).
    Mod,
    /// An external module declaration (`mod foo;`), whose contents are unknown.
    ExternMod,
    /// A `macro_rules!` definition.
    Macro,
    /// A re-export through a `use` item.
    Reexport,
}

/// The visibility source governing an export.
///
/// This preserves the form as written (`pub`, `pub(crate)`, `pub(super)`,
/// `pub(in ...)`, or private). The resolved boundary used for visibility
/// checks is stored separately on the [`Export`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VisibilitySource {
    /// `pub` — visible everywhere.
    Public,
    /// `pub(crate)` — visible throughout the crate.
    Crate,
    /// `pub(self)` — visible in the declaring module and its descendants.
    SelfVis,
    /// `pub(super)` — visible in the parent module and its descendants.
    Super,
    /// `pub(in path)` — the path as written, e.g. `["crate", "a"]`.
    In(Vec<String>),
    /// Private (no visibility modifier) — visible in the declaring module and
    /// descendants.
    Private,
}

impl fmt::Display for VisibilitySource {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            VisibilitySource::Public => f.write_str("pub"),
            VisibilitySource::Crate => f.write_str("pub(crate)"),
            VisibilitySource::SelfVis => f.write_str("pub(self)"),
            VisibilitySource::Super => f.write_str("pub(super)"),
            VisibilitySource::In(segs) => {
                f.write_str("pub(in ")?;
                for (i, seg) in segs.iter().enumerate() {
                    if i > 0 {
                        f.write_str("::")?;
                    }
                    f.write_str(seg)?;
                }
                f.write_str(")")
            }
            VisibilitySource::Private => f.write_str("private"),
        }
    }
}

/// A re-export through a `use` item.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Reexport {
    /// The target path as written in the `use` item, e.g.
    /// `["crate", "a", "b", "foo"]`.
    pub target: Vec<String>,
    /// Span of the target path.
    pub target_span: Span,
    /// True if the target's visibility could not be determined (external
    /// module, glob import, or unresolvable path).
    pub unknown_boundary: bool,
}

/// A named export item in a module.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Export {
    /// The name as it appears in the module (the declared name, or the alias
    /// for re-exports).
    pub name: String,
    /// Span of the declaration (or the exported name segment for re-exports).
    pub span: Span,
    /// The kind of item.
    pub kind: ExportKind,
    /// The module that declares this export.
    pub declared_in: ModulePath,
    /// The visibility as written on the declaring item (or the `use` item for
    /// re-exports).
    pub visibility: VisibilitySource,
    /// The effective visibility boundary: the module whose descendants can
    /// see this export. `None` means visible everywhere (`pub`).
    pub boundary: Option<ModulePath>,
    /// For re-exports: the target path and resolution status.
    pub reexport: Option<Reexport>,
}

impl Export {
    /// Returns true if this export is visible from the given observer module.
    pub fn is_visible_from(&self, observer: &[String]) -> bool {
        match &self.boundary {
            None => true,
            Some(b) => is_prefix(b, observer),
        }
    }
}

/// Information about a module in the tree.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModuleInfo {
    /// The module path (empty for the crate root).
    pub path: ModulePath,
    /// The module name (empty for the crate root).
    pub name: String,
    /// Span of the `mod` declaration (call site for the root).
    pub span: Span,
    /// The visibility of the module itself.
    pub visibility: VisibilitySource,
    /// True for external module declarations (`mod foo;`), whose contents are
    /// unknown.
    pub is_external: bool,
    /// Direct named exports of this module (including re-exports).
    pub exports: Vec<Export>,
    /// Paths of direct child modules.
    pub children: Vec<ModulePath>,
}

/// A diagnostic produced while building the view.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Diagnostic {
    /// Span of the offending visibility path or use item.
    pub span: Span,
    /// The diagnostic message.
    pub message: String,
}

/// A module export view for an inline module tree.
#[derive(Debug, Clone)]
pub struct ModuleView {
    modules: BTreeMap<ModulePath, ModuleInfo>,
    diagnostics: Vec<Diagnostic>,
}

// ---------------------------------------------------------------------------
// Module path helpers
// ---------------------------------------------------------------------------

fn append_path(path: &ModulePath, seg: &str) -> ModulePath {
    let mut p = path.clone();
    p.push(seg.to_string());
    p
}

fn walk_up(path: &ModulePath, count: usize) -> ModulePath {
    let mut p = path.clone();
    for _ in 0..count {
        p.pop();
    }
    p
}

/// Returns true if `prefix` is a prefix of `path` (i.e. `path` is `prefix` or a
/// descendant of `prefix`).
fn is_prefix(prefix: &[String], path: &[String]) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == prefix[..]
}

/// Returns the span of the first segment of a path, or call site if empty.
fn path_span(path: &Path) -> Span {
    path.segments
        .first()
        .map(|seg| seg.ident.span())
        .unwrap_or_else(Span::call_site)
}

// ---------------------------------------------------------------------------
// Use tree flattening
// ---------------------------------------------------------------------------

struct UseEntry {
    path: Vec<String>,
    name: String,
    alias: Option<String>,
    is_glob: bool,
    leading_colon: bool,
    span: Span,
}

impl UseEntry {
    fn exported_name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.name.clone())
    }

    fn full_path(&self) -> Vec<String> {
        let mut p = self.path.clone();
        if !self.is_glob {
            p.push(self.name.clone());
        }
        p
    }
}

fn flatten_use_tree(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    leading_colon: bool,
    entries: &mut Vec<UseEntry>,
) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            flatten_use_tree(&p.tree, prefix, leading_colon, entries);
            prefix.pop();
        }
        UseTree::Name(n) => {
            entries.push(UseEntry {
                path: prefix.clone(),
                name: n.ident.to_string(),
                alias: None,
                is_glob: false,
                leading_colon,
                span: n.ident.span(),
            });
        }
        UseTree::Rename(r) => {
            entries.push(UseEntry {
                path: prefix.clone(),
                name: r.ident.to_string(),
                alias: Some(r.rename.to_string()),
                is_glob: false,
                leading_colon,
                span: r.rename.span(),
            });
        }
        UseTree::Glob(g) => {
            entries.push(UseEntry {
                path: prefix.clone(),
                name: "*".to_string(),
                alias: None,
                is_glob: true,
                leading_colon,
                span: g.star_token.span,
            });
        }
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(item, prefix, leading_colon, entries);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Visibility resolution
// ---------------------------------------------------------------------------

/// Resolves a visibility level relative to `declared_in`.
///
/// Returns the visibility source (as written) and the resolved boundary module
/// (`None` for `pub`). Emits a diagnostic for illegal ancestor paths.
fn resolve_visibility(
    vis: &Visibility,
    declared_in: &ModulePath,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (VisibilitySource, Option<ModulePath>) {
    match vis {
        Visibility::Public(_) => (VisibilitySource::Public, None),
        Visibility::Inherited => (VisibilitySource::Private, Some(declared_in.clone())),
        Visibility::Restricted(r) => {
            let segs: Vec<String> = r
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let has_in = r.in_token.is_some();

            let (anchor, remaining) =
                match restricted_anchor(&segs, declared_in, path_span(&r.path), diagnostics) {
                    Some(a) => a,
                    None => return (VisibilitySource::In(segs), Some(declared_in.clone())),
                };

            let mut target = anchor;
            for seg in &remaining {
                let child = append_path(&target, seg);
                match modules.get(&child) {
                    Some(info) if !info.is_external => target = child,
                    Some(_) => {
                        diagnostics.push(Diagnostic {
                            span: path_span(&r.path),
                            message: format!(
                                "cannot restrict visibility into external module `{}`",
                                seg
                            ),
                        });
                        return (VisibilitySource::In(segs), Some(declared_in.clone()));
                    }
                    None => {
                        diagnostics.push(Diagnostic {
                            span: path_span(&r.path),
                            message: format!("module `{}` not found", seg),
                        });
                        return (VisibilitySource::In(segs), Some(declared_in.clone()));
                    }
                }
            }

            if !is_prefix(&target, declared_in) {
                diagnostics.push(Diagnostic {
                    span: path_span(&r.path),
                    message: "visibilities can only be restricted to ancestor modules".into(),
                });
                return (VisibilitySource::In(segs), Some(declared_in.clone()));
            }

            let source = if has_in {
                VisibilitySource::In(segs)
            } else {
                match segs[0].as_str() {
                    "crate" => VisibilitySource::Crate,
                    "self" => VisibilitySource::SelfVis,
                    "super" => VisibilitySource::Super,
                    _ => unreachable!(),
                }
            };
            (source, Some(target))
        }
    }
}

/// Determines the anchor module and remaining segments for a restricted
/// visibility path.
fn restricted_anchor(
    segs: &[String],
    declared_in: &ModulePath,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(ModulePath, Vec<String>)> {
    if segs.is_empty() {
        return Some((declared_in.clone(), Vec::new()));
    }
    match segs[0].as_str() {
        "crate" => Some((ModulePath::new(), segs[1..].to_vec())),
        "self" => Some((declared_in.clone(), segs[1..].to_vec())),
        "super" => {
            let mut count = 0;
            for s in segs {
                if s == "super" {
                    count += 1;
                } else {
                    break;
                }
            }
            if count > declared_in.len() {
                diagnostics.push(Diagnostic {
                    span,
                    message: "too many leading `super` keywords within `crate`".into(),
                });
                return None;
            }
            let anchor = walk_up(declared_in, count);
            Some((anchor, segs[count..].to_vec()))
        }
        _ => {
            diagnostics.push(Diagnostic {
                span,
                message: "relative paths are not supported in visibilities in 2018 edition or later"
                    .into(),
            });
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Re-export resolution
// ---------------------------------------------------------------------------

/// Resolves a use entry to its target export.
///
/// Returns the target export (if found) and whether the boundary is unknown.
fn resolve_use_target(
    entry: &UseEntry,
    declared_in: &ModulePath,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Option<Export>, bool) {
    let mut segs = entry.full_path();
    // A leading `::` means the path is rooted at the crate root (like `crate::`).
    if entry.leading_colon && !segs.is_empty() && segs[0] != "crate" {
        segs.insert(0, "crate".to_string());
    }
    let (anchor, start) = match use_anchor(&segs, declared_in, entry.span, diagnostics) {
        Some(a) => a,
        None => return (None, true),
    };

    let module_end = segs.len().saturating_sub(1);
    let mut anchor = anchor;
    for seg in &segs[start..module_end] {
        let child = append_path(&anchor, seg);
        match modules.get(&child) {
            Some(info) if !info.is_external => anchor = child,
            Some(_) | None => return (None, true),
        }
    }

    if entry.is_glob {
        return (None, true);
    }

    let last = match segs.last() {
        Some(s) => s,
        None => return (None, true),
    };

    match modules.get(&anchor) {
        Some(info) => {
            let export = info.exports.iter().find(|e| &e.name == last);
            (export.cloned(), false)
        }
        None => (None, true),
    }
}

/// Determines the anchor module for a use path.
fn use_anchor(
    segs: &[String],
    declared_in: &ModulePath,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(ModulePath, usize)> {
    if segs.is_empty() {
        return Some((declared_in.clone(), 0));
    }
    match segs[0].as_str() {
        "crate" => Some((ModulePath::new(), 1)),
        "self" => Some((declared_in.clone(), 1)),
        "super" => {
            let mut count = 0;
            for s in segs {
                if s == "super" {
                    count += 1;
                } else {
                    break;
                }
            }
            if count > declared_in.len() {
                diagnostics.push(Diagnostic {
                    span,
                    message: "too many leading `super` keywords within `crate`".into(),
                });
                return None;
            }
            Some((walk_up(declared_in, count), count))
        }
        _ => Some((declared_in.clone(), 0)),
    }
}

/// Resolves the effective boundary of a re-export by following re-export
/// chains.
fn resolve_effective_boundary(
    export: &Export,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
) -> Option<ModulePath> {
    let mut result = export.boundary.clone();
    let mut current = export;
    let mut visited = BTreeSet::new();
    while let Some(reexport) = &current.reexport {
        if reexport.unknown_boundary {
            return result;
        }
        let (target_module, target_name) =
            match resolve_path_to_module(&reexport.target, &current.declared_in, modules) {
                Some(x) => x,
                None => return result,
            };
        let next = match modules.get(&target_module) {
            Some(info) => info.exports.iter().find(|e| e.name == target_name),
            None => return result,
        };
        let next = match next {
            Some(e) => e,
            None => return result,
        };
        let key = (target_module.clone(), target_name.to_string());
        if !visited.insert(key) {
            return result;
        }
        result = meet_boundary(&result, &next.boundary);
        current = next;
    }
    result
}

/// Resolves a use path to a (module, name) pair.
fn resolve_path_to_module(
    segs: &[String],
    declared_in: &ModulePath,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
) -> Option<(ModulePath, String)> {
    if segs.is_empty() {
        return None;
    }
    let (mut anchor, start) = match segs[0].as_str() {
        "crate" => (ModulePath::new(), 1),
        "self" => (declared_in.clone(), 1),
        "super" => {
            let mut count = 0;
            for s in segs {
                if s == "super" {
                    count += 1;
                } else {
                    break;
                }
            }
            if count > declared_in.len() {
                return None;
            }
            (walk_up(declared_in, count), count)
        }
        _ => (declared_in.clone(), 0),
    };
    let module_end = segs.len().saturating_sub(1);
    for seg in &segs[start..module_end] {
        let child = append_path(&anchor, seg);
        match modules.get(&child) {
            Some(info) if !info.is_external => anchor = child,
            _ => return None,
        }
    }
    let name = segs.last()?.clone();
    Some((anchor, name))
}

/// Returns the meet (most restrictive) of two visibility boundaries.
fn meet_boundary(a: &Option<ModulePath>, b: &Option<ModulePath>) -> Option<ModulePath> {
    match (a, b) {
        (None, _) => b.clone(),
        (_, None) => a.clone(),
        (Some(x), Some(y)) => {
            if is_prefix(x, y) {
                Some(y.clone())
            } else if is_prefix(y, x) {
                Some(x.clone())
            } else {
                // Incomparable boundaries — conservative choice.
                Some(x.clone())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tree building
// ---------------------------------------------------------------------------

fn build_tree(file: &File) -> (BTreeMap<ModulePath, ModuleInfo>, Vec<Diagnostic>) {
    let mut modules = BTreeMap::new();
    let mut diagnostics = Vec::new();
    // Pass 1: build the module tree, storing raw items per module.
    let mut raw_items: BTreeMap<ModulePath, Vec<Item>> = BTreeMap::new();
    build_module_tree(
        &ModulePath::new(),
        String::new(),
        Span::call_site(),
        VisibilitySource::Public,
        false,
        &file.items,
        &mut modules,
        &mut raw_items,
        &mut diagnostics,
    );
    // Pass 2: create exports (visibility resolution can now see all modules).
    for (path, items) in &raw_items {
        let mut exports = Vec::new();
        let mut children = Vec::new();
        for item in items {
            visit_item(
                item,
                path,
                &modules,
                &mut exports,
                &mut children,
                &mut diagnostics,
            );
        }
        if let Some(info) = modules.get_mut(path) {
            info.exports = exports;
            info.children = children;
        }
    }
    (modules, diagnostics)
}

/// Pass 1: builds the module tree with placeholder modules (empty exports).
fn build_module_tree(
    path: &ModulePath,
    name: String,
    span: Span,
    visibility: VisibilitySource,
    is_external: bool,
    items: &[Item],
    modules: &mut BTreeMap<ModulePath, ModuleInfo>,
    raw_items: &mut BTreeMap<ModulePath, Vec<Item>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Insert a placeholder so that visibility resolution can reference this
    // module and its ancestors even before its exports are created.
    modules.insert(
        path.clone(),
        ModuleInfo {
            path: path.clone(),
            name,
            span,
            visibility,
            is_external,
            exports: Vec::new(),
            children: Vec::new(),
        },
    );
    raw_items.insert(path.clone(), items.to_vec());
    for item in items {
        if let Item::Mod(m) = item {
            let mod_path = append_path(path, &m.ident.to_string());
            let (is_external, child_items) = match &m.content {
                Some((_, items)) => (false, items.as_slice()),
                None => (true, &[][..]),
            };
            // The module's own visibility is resolved relative to its parent.
            let (vis_source, _) = resolve_visibility(&m.vis, path, modules, diagnostics);
            build_module_tree(
                &mod_path,
                m.ident.to_string(),
                m.ident.span(),
                vis_source,
                is_external,
                child_items,
                modules,
                raw_items,
                diagnostics,
            );
        }
    }
}

fn visit_item(
    item: &Item,
    current: &ModulePath,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
    exports: &mut Vec<Export>,
    children: &mut Vec<ModulePath>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match item {
        Item::Mod(m) => {
            let mod_path = append_path(current, &m.ident.to_string());
            children.push(mod_path.clone());
            // Module visibility was already resolved in pass 1.
        }
        Item::Use(u) => {
            let mut entries = Vec::new();
            flatten_use_tree(
                &u.tree,
                &mut Vec::new(),
                u.leading_colon.is_some(),
                &mut entries,
            );
            for entry in entries {
                let export = resolve_reexport(&entry, u, current, modules, diagnostics);
                exports.push(export);
            }
        }
        Item::Fn(f) => {
            let (vis_source, boundary) =
                resolve_visibility(&f.vis, current, modules, diagnostics);
            exports.push(Export {
                name: f.sig.ident.to_string(),
                span: f.sig.ident.span(),
                kind: ExportKind::Fn,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Struct(s) => {
            let (vis_source, boundary) =
                resolve_visibility(&s.vis, current, modules, diagnostics);
            exports.push(Export {
                name: s.ident.to_string(),
                span: s.ident.span(),
                kind: ExportKind::Struct,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Enum(e) => {
            let (vis_source, boundary) =
                resolve_visibility(&e.vis, current, modules, diagnostics);
            exports.push(Export {
                name: e.ident.to_string(),
                span: e.ident.span(),
                kind: ExportKind::Enum,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Union(u) => {
            let (vis_source, boundary) =
                resolve_visibility(&u.vis, current, modules, diagnostics);
            exports.push(Export {
                name: u.ident.to_string(),
                span: u.ident.span(),
                kind: ExportKind::Union,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Trait(t) => {
            let (vis_source, boundary) =
                resolve_visibility(&t.vis, current, modules, diagnostics);
            exports.push(Export {
                name: t.ident.to_string(),
                span: t.ident.span(),
                kind: ExportKind::Trait,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::TraitAlias(t) => {
            let (vis_source, boundary) =
                resolve_visibility(&t.vis, current, modules, diagnostics);
            exports.push(Export {
                name: t.ident.to_string(),
                span: t.ident.span(),
                kind: ExportKind::TraitAlias,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Type(t) => {
            let (vis_source, boundary) =
                resolve_visibility(&t.vis, current, modules, diagnostics);
            exports.push(Export {
                name: t.ident.to_string(),
                span: t.ident.span(),
                kind: ExportKind::Type,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Const(c) => {
            let (vis_source, boundary) =
                resolve_visibility(&c.vis, current, modules, diagnostics);
            exports.push(Export {
                name: c.ident.to_string(),
                span: c.ident.span(),
                kind: ExportKind::Const,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Static(s) => {
            let (vis_source, boundary) =
                resolve_visibility(&s.vis, current, modules, diagnostics);
            exports.push(Export {
                name: s.ident.to_string(),
                span: s.ident.span(),
                kind: ExportKind::Static,
                declared_in: current.clone(),
                visibility: vis_source,
                boundary,
                reexport: None,
            });
        }
        Item::Macro(m) => {
            if let Some(ident) = &m.ident {
                let (vis_source, boundary) =
                    resolve_visibility(&Visibility::Inherited, current, modules, diagnostics);
                exports.push(Export {
                    name: ident.to_string(),
                    span: ident.span(),
                    kind: ExportKind::Macro,
                    declared_in: current.clone(),
                    visibility: vis_source,
                    boundary,
                    reexport: None,
                });
            }
            // Macro invocations are skipped: macros are never executed.
        }
        // Impls, foreign modules, and extern crates have no named export in the
        // module sense.
        _ => {}
    }
}

fn resolve_reexport(
    entry: &UseEntry,
    use_item: &ItemUse,
    declared_in: &ModulePath,
    modules: &BTreeMap<ModulePath, ModuleInfo>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Export {
    let (use_source, use_boundary) =
        resolve_visibility(&use_item.vis, declared_in, modules, diagnostics);
    let (target, unknown) = resolve_use_target(entry, declared_in, modules, diagnostics);

    let (boundary, unknown_boundary) = if entry.is_glob || unknown {
        (use_boundary, true)
    } else if let Some(target) = &target {
        let target_boundary = resolve_effective_boundary(target, modules);
        let legal = match (&use_boundary, &target_boundary) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(u), Some(t)) => is_prefix(t, u),
        };
        if legal {
            (use_boundary, false)
        } else {
            diagnostics.push(Diagnostic {
                span: use_item.use_token.span,
                message: format!("re-export of `{}` widens visibility", entry.exported_name()),
            });
            (target_boundary, false)
        }
    } else {
        (use_boundary, true)
    };

    let kind = target
        .as_ref()
        .map(|t| t.kind)
        .unwrap_or(ExportKind::Reexport);
    Export {
        name: entry.exported_name(),
        span: entry.span,
        kind,
        declared_in: declared_in.clone(),
        visibility: use_source,
        boundary,
        reexport: Some(Reexport {
            target: entry.full_path(),
            target_span: entry.span,
            unknown_boundary,
        }),
    }
}

// ---------------------------------------------------------------------------
// Fixpoint resolution of re-export chains
// ---------------------------------------------------------------------------

/// Re-resolves re-export boundaries until stable.
///
/// Re-export targets may live in modules that were built later in the tree,
/// so a single pass is not enough. This loop follows chains to a fixpoint.
fn resolve_reexport_chains(
    modules: &mut BTreeMap<ModulePath, ModuleInfo>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let max_iterations = modules.len().saturating_add(1);
    for _ in 0..max_iterations {
        let mut changes = Vec::new();
        for (path, info) in modules.iter() {
            for (i, export) in info.exports.iter().enumerate() {
                if export.reexport.is_none() {
                    continue;
                }
                let effective = resolve_effective_boundary(export, modules);
                if effective != export.boundary {
                    changes.push((path.clone(), i, effective));
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        for (path, i, effective) in changes {
            if let Some(info) = modules.get_mut(&path) {
                if let Some(export) = info.exports.get_mut(i) {
                    export.boundary = effective;
                }
            }
        }
    }
    let _ = diagnostics;
}

// ---------------------------------------------------------------------------
// ModuleView
// ---------------------------------------------------------------------------

impl ModuleView {
    /// Builds a module export view from an inline module tree.
    pub fn from_file(file: &File) -> Self {
        let (mut modules, mut diagnostics) = build_tree(file);
        resolve_reexport_chains(&mut modules, &mut diagnostics);
        ModuleView {
            modules,
            diagnostics,
        }
    }

    /// Returns an iterator over all modules in the tree.
    pub fn modules(&self) -> impl Iterator<Item = &ModuleInfo> {
        self.modules.values()
    }

    /// Returns the module at the given path, if it exists.
    pub fn module(&self, path: &[String]) -> Option<&ModuleInfo> {
        self.modules.get(path)
    }

    /// Returns the crate root module.
    pub fn root(&self) -> &ModuleInfo {
        self.modules.get(&ModulePath::new()).unwrap()
    }

    /// Returns the direct exports of the given module.
    pub fn direct_exports(&self, module: &[String]) -> &[Export] {
        self.modules
            .get(module)
            .map(|m| m.exports.as_slice())
            .unwrap_or(&[])
    }

    /// Returns all exports visible from the given observer module.
    ///
    /// An export is visible if its effective visibility boundary is the
    /// observer module or an ancestor of it.
    pub fn exports_visible_from(&self, observer: &[String]) -> Vec<&Export> {
        self.modules
            .values()
            .flat_map(|m| m.exports.iter())
            .filter(|e| e.is_visible_from(observer))
            .collect()
    }

    /// Returns all diagnostics produced while building the view.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::File;
    use alloc::vec;

    fn parse(src: &str) -> File {
        crate::parse_str(src).unwrap()
    }

    fn names<'a>(exports: &[&'a Export]) -> Vec<&'a str> {
        exports.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn pub_super_visible_to_parent_not_sibling() {
        let file = parse(
            r#"
            mod a {
                pub mod b {
                    pub(super) fn parent_only() {}
                    pub(crate) fn crate_wide() {}
                    pub fn everywhere() {}
                }
            }
            mod c {
                pub fn call() {}
            }
            "#,
        );
        let view = ModuleView::from_file(&file);

        let from_a = view.exports_visible_from(&["a".into()]);
        let from_a_names = names(&from_a);
        assert!(from_a_names.contains(&"parent_only"));
        assert!(from_a_names.contains(&"crate_wide"));
        assert!(from_a_names.contains(&"everywhere"));

        let from_c = view.exports_visible_from(&["c".into()]);
        let from_c_names = names(&from_c);
        assert!(!from_c_names.contains(&"parent_only"));
        assert!(from_c_names.contains(&"crate_wide"));
        assert!(from_c_names.contains(&"everywhere"));
    }

    #[test]
    fn private_visible_in_declaring_module_and_descendants() {
        let file = parse(
            r#"
            mod a {
                pub mod b {
                    fn private_item() {}
                    pub(in self) fn self_item() {}
                }
            }
            "#,
        );
        let view = ModuleView::from_file(&file);

        let from_a = view.exports_visible_from(&["a".into()]);
        assert!(!names(&from_a).contains(&"private_item"));
        assert!(!names(&from_a).contains(&"self_item"));

        let from_b = view.exports_visible_from(&["a".into(), "b".into()]);
        let from_b_names = names(&from_b);
        assert!(from_b_names.contains(&"private_item"));
        assert!(from_b_names.contains(&"self_item"));
    }

    #[test]
    fn pub_in_ancestor_resolves_to_real_ancestor() {
        let file = parse(
            r#"
            mod a {
                pub mod b {
                    pub(in crate::a) fn in_a() {}
                    pub(in crate::a::b) fn in_b() {}
                }
            }
            mod c {}
            "#,
        );
        let view = ModuleView::from_file(&file);

        let from_a = view.exports_visible_from(&["a".into()]);
        assert!(names(&from_a).contains(&"in_a"));
        // `pub(in crate::a::b)` is visible in `a::b` and descendants, not in
        // the parent `a`.
        assert!(!names(&from_a).contains(&"in_b"));

        let from_b = view.exports_visible_from(&["a".into(), "b".into()]);
        assert!(names(&from_b).contains(&"in_a"));
        assert!(names(&from_b).contains(&"in_b"));

        let from_c = view.exports_visible_from(&["c".into()]);
        assert!(!names(&from_c).contains(&"in_a"));
        assert!(!names(&from_c).contains(&"in_b"));
    }

    #[test]
    fn illegal_ancestor_paths_give_diagnostics() {
        let file = parse(
            r#"
            mod a {
                pub mod b {
                    pub(super) fn super_ok() {}
                    pub(in crate::c) fn bad_ancestor() {}
                    pub(in super::super::super) fn too_many_supers() {}
                    pub(in a::b) fn relative_path() {}
                }
            }
            mod c {}
            pub(super) fn root_super() {}
            "#,
        );
        let view = ModuleView::from_file(&file);
        let messages: Vec<&str> =
            view.diagnostics().iter().map(|d| d.message.as_str()).collect();

        assert!(messages.iter().any(|m| m.contains("ancestor modules")));
        assert!(messages.iter().any(|m| m.contains("too many leading `super`")));
        assert!(messages.iter().any(|m| m.contains("relative paths")));
    }

    #[test]
    fn external_module_declaration_is_unknown_boundary() {
        let file = parse(
            r#"
            mod external;
            mod a {
                pub use crate::external::Thing;
                pub use crate::external::*;
            }
            "#,
        );
        let view = ModuleView::from_file(&file);

        let ext = view.module(&["external".into()]).unwrap();
        assert!(ext.is_external);

        let reexports: Vec<&Export> = view
            .direct_exports(&["a".into()])
            .iter()
            .filter(|e| e.reexport.is_some())
            .collect();
        assert_eq!(reexports.len(), 2);
        for r in &reexports {
            assert!(r.reexport.as_ref().unwrap().unknown_boundary);
        }
    }

    #[test]
    fn reexport_intersection_narrows() {
        let file = parse(
            r#"
            mod a {
                pub fn pub_item() {}
                pub(crate) fn crate_item() {}
            }
            mod b {
                pub(crate) use crate::a::pub_item as re_crate;
                pub use crate::a::crate_item as re_pub;
            }
            "#,
        );
        let view = ModuleView::from_file(&file);

        let reexports: Vec<&Export> = view
            .direct_exports(&["b".into()])
            .iter()
            .filter(|e| e.reexport.is_some())
            .collect();
        assert_eq!(reexports.len(), 2);

        let re_crate = reexports.iter().find(|e| e.name == "re_crate").unwrap();
        assert_eq!(re_crate.boundary, Some(ModulePath::new()));

        let re_pub = reexports.iter().find(|e| e.name == "re_pub").unwrap();
        assert_eq!(re_pub.boundary, Some(ModulePath::new()));
        assert!(view
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("widens visibility")));
    }

    #[test]
    fn three_module_example() {
        let file = parse(
            r#"
            mod a {
                pub mod b {
                    pub fn everywhere() {}
                    pub(crate) fn crate_wide() {}
                    pub(super) fn a_only() {}
                    fn b_private() {}
                }
                pub fn in_a() {}
            }
            mod c {
                pub fn in_c() {}
            }
            "#,
        );
        let view = ModuleView::from_file(&file);

        // From `a`: sees a's items and a::b's pub/pub(crate)/pub(super) items.
        let from_a = view.exports_visible_from(&["a".into()]);
        let from_a_names = names(&from_a);
        assert!(from_a_names.contains(&"in_a"));
        assert!(from_a_names.contains(&"everywhere"));
        assert!(from_a_names.contains(&"crate_wide"));
        assert!(from_a_names.contains(&"a_only"));
        assert!(!from_a_names.contains(&"b_private"));

        // From `c`: sees only pub and pub(crate) items.
        let from_c = view.exports_visible_from(&["c".into()]);
        let from_c_names = names(&from_c);
        assert!(from_c_names.contains(&"in_c"));
        assert!(from_c_names.contains(&"everywhere"));
        assert!(from_c_names.contains(&"crate_wide"));
        assert!(!from_c_names.contains(&"a_only"));
        assert!(!from_c_names.contains(&"b_private"));

        // From `a::b`: sees everything in a::b.
        let from_b = view.exports_visible_from(&["a".into(), "b".into()]);
        let from_b_names = names(&from_b);
        assert!(from_b_names.contains(&"b_private"));
        assert!(from_b_names.contains(&"a_only"));
    }

    #[test]
    fn visibility_source_display() {
        assert_eq!(VisibilitySource::Public.to_string(), "pub");
        assert_eq!(VisibilitySource::Crate.to_string(), "pub(crate)");
        assert_eq!(VisibilitySource::Super.to_string(), "pub(super)");
        assert_eq!(
            VisibilitySource::In(vec!["crate".into(), "a".into()]).to_string(),
            "pub(in crate::a)"
        );
        assert_eq!(VisibilitySource::Private.to_string(), "private");
    }
}
