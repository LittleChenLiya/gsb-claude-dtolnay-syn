//! Public-interface fingerprinting for Rust source files, built on [`syn`].
//!
//! The analyzer takes a parsed [`syn::File`] (or raw source text) of a crate
//! root and produces a deterministic, normalized description of the public
//! interface of the crate — public functions (including inherent methods),
//! structs, enums, traits and `pub use` re-exports — together with a
//! SHA-256 digest.
//!
//! **No compilation of the analyzed crate is required**: everything is
//! derived from the syntax tree alone.
//!
//! # Normalization rules
//!
//! The digest is insensitive to changes that do not affect the public
//! interface and sensitive to those that do:
//!
//! * Whitespace, comments, doc comments and function bodies are ignored.
//! * Generic constraints, `where` clauses, visibility, field order, enum
//!   discriminants, `#[repr(...)]`, `#[non_exhaustive]`, `#[cfg(...)]` and
//!   function ABI (`extern "C"` etc.) are preserved.
//! * Lifetimes that are explicitly bound on a generic parameter list of the
//!   item are alpha-renamed to `'lt0`, `'lt1`, ... in declaration order, so
//!   renaming a bound lifetime does not change the digest. Two independently
//!   bound lifetimes always map to two distinct names and are never merged.
//!   Elided (`&str`) and `'_` / `'static` lifetimes are left untouched.
//! * Raw identifiers are normalized (`r#type` and `type` are the same name).
//! * Generic parameter defaults (`struct S<T = u32>`) are preserved.
//! * Items inside private modules are not exports and never appear in the
//!   report. Nested `pub mod` paths are reflected in entry paths
//!   (`outer::inner::f`).
//!
//! # Unknown boundaries
//!
//! Anything whose public-API effect cannot be decided from the AST of a
//! single file is recorded as an [`Unknown`] with a source location, and the
//! fingerprint is then *incomplete*: it must not be reported as definitely
//! compatible. Unknown boundaries are:
//!
//! * macro invocations in exported scope (they may generate public items),
//!   including `#[derive(...)]` output;
//! * `pub use` of paths that cannot be resolved inside this file (external
//!   crates) — the textual path is preserved and feeds the digest;
//! * glob re-exports (`pub use foo::*`);
//! * out-of-line modules (`pub mod ext;`) whose file is not part of the
//!   input;
//! * unparseable items (`syn::Item::Verbatim`).

mod sha256;

use proc_macro2::{Ident, Span, TokenStream};
use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use syn::visit_mut::{self, VisitMut};
use syn::{
    Attribute, Field, ImplItem, Item, ItemFn, ItemImpl, ItemUse, Lifetime, LifetimeParam, Macro,
    TraitItem, TraitItemFn, Type, UseTree, Variant, Visibility,
};

pub use sha256::sha256_hex;

/// The kind of an exported interface entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Use,
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ItemKind::Fn => "fn",
            ItemKind::Struct => "struct",
            ItemKind::Enum => "enum",
            ItemKind::Trait => "trait",
            ItemKind::Use => "use",
        };
        f.write_str(s)
    }
}

/// Why a boundary of the public interface could not be resolved statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnknownReason {
    /// A macro invocation (or `#[derive]`) may generate public items.
    MacroGenerated,
    /// `pub use` of a path that cannot be resolved within this file.
    ExternalReexport,
    /// `pub use foo::*` — the globbed set of names is not known.
    GlobReexport,
    /// `pub mod name;` — the module body lives in another file.
    OutOfLineModule,
    /// An item syn could not parse into a structured form.
    UnresolvedItem,
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            UnknownReason::MacroGenerated => "macro-generated",
            UnknownReason::ExternalReexport => "external-reexport",
            UnknownReason::GlobReexport => "glob-reexport",
            UnknownReason::OutOfLineModule => "out-of-line-module",
            UnknownReason::UnresolvedItem => "unresolved-item",
        };
        f.write_str(s)
    }
}

/// A 1-based-line / 0-based-column source position for diff localization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// One normalized exported item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: ItemKind,
    /// Crate-relative path, e.g. `outer::inner::compute` or `Point::norm`.
    pub path: String,
    /// Normalized signature: canonical token spacing, no body, no comments,
    /// alpha-renamed lifetimes, normalized raw identifiers.
    pub signature: String,
    pub location: Location,
}

/// A boundary of the public interface that could not be resolved statically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    pub reason: UnknownReason,
    /// Deterministic textual detail (e.g. the textual `pub use` path or the
    /// macro invocation). Feeds the digest.
    pub detail: String,
    pub location: Location,
}

/// The fingerprint of one parsed file.
#[derive(Debug, Clone)]
pub struct Fingerprint {
    /// Known exported items, sorted by `(path, kind)`.
    pub entries: Vec<Entry>,
    /// Unresolvable export boundaries, sorted by source location.
    pub unknowns: Vec<Unknown>,
    /// SHA-256 (hex) over the canonical form of `entries` and `unknowns`.
    /// Source locations are deliberately excluded so that moving items
    /// around or reformatting does not change the digest.
    pub digest: String,
}

impl Fingerprint {
    /// True when no unknown boundaries were encountered. Only a complete
    /// fingerprint may be treated as definite evidence of compatibility.
    pub fn is_complete(&self) -> bool {
        self.unknowns.is_empty()
    }

    /// Human-readable canonical report (also usable as a golden fixture).
    pub fn report(&self) -> String {
        let mut out = String::from("api-fingerprint v1\n");
        out.push_str(&format!("digest: {}\n", self.digest));
        if self.unknowns.is_empty() {
            out.push_str("status: complete\n");
        } else {
            out.push_str(&format!(
                "status: incomplete ({} unknown item(s); compatibility cannot be confirmed)\n",
                self.unknowns.len()
            ));
        }
        for e in &self.entries {
            out.push_str(&format!(
                "\nentry {} {} @ {}\n  {}\n",
                e.kind, e.path, e.location, e.signature
            ));
        }
        for u in &self.unknowns {
            out.push_str(&format!(
                "\nunknown {} @ {}\n  {}\n",
                u.reason, u.location, u.detail
            ));
        }
        out
    }
}

/// Fingerprint an already-parsed file AST.
pub fn fingerprint_file(file: &syn::File) -> Fingerprint {
    let mut collector = Collector::default();
    collect_items(&file.items, &mut collector);
    let mut entries = collector.entries;
    let mut unknowns = collector.unknowns;
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.kind.cmp(&b.kind)));
    unknowns.sort_by(|a, b| {
        a.location
            .cmp(&b.location)
            .then(a.reason.to_string().cmp(&b.reason.to_string()))
            .then(a.detail.cmp(&b.detail))
    });
    let digest = sha256_hex(canonical(&entries, &unknowns).as_bytes());
    Fingerprint {
        entries,
        unknowns,
        digest,
    }
}

/// Parse `source` as a crate root and fingerprint it.
pub fn fingerprint_source(source: &str) -> Result<Fingerprint, syn::Error> {
    let file = syn::parse_file(source)?;
    Ok(fingerprint_file(&file))
}

/// Canonical digest input: one line per entry/unknown, sorted.
/// Locations are intentionally not part of the digest.
fn canonical(entries: &[Entry], unknowns: &[Unknown]) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| format!("entry\t{}\t{}\t{}", e.kind, e.path, e.signature))
        .chain(
            unknowns
                .iter()
                .map(|u| format!("unknown\t{}\t{}", u.reason, u.detail)),
        )
        .collect();
    lines.sort();
    lines.join("\n")
}

/// The result of comparing two fingerprints.
#[derive(Debug, Clone)]
pub struct Diff {
    pub added: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub changed: Vec<EntryChange>,
    /// Unknown boundaries of the *new* fingerprint. While any of these are
    /// present, compatibility cannot be confirmed.
    pub unknowns: Vec<Unknown>,
}

#[derive(Debug, Clone)]
pub struct EntryChange {
    pub kind: ItemKind,
    pub path: String,
    pub before: String,
    pub after: String,
    pub location: Location,
}

impl Diff {
    /// No added, removed or changed entries.
    pub fn is_identical(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// True only when the known surface is identical *and* there are no
    /// unknown boundaries that could hide further differences.
    pub fn is_definitely_compatible(&self) -> bool {
        self.is_identical() && self.unknowns.is_empty()
    }
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.changed {
            writeln!(f, "~ {} {}", c.kind, c.path)?;
            writeln!(f, "  - {}", c.before)?;
            writeln!(f, "  + {}", c.after)?;
        }
        for e in &self.added {
            writeln!(f, "+ {} {}", e.kind, e.path)?;
            writeln!(f, "    {}", e.signature)?;
        }
        for e in &self.removed {
            writeln!(f, "- {} {}", e.kind, e.path)?;
            writeln!(f, "    {}", e.signature)?;
        }
        for u in &self.unknowns {
            writeln!(f, "! unknown {} @ {}", u.reason, u.location)?;
            writeln!(f, "    {}", u.detail)?;
        }
        writeln!(
            f,
            "summary: {} added, {} removed, {} changed, {} unknown",
            self.added.len(),
            self.removed.len(),
            self.changed.len(),
            self.unknowns.len()
        )?;
        if self.is_definitely_compatible() {
            writeln!(f, "verdict: interfaces identical (complete)")?;
        } else if self.is_identical() {
            writeln!(
                f,
                "verdict: known surface identical, but unknown boundaries \
                 prevent a definite compatibility claim"
            )?;
        } else {
            writeln!(f, "verdict: interface changed")?;
        }
        Ok(())
    }
}

/// Compare two fingerprints, matching entries by `(kind, path)`.
pub fn diff(old: &Fingerprint, new: &Fingerprint) -> Diff {
    let old_map: BTreeMap<(ItemKind, &str), &Entry> = old
        .entries
        .iter()
        .map(|e| ((e.kind, e.path.as_str()), e))
        .collect();
    let new_map: BTreeMap<(ItemKind, &str), &Entry> = new
        .entries
        .iter()
        .map(|e| ((e.kind, e.path.as_str()), e))
        .collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for e in &new.entries {
        match old_map.get(&(e.kind, e.path.as_str())) {
            None => added.push(e.clone()),
            Some(o) if o.signature != e.signature => changed.push(EntryChange {
                kind: e.kind,
                path: e.path.clone(),
                before: o.signature.clone(),
                after: e.signature.clone(),
                location: e.location,
            }),
            _ => {}
        }
    }
    for e in &old.entries {
        if !new_map.contains_key(&(e.kind, e.path.as_str())) {
            removed.push(e.clone());
        }
    }

    Diff {
        added,
        removed,
        changed,
        unknowns: new.unknowns.clone(),
    }
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Collector {
    /// Path of the `pub mod` nesting currently being walked.
    modules: Vec<String>,
    entries: Vec<Entry>,
    unknowns: Vec<Unknown>,
}

impl Collector {
    fn path(&self, name: &str) -> String {
        let mut p = self.modules.join("::");
        if !p.is_empty() {
            p.push_str("::");
        }
        p.push_str(name);
        p
    }
}

fn is_public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn loc(span: Span) -> Location {
    let lc = span.start();
    Location {
        line: lc.line,
        column: lc.column,
    }
}

fn ident_str(ident: &Ident) -> String {
    let s = ident.to_string();
    match s.strip_prefix("r#") {
        Some(raw) => raw.to_string(),
        None => s,
    }
}

fn collect_items(items: &[Item], c: &mut Collector) {
    // Names of modules declared at this level; used to classify the first
    // segment of a `pub use` path as internal (resolvable) vs external.
    let local_mods: BTreeSet<String> = items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(m) => Some(ident_str(&m.ident)),
            _ => None,
        })
        .collect();

    for item in items {
        match item {
            Item::Fn(f) => {
                if is_public(&f.vis) {
                    derive_unknown(&f.attrs, c, f.sig.ident.span());
                    c.entries.push(fn_entry(c, f));
                }
            }
            Item::Struct(s) => {
                if is_public(&s.vis) {
                    derive_unknown(&s.attrs, c, s.ident.span());
                    c.entries.push(whole_entry(
                        c,
                        ItemKind::Struct,
                        &ident_str(&s.ident),
                        Item::Struct(s.clone()),
                        s.ident.span(),
                    ));
                }
            }
            Item::Enum(e) => {
                if is_public(&e.vis) {
                    derive_unknown(&e.attrs, c, e.ident.span());
                    c.entries.push(whole_entry(
                        c,
                        ItemKind::Enum,
                        &ident_str(&e.ident),
                        Item::Enum(e.clone()),
                        e.ident.span(),
                    ));
                }
            }
            Item::Trait(t) => {
                if is_public(&t.vis) {
                    derive_unknown(&t.attrs, c, t.ident.span());
                    for ti in &t.items {
                        match ti {
                            TraitItem::Macro(m) => c.unknowns.push(macro_unknown(&m.mac)),
                            TraitItem::Verbatim(ts) => c.unknowns.push(verbatim_unknown(ts)),
                            _ => {}
                        }
                    }
                    c.entries.push(whole_entry(
                        c,
                        ItemKind::Trait,
                        &ident_str(&t.ident),
                        Item::Trait(t.clone()),
                        t.ident.span(),
                    ));
                }
            }
            Item::Mod(m) => {
                // Only `pub` modules are traversed: items under private
                // modules are not exports.
                if is_public(&m.vis) {
                    match &m.content {
                        Some((_, items)) => {
                            c.modules.push(ident_str(&m.ident));
                            collect_items(items, c);
                            c.modules.pop();
                        }
                        None => c.unknowns.push(Unknown {
                            reason: UnknownReason::OutOfLineModule,
                            detail: format!("pub mod {};", ident_str(&m.ident)),
                            location: loc(m.ident.span()),
                        }),
                    }
                }
            }
            Item::Use(u) => {
                if is_public(&u.vis) {
                    handle_use(u, c, &local_mods);
                }
            }
            Item::Impl(imp) => {
                // Inherent impls only; trait impl signatures are covered by
                // the trait entry itself.
                if imp.trait_.is_none() {
                    collect_impl(imp, c);
                }
            }
            Item::Macro(m) => c.unknowns.push(macro_unknown(&m.mac)),
            Item::Verbatim(ts) => c.unknowns.push(verbatim_unknown(ts)),
            _ => {}
        }
    }
}

fn collect_impl(imp: &ItemImpl, c: &mut Collector) {
    for it in &imp.items {
        match it {
            ImplItem::Fn(m) => {
                if is_public(&m.vis) {
                    // Normalize the method together with the impl generics so
                    // that lifetimes and type params bound on the impl block
                    // are renamed consistently.
                    let mut single = imp.clone();
                    single.items = vec![ImplItem::Fn(m.clone())];
                    let signature =
                        normalize(Item::Impl(single)).to_token_stream().to_string();
                    let path = c.path(&format!(
                        "{}::{}",
                        type_name(&imp.self_ty),
                        ident_str(&m.sig.ident)
                    ));
                    c.entries.push(Entry {
                        kind: ItemKind::Fn,
                        path,
                        signature,
                        location: loc(m.sig.ident.span()),
                    });
                }
            }
            ImplItem::Macro(m) => c.unknowns.push(macro_unknown(&m.mac)),
            ImplItem::Verbatim(ts) => c.unknowns.push(verbatim_unknown(ts)),
            _ => {}
        }
    }
}

fn type_name(ty: &Type) -> String {
    if let Type::Path(tp) = ty {
        if tp.qself.is_none() {
            if let Some(seg) = tp.path.segments.last() {
                return ident_str(&seg.ident);
            }
        }
    }
    ty.to_token_stream().to_string()
}

fn fn_entry(c: &Collector, f: &ItemFn) -> Entry {
    let item = normalize(Item::Fn(f.clone()));
    let Item::Fn(f) = item else {
        unreachable!()
    };
    // Signature only: kept attributes, visibility and the `fn` signature.
    // The body never participates.
    let mut ts = TokenStream::new();
    for attr in &f.attrs {
        attr.to_tokens(&mut ts);
    }
    f.vis.to_tokens(&mut ts);
    f.sig.to_tokens(&mut ts);
    Entry {
        kind: ItemKind::Fn,
        path: c.path(&ident_str(&f.sig.ident)),
        signature: ts.to_string(),
        location: loc(f.sig.ident.span()),
    }
}

fn whole_entry(c: &Collector, kind: ItemKind, name: &str, item: Item, span: Span) -> Entry {
    let signature = normalize(item).to_token_stream().to_string();
    Entry {
        kind,
        path: c.path(name),
        signature,
        location: loc(span),
    }
}

fn derive_unknown(attrs: &[Attribute], c: &mut Collector, span: Span) {
    for attr in attrs {
        if attr.path().is_ident("derive") {
            c.unknowns.push(Unknown {
                reason: UnknownReason::MacroGenerated,
                detail: format!(
                    "derive macro output cannot be expanded without compilation: {}",
                    attr.to_token_stream()
                ),
                location: loc(span),
            });
        }
    }
}

fn macro_unknown(mac: &Macro) -> Unknown {
    let span = mac
        .path
        .segments
        .first()
        .map(|s| s.ident.span())
        .unwrap_or(mac.bang_token.span);
    Unknown {
        reason: UnknownReason::MacroGenerated,
        detail: format!(
            "macro invocation may generate public API: {}",
            mac.to_token_stream()
        ),
        location: loc(span),
    }
}

fn verbatim_unknown(ts: &TokenStream) -> Unknown {
    let span = ts
        .clone()
        .into_iter()
        .next()
        .map(|tt| tt.span())
        .unwrap_or_else(Span::call_site);
    Unknown {
        reason: UnknownReason::UnresolvedItem,
        detail: format!("item could not be parsed into a structured form: {ts}"),
        location: loc(span),
    }
}

// ---------------------------------------------------------------------------
// `pub use` handling
// ---------------------------------------------------------------------------

struct UseLeaf {
    path: String,
    rename: Option<String>,
    glob: bool,
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

fn walk_use_tree(tree: &UseTree, prefix: String, out: &mut Vec<UseLeaf>) {
    match tree {
        UseTree::Path(p) => {
            let prefix = join_path(&prefix, &ident_str(&p.ident));
            walk_use_tree(&p.tree, prefix, out);
        }
        UseTree::Name(n) => out.push(UseLeaf {
            path: join_path(&prefix, &ident_str(&n.ident)),
            rename: None,
            glob: false,
        }),
        UseTree::Rename(r) => out.push(UseLeaf {
            path: join_path(&prefix, &ident_str(&r.ident)),
            rename: Some(ident_str(&r.rename)),
            glob: false,
        }),
        UseTree::Glob(_) => out.push(UseLeaf {
            path: prefix,
            rename: None,
            glob: true,
        }),
        UseTree::Group(g) => {
            for t in &g.items {
                walk_use_tree(t, prefix.clone(), out);
            }
        }
    }
}

fn handle_use(u: &ItemUse, c: &mut Collector, local_mods: &BTreeSet<String>) {
    let mut leaves = Vec::new();
    walk_use_tree(&u.tree, String::new(), &mut leaves);
    for leaf in leaves {
        if leaf.glob {
            c.unknowns.push(Unknown {
                reason: UnknownReason::GlobReexport,
                detail: format!("pub use {}::*;", leaf.path),
                location: loc(u.use_token.span),
            });
            continue;
        }
        let alias = leaf.rename.clone().unwrap_or_else(|| {
            leaf.path
                .rsplit("::")
                .next()
                .unwrap_or(&leaf.path)
                .to_string()
        });
        let detail = match &leaf.rename {
            Some(r) => format!("pub use {} as {};", leaf.path, r),
            None => format!("pub use {};", leaf.path),
        };
        let first = leaf.path.split("::").next().unwrap_or("");
        let internal = matches!(first, "self" | "crate" | "super") || local_mods.contains(first);
        if internal {
            // Alias is recorded textually; the target item is fingerprinted
            // at its own definition site.
            c.entries.push(Entry {
                kind: ItemKind::Use,
                path: c.path(&alias),
                signature: detail,
                location: loc(u.use_token.span),
            });
        } else {
            // External crate: never resolved, textual path preserved.
            c.unknowns.push(Unknown {
                reason: UnknownReason::ExternalReexport,
                detail,
                location: loc(u.use_token.span),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Normalization passes
// ---------------------------------------------------------------------------

/// Attributes that are part of the public interface contract. Everything
/// else (doc comments, `#[inline]`, `#[must_use]`, ...) is dropped.
fn keep_attr(attr: &Attribute) -> bool {
    let path = attr.path();
    path.is_ident("repr")
        || path.is_ident("non_exhaustive")
        || path.is_ident("cfg")
        || path.is_ident("derive")
}

fn item_attrs(item: &mut Item) -> Option<&mut Vec<Attribute>> {
    match item {
        Item::Const(x) => Some(&mut x.attrs),
        Item::Enum(x) => Some(&mut x.attrs),
        Item::ExternCrate(x) => Some(&mut x.attrs),
        Item::Fn(x) => Some(&mut x.attrs),
        Item::ForeignMod(x) => Some(&mut x.attrs),
        Item::Impl(x) => Some(&mut x.attrs),
        Item::Macro(x) => Some(&mut x.attrs),
        Item::Mod(x) => Some(&mut x.attrs),
        Item::Static(x) => Some(&mut x.attrs),
        Item::Struct(x) => Some(&mut x.attrs),
        Item::Trait(x) => Some(&mut x.attrs),
        Item::TraitAlias(x) => Some(&mut x.attrs),
        Item::Type(x) => Some(&mut x.attrs),
        Item::Union(x) => Some(&mut x.attrs),
        Item::Use(x) => Some(&mut x.attrs),
        _ => None,
    }
}

/// Drop every attribute that is not interface-relevant, recursively.
struct AttrStripper;

impl VisitMut for AttrStripper {
    fn visit_item_mut(&mut self, i: &mut Item) {
        if let Some(attrs) = item_attrs(i) {
            attrs.retain(keep_attr);
        }
        visit_mut::visit_item_mut(self, i);
    }
    fn visit_field_mut(&mut self, f: &mut Field) {
        f.attrs.retain(keep_attr);
        visit_mut::visit_field_mut(self, f);
    }
    fn visit_variant_mut(&mut self, v: &mut Variant) {
        v.attrs.retain(keep_attr);
        visit_mut::visit_variant_mut(self, v);
    }
    fn visit_trait_item_mut(&mut self, ti: &mut TraitItem) {
        match ti {
            TraitItem::Const(x) => x.attrs.retain(keep_attr),
            TraitItem::Fn(x) => x.attrs.retain(keep_attr),
            TraitItem::Type(x) => x.attrs.retain(keep_attr),
            TraitItem::Macro(x) => x.attrs.retain(keep_attr),
            _ => {}
        }
        visit_mut::visit_trait_item_mut(self, ti);
    }
    fn visit_impl_item_mut(&mut self, ii: &mut ImplItem) {
        match ii {
            ImplItem::Const(x) => x.attrs.retain(keep_attr),
            ImplItem::Fn(x) => x.attrs.retain(keep_attr),
            ImplItem::Type(x) => x.attrs.retain(keep_attr),
            ImplItem::Macro(x) => x.attrs.retain(keep_attr),
            _ => {}
        }
        visit_mut::visit_impl_item_mut(self, ii);
    }
}

/// Remove function bodies: they never participate in the interface.
struct BodyStripper;

impl VisitMut for BodyStripper {
    fn visit_item_fn_mut(&mut self, f: &mut ItemFn) {
        f.block.stmts.clear();
        visit_mut::visit_item_fn_mut(self, f);
    }
    fn visit_trait_item_fn_mut(&mut self, f: &mut TraitItemFn) {
        f.default = None;
        visit_mut::visit_trait_item_fn_mut(self, f);
    }
    fn visit_impl_item_fn_mut(&mut self, f: &mut syn::ImplItemFn) {
        f.block.stmts.clear();
        visit_mut::visit_impl_item_fn_mut(self, f);
    }
}

/// Collect explicitly bound lifetime parameter names in declaration order.
#[derive(Default)]
struct LifetimeCollector {
    names: Vec<String>,
}

impl VisitMut for LifetimeCollector {
    fn visit_lifetime_param_mut(&mut self, lp: &mut LifetimeParam) {
        let name = lp.lifetime.ident.to_string();
        if !self.names.contains(&name) {
            self.names.push(name);
        }
        visit_mut::visit_lifetime_param_mut(self, lp);
    }
}

/// Alpha-rename bound lifetimes to `'lt0`, `'lt1`, ... Only lifetimes that
/// were explicitly declared on a generic parameter list (or `for<>` binder)
/// of the normalized unit are renamed; distinct declarations always receive
/// distinct names, so independent lifetimes are never merged.
struct LifetimeRenamer<'a> {
    map: &'a BTreeMap<String, String>,
}

impl VisitMut for LifetimeRenamer<'_> {
    fn visit_lifetime_mut(&mut self, lt: &mut Lifetime) {
        if let Some(new) = self.map.get(&lt.ident.to_string()) {
            *lt = Lifetime::new(new, lt.span());
        }
    }
}

/// Normalize raw identifiers: `r#type` and `type` denote the same name.
struct RawIdentStripper;

impl VisitMut for RawIdentStripper {
    fn visit_ident_mut(&mut self, id: &mut Ident) {
        let s = id.to_string();
        if let Some(raw) = s.strip_prefix("r#") {
            *id = Ident::new(raw, id.span());
        }
    }
}

/// Run all normalization passes over one exported item (or single-method
/// impl block) and return the normalized AST.
fn normalize(mut item: Item) -> Item {
    AttrStripper.visit_item_mut(&mut item);
    BodyStripper.visit_item_mut(&mut item);
    let mut collector = LifetimeCollector::default();
    collector.visit_item_mut(&mut item);
    if !collector.names.is_empty() {
        let map: BTreeMap<String, String> = collector
            .names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), format!("'lt{i}")))
            .collect();
        LifetimeRenamer { map: &map }.visit_item_mut(&mut item);
    }
    RawIdentStripper.visit_item_mut(&mut item);
    item
}
