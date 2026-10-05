//! Module export view built on `syn`.
//!
//! Given an inline module AST (a [`syn::File`] or an inline [`syn::ItemMod`])
//! and an observer module path, decide for every declared item whether it is
//! visible to the observer.
//!
//! Visibility restrictions (`pub`, `pub(crate)`, `pub(super)`, `pub(in path)`)
//! are resolved along the *real* module ancestor chain of the inline AST:
//! `pub(super)` means "visible to the parent module of the declaring module",
//! it is **not** treated as crate-visible. `pub(in path)` must name an
//! ancestor module of the declaring module; anything else is a diagnostic.
//!
//! External module declarations (`mod ext;`) are an unknown boundary: they
//! are reported as [`Verdict::Unknown`] and their files are never read.
//! Macros are never expanded.

use std::fmt::Write as _;

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Item, ItemMod, ItemUse, UseTree, Visibility};

/// Visibility decision for one item, from the observer's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The item is visible to the observer.
    Visible,
    /// The item exists but is not visible to the observer.
    Hidden,
    /// Cannot be decided from the inline AST alone (external `mod name;`
    /// boundary or unresolvable path). No files were read to decide this.
    Unknown(String),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Visible => f.write_str("visible"),
            Verdict::Hidden => f.write_str("hidden"),
            Verdict::Unknown(reason) => write!(f, "unknown({reason})"),
        }
    }
}

/// One row of the export view: a declared item, where it was declared, the
/// visibility it was declared with, and whether the observer can see it.
#[derive(Debug, Clone)]
pub struct ItemReport {
    /// Path of the module that declares the item, e.g. `["a", "b"]`.
    pub module: Vec<String>,
    /// Item name (for `use`, the re-exported name).
    pub name: String,
    /// Item kind: "fn", "struct", "mod", "use", ...
    pub kind: &'static str,
    /// (line, column) of the start of the item declaration, 1-based line.
    pub span: (usize, usize),
    /// Source of the visibility as written: "pub", "pub(crate)",
    /// "pub(super)", "pub(in crate::a)", "private", ...
    pub visibility: String,
    /// (line, column) of the visibility token itself.
    pub visibility_span: (usize, usize),
    /// Decision for the observer.
    pub verdict: Verdict,
}

/// A non-fatal problem found while building the view.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// (line, column) the diagnostic points at.
    pub span: (usize, usize),
    pub message: String,
}

/// The resulting export view for one observer.
#[derive(Debug, Default)]
pub struct ExportView {
    pub items: Vec<ItemReport>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ExportView {
    /// Look up the report for one item by name.
    pub fn get(&self, name: &str) -> Option<&ItemReport> {
        self.items.iter().find(|r| r.name == name)
    }
}

/// Analyze a parsed crate root (`syn::parse_file` output).
///
/// `observer` is the observing module path from the crate root, e.g.
/// `&["a", "b"]` for `a::b`. `&[]` observes from the crate root.
pub fn analyze_file(file: &syn::File, observer: &[&str]) -> ExportView {
    let root = build_tree(&file.items);
    let mut view = ExportView::default();
    let observer = check_observer(&root, observer, &mut view);
    walk(&file.items, &mut Vec::new(), &root, &observer, &mut view);
    view
}

/// Analyze an inline module AST (`mod name { ... }`). The module itself
/// becomes the first path segment, so an item directly inside `mod m` is
/// declared in module `["m"]`.
pub fn analyze_module(module: &ItemMod, observer: &[&str]) -> ExportView {
    let mut view = ExportView::default();
    let name = module.ident.to_string();
    let Some((_, items)) = &module.content else {
        view.diagnostics.push(Diagnostic {
            span: span_pos(module.ident.span()),
            message: format!(
                "external module declaration `mod {name};` has no inline body; \
                 its file is not read"
            ),
        });
        return view;
    };
    let mut root = ModTree::default();
    root.children.push((name.clone(), build_tree(items)));
    let observer = check_observer(&root, observer, &mut view);
    walk(items, &mut vec![name], &root, &observer, &mut view);
    view
}

// --- module tree -----------------------------------------------------------

#[derive(Default)]
struct ModTree {
    /// Declared as `mod name;` -- contents unknown, file never read.
    external: bool,
    children: Vec<(String, ModTree)>,
}

fn build_tree(items: &[Item]) -> ModTree {
    let mut tree = ModTree::default();
    for item in items {
        if let Item::Mod(m) = item {
            let child = match &m.content {
                Some((_, items)) => build_tree(items),
                None => ModTree {
                    external: true,
                    ..ModTree::default()
                },
            };
            tree.children.push((m.ident.to_string(), child));
        }
    }
    tree
}

enum Lookup<'a> {
    Inline(&'a ModTree),
    External,
    Missing,
}

fn lookup<'a>(root: &'a ModTree, path: &[String]) -> Lookup<'a> {
    let mut node = root;
    for seg in path {
        match node.children.iter().find(|(name, _)| name == seg) {
            Some((_, child)) if child.external => return Lookup::External,
            Some((_, child)) => node = child,
            None => return Lookup::Missing,
        }
    }
    Lookup::Inline(node)
}

/// Why a module path could not be resolved inside the inline AST.
enum Boundary {
    /// The path crosses an external `mod name;` declaration.
    External(String),
    /// The path names a module that does not exist in the inline AST.
    Missing(String),
}

fn descend(root: &ModTree, base: &[String], seg: &str) -> Result<Vec<String>, Boundary> {
    let mut path = base.to_vec();
    path.push(seg.to_string());
    match lookup(root, &path) {
        Lookup::Inline(_) => Ok(path),
        Lookup::External => Err(Boundary::External(path.join("::"))),
        Lookup::Missing => Err(Boundary::Missing(path.join("::"))),
    }
}

/// Resolve a module path (segments of a `pub(in ...)` path or the module
/// part of a `use` path) relative to module `from`, returning an absolute
/// path from the crate root.
fn resolve_segments(
    root: &ModTree,
    from: &[String],
    segs: &[String],
) -> Result<Vec<String>, Boundary> {
    let mut iter = segs.iter();
    let Some(first) = iter.next() else {
        return Ok(from.to_vec());
    };
    let mut cur: Vec<String> = match first.as_str() {
        "crate" => Vec::new(),
        "self" => from.to_vec(),
        "super" => {
            if from.is_empty() {
                return Err(Boundary::Missing("`super` at the crate root".to_string()));
            }
            from[..from.len() - 1].to_vec()
        }
        name => descend(root, from, name)?,
    };
    for seg in iter {
        match seg.as_str() {
            "self" => {}
            "super" => {
                if cur.pop().is_none() {
                    return Err(Boundary::Missing(
                        "`super` above the crate root".to_string(),
                    ));
                }
            }
            name => cur = descend(root, &cur, name)?,
        }
    }
    Ok(cur)
}

// --- visibility evaluation -------------------------------------------------

fn span_pos(span: Span) -> (usize, usize) {
    let start = span.start();
    (start.line, start.column)
}

fn path_string(path: &syn::Path) -> String {
    let mut s = String::new();
    if path.leading_colon.is_some() {
        s.push_str("::");
    }
    for (i, seg) in path.segments.iter().enumerate() {
        if i > 0 {
            s.push_str("::");
        }
        let _ = write!(s, "{}", seg.ident);
    }
    s
}

fn visibility_string(vis: &Visibility) -> String {
    match vis {
        Visibility::Public(_) => "pub".to_string(),
        Visibility::Inherited => "private".to_string(),
        Visibility::Restricted(r) => {
            let path = path_string(&r.path);
            if r.in_token.is_some() {
                format!("pub(in {path})")
            } else {
                format!("pub({path})")
            }
        }
    }
}

fn visible_within(observer: &[String], module: &[String]) -> Verdict {
    if observer.starts_with(module) {
        Verdict::Visible
    } else {
        Verdict::Hidden
    }
}

fn eval_visibility(
    vis: &Visibility,
    module_path: &[String],
    observer: &[String],
    root: &ModTree,
    diagnostics: &mut Vec<Diagnostic>,
) -> Verdict {
    match vis {
        Visibility::Public(_) => Verdict::Visible,
        Visibility::Inherited => visible_within(observer, module_path),
        Visibility::Restricted(r) => {
            let segs: Vec<String> = r
                .path
                .segments
                .iter()
                .map(|seg| seg.ident.to_string())
                .collect();
            let pos = span_pos(r.path.span());
            let joined = segs.join("::");
            if r.in_token.is_none() {
                match segs.first().map(String::as_str) {
                    Some("crate") => Verdict::Visible,
                    Some("self") => visible_within(observer, module_path),
                    Some("super") => {
                        if module_path.is_empty() {
                            diagnostics.push(Diagnostic {
                                span: pos,
                                message: "`pub(super)` has no parent module at the crate root"
                                    .to_string(),
                            });
                            Verdict::Hidden
                        } else {
                            // The parent module of the declaring module --
                            // not the whole crate.
                            visible_within(observer, &module_path[..module_path.len() - 1])
                        }
                    }
                    _ => {
                        diagnostics.push(Diagnostic {
                            span: pos,
                            message: format!("unsupported restricted visibility `pub({joined})`"),
                        });
                        Verdict::Unknown(format!("unsupported restriction `pub({joined})`"))
                    }
                }
            } else {
                match resolve_segments(root, module_path, &segs) {
                    Err(Boundary::External(m)) => Verdict::Unknown(format!(
                        "`pub(in {joined})` crosses external module boundary `{m}`"
                    )),
                    Err(Boundary::Missing(m)) => {
                        diagnostics.push(Diagnostic {
                            span: pos,
                            message: format!("cannot resolve `pub(in {joined})`: {m}"),
                        });
                        Verdict::Unknown(format!("unresolved `pub(in {joined})`"))
                    }
                    Ok(target) => {
                        if !module_path.starts_with(&target) {
                            diagnostics.push(Diagnostic {
                                span: pos,
                                message: format!(
                                    "`pub(in {joined})`: `{}` is not an ancestor module of `{}`",
                                    if target.is_empty() {
                                        "crate".to_string()
                                    } else {
                                        target.join("::")
                                    },
                                    if module_path.is_empty() {
                                        "crate".to_string()
                                    } else {
                                        module_path.join("::")
                                    },
                                ),
                            });
                            Verdict::Hidden
                        } else {
                            visible_within(observer, &target)
                        }
                    }
                }
            }
        }
    }
}

// --- use re-exports --------------------------------------------------------

/// Collect the module part of every path in a `use` tree. For
/// `use a::b::{c, d}` this yields `["a","b"]`; for `use a::*` it yields
/// `["a"]`.
fn use_module_paths(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            use_module_paths(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) => out.push(prefix.clone()),
        UseTree::Group(g) => {
            for t in &g.items {
                use_module_paths(t, prefix, out);
            }
        }
    }
}

/// If a re-export's target cannot be followed inside the inline AST (it
/// crosses an external `mod name;` boundary or does not resolve), explain
/// why. Never reads files.
fn reexport_boundary(root: &ModTree, module_path: &[String], u: &ItemUse) -> Option<String> {
    let mut paths = Vec::new();
    use_module_paths(&u.tree, &mut Vec::new(), &mut paths);
    for module_part in &paths {
        match resolve_segments(root, module_path, module_part) {
            Ok(_) => {}
            Err(Boundary::External(m)) => {
                return Some(format!("re-export crosses external module boundary `{m}`"));
            }
            Err(Boundary::Missing(m)) => {
                return Some(format!(
                    "re-export target does not resolve in the inline AST ({m}); \
                     it may live in an external crate or module"
                ));
            }
        }
    }
    None
}

fn use_name(tree: &UseTree) -> String {
    match tree {
        UseTree::Name(n) => n.ident.to_string(),
        UseTree::Rename(r) => r.rename.to_string(),
        UseTree::Glob(_) => "*".to_string(),
        UseTree::Path(p) => use_name(&p.tree),
        UseTree::Group(_) => "{..}".to_string(),
    }
}

// --- item walk -------------------------------------------------------------

fn item_name_kind(item: &Item) -> Option<(String, &'static str)> {
    Some(match item {
        Item::Fn(x) => (x.sig.ident.to_string(), "fn"),
        Item::Struct(x) => (x.ident.to_string(), "struct"),
        Item::Enum(x) => (x.ident.to_string(), "enum"),
        Item::Union(x) => (x.ident.to_string(), "union"),
        Item::Trait(x) => (x.ident.to_string(), "trait"),
        Item::Mod(x) => (x.ident.to_string(), "mod"),
        Item::Const(x) => (x.ident.to_string(), "const"),
        Item::Static(x) => (x.ident.to_string(), "static"),
        Item::Type(x) => (x.ident.to_string(), "type"),
        Item::ExternCrate(x) => (x.ident.to_string(), "extern crate"),
        Item::Use(x) => (use_name(&x.tree), "use"),
        // Macro invocations are not expanded; impl blocks and anything else
        // declare no nameable export.
        _ => return None,
    })
}

fn item_vis(item: &Item) -> Option<&Visibility> {
    Some(match item {
        Item::Fn(x) => &x.vis,
        Item::Struct(x) => &x.vis,
        Item::Enum(x) => &x.vis,
        Item::Union(x) => &x.vis,
        Item::Trait(x) => &x.vis,
        Item::Mod(x) => &x.vis,
        Item::Const(x) => &x.vis,
        Item::Static(x) => &x.vis,
        Item::Type(x) => &x.vis,
        Item::ExternCrate(x) => &x.vis,
        Item::Use(x) => &x.vis,
        _ => return None,
    })
}

fn check_observer(root: &ModTree, observer: &[&str], view: &mut ExportView) -> Vec<String> {
    let observer: Vec<String> = observer.iter().map(|s| (*s).to_string()).collect();
    match lookup(root, &observer) {
        Lookup::Inline(_) => {}
        Lookup::External => view.diagnostics.push(Diagnostic {
            span: (0, 0),
            message: format!(
                "observer `{}` is inside an external module declaration; \
                 results reflect the inline AST only",
                observer.join("::")
            ),
        }),
        Lookup::Missing => view.diagnostics.push(Diagnostic {
            span: (0, 0),
            message: format!(
                "observer module path `{}` not found in the inline AST",
                observer.join("::")
            ),
        }),
    }
    observer
}

fn walk(
    items: &[Item],
    module_path: &mut Vec<String>,
    root: &ModTree,
    observer: &[String],
    view: &mut ExportView,
) {
    for item in items {
        let Some((name, kind)) = item_name_kind(item) else {
            continue;
        };
        let Some(vis) = item_vis(item) else {
            continue;
        };

        // External module declaration: report the boundary, never read a file.
        if let Item::Mod(m) = item {
            if m.content.is_none() {
                view.items.push(ItemReport {
                    module: module_path.clone(),
                    name,
                    kind,
                    span: span_pos(item.span()),
                    visibility: visibility_string(vis),
                    visibility_span: span_pos(vis.span()),
                    verdict: Verdict::Unknown(
                        "external module declaration; its file is not read".to_string(),
                    ),
                });
                continue;
            }
        }

        let mut verdict = eval_visibility(vis, module_path, observer, root, &mut view.diagnostics);

        // A `use` re-export is only as knowable as its target.
        if let Item::Use(u) = item {
            if !matches!(u.vis, Visibility::Inherited) && verdict == Verdict::Visible {
                if let Some(reason) = reexport_boundary(root, module_path, u) {
                    verdict = Verdict::Unknown(reason);
                }
            }
        }

        view.items.push(ItemReport {
            module: module_path.clone(),
            name,
            kind,
            span: span_pos(item.span()),
            visibility: visibility_string(vis),
            visibility_span: span_pos(vis.span()),
            verdict,
        });

        if let Item::Mod(m) = item {
            if let Some((_, content)) = &m.content {
                module_path.push(m.ident.to_string());
                walk(content, module_path, root, observer, view);
                module_path.pop();
            }
        }
    }
}
