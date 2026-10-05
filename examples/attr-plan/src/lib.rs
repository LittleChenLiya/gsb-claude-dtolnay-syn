//! AST attribute edit plan.
//!
//! Select items by explicit path, add/remove outer attributes, preserve inner
//! attributes and unrelated token order.
//!
//! # Usage
//!
//! ```
//! use attr_plan::{AttrSpec, Plan};
//! use syn::parse_quote;
//!
//! let src = r#"
//!     mod a {
//!         fn f() {}
//!     }
//!     mod b {
//!         fn f() {}
//!     }
//! "#;
//! let file = syn::parse_file(src).unwrap();
//!
//! let mut plan = Plan::new();
//! plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());
//!
//! let result = plan.apply(&file).unwrap();
//! // Only a::f got #[inline]; b::f is unchanged.
//! ```
//!
//! # Guarantees
//!
//! - **Explicit path selection** - items are selected by a full path like
//!   `a::f`, not by a bare name.
//! - **Path ambiguity is rejected** - if a path segment matches more than one
//!   item in the same scope, the plan fails before any edit.
//! - **Attribute parameter parsing** - every attribute spec is parsed up front;
//!   a malformed spec fails the plan before any edit.
//! - **`cfg_attr` text is never treated as a direct attribute** - the tokens
//!   inside `#[cfg_attr(...)]` are not parsed as attributes, so removing
//!   `allow` never touches an `allow` nested inside `cfg_attr`.
//! - **Atomic failure** - validation runs entirely on the original file; the
//!   original [`File`] is never mutated. A new [`File`] is returned only when
//!   every step succeeds.
//! - **No item reordering, no function body rewrite** - only the outer
//!   attributes of targeted items are changed; inner attributes and token
//!   order are preserved.

use proc_macro2::Span;
use quote::quote;
use std::collections::HashMap;
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{
    parse_str, Attribute, AttrStyle, File, Ident, Item, Meta, Path, Result,
};

/// A parsed attribute specification: the meta of an outer attribute.
///
/// Parsed from strings like `inline`, `allow(dead_code)`, `derive(Debug)`.
#[derive(Clone)]
pub struct AttrSpec {
    pub meta: Meta,
}

impl AttrSpec {
    /// Parse an attribute specification from a string.
    ///
    /// ```
    /// use attr_plan::AttrSpec;
    /// let spec = AttrSpec::parse("allow(dead_code)").unwrap();
    /// ```
    pub fn parse(s: &str) -> Result<AttrSpec> {
        let meta = parse_str::<Meta>(s)?;
        Ok(AttrSpec { meta })
    }

    /// Construct from a [`syn::Meta`].
    pub fn from_meta(meta: Meta) -> AttrSpec {
        AttrSpec { meta }
    }

    /// The path identifying this attribute (e.g. `allow` in `allow(dead_code)`).
    pub fn path(&self) -> &Path {
        self.meta.path()
    }

    /// Build an outer [`Attribute`] (`#[...]`) from this spec.
    pub fn to_attribute(&self) -> Attribute {
        let meta = &self.meta;
        let tokens = quote! { #[#meta] };
        let attrs = Attribute::parse_outer
            .parse2(tokens)
            .expect("attr-plan: generated attribute should parse");
        attrs
            .into_iter()
            .next()
            .expect("attr-plan: generated attribute should parse")
    }
}

/// An attribute edit: add or remove an outer attribute.
#[derive(Clone)]
pub enum AttrEdit {
    /// Add an outer attribute. Errors if a direct outer attribute with the
    /// same path already exists on the target.
    Add(AttrSpec),
    /// Remove the direct outer attribute with this path. Errors if no such
    /// attribute exists.
    Remove(AttrSpec),
}

impl AttrEdit {
    /// The attribute spec this edit operates on.
    pub fn spec(&self) -> &AttrSpec {
        match self {
            AttrEdit::Add(spec) | AttrEdit::Remove(spec) => spec,
        }
    }

    /// The kind of edit.
    pub fn kind(&self) -> AttrEditKind {
        match self {
            AttrEdit::Add(_) => AttrEditKind::Add,
            AttrEdit::Remove(_) => AttrEditKind::Remove,
        }
    }
}

/// The kind of attribute edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrEditKind {
    Add,
    Remove,
}

/// A plan entry: a target item (by explicit path) and the edits to apply.
#[derive(Clone)]
pub struct PlanEntry {
    pub target: Path,
    pub edits: Vec<AttrEdit>,
}

/// An attribute edit plan.
///
/// Build a plan with [`Plan::add`] and [`Plan::remove`], then apply it with
/// [`Plan::apply`].
#[derive(Default)]
pub struct Plan {
    pub(crate) entries: Vec<PlanEntry>,
}

impl Plan {
    /// Create an empty plan.
    pub fn new() -> Plan {
        Plan::default()
    }

    /// Add an outer attribute to the item at `target`.
    pub fn add(&mut self, target: Path, spec: AttrSpec) -> &mut Plan {
        self.entries.push(PlanEntry {
            target,
            edits: vec![AttrEdit::Add(spec)],
        });
        self
    }

    /// Remove the outer attribute with this path from the item at `target`.
    pub fn remove(&mut self, target: Path, spec: AttrSpec) -> &mut Plan {
        self.entries.push(PlanEntry {
            target,
            edits: vec![AttrEdit::Remove(spec)],
        });
        self
    }

    /// Add an entry with multiple edits.
    pub fn entry(&mut self, target: Path, edits: Vec<AttrEdit>) -> &mut Plan {
        self.entries.push(PlanEntry { target, edits });
        self
    }

    /// Apply the plan to a parsed [`File`].
    ///
    /// On success, returns a new [`File`] with the edits applied (the
    /// original is untouched) plus a span mapping for every edit.
    ///
    /// On failure (duplicate target, ambiguous path, missing attribute,
    /// add/remove conflict), returns an error and the original file is
    /// unchanged.
    pub fn apply(&self, file: &File) -> Result<EditResult> {
        // 1. Validate: duplicate targets.
        self.check_duplicate_targets()?;

        // 2. Validate: resolve each target path (ambiguity / not found).
        for entry in &self.entries {
            let item = resolve_path(&file.items, &entry.target)?;
            // 3. Validate: add/remove conflicts on the resolved item.
            check_edit_conflicts(item, entry)?;
        }

        // 4. All validation passed - apply edits to a clone.
        let mut new_file = file.clone();
        let mut mappings = Vec::new();
        for entry in &self.entries {
            let segments: Vec<String> = entry
                .target
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let full_path = path_key(&entry.target);
            apply_to_items(
                &mut new_file.items,
                &segments,
                &full_path,
                &entry.edits,
                &mut mappings,
            );
        }

        Ok(EditResult {
            file: new_file,
            mappings,
        })
    }

    fn check_duplicate_targets(&self) -> Result<()> {
        let mut seen: HashMap<String, Span> = HashMap::new();
        for entry in &self.entries {
            let key = path_key(&entry.target);
            if let Some(prev) = seen.get(&key) {
                return Err(syn::Error::new(
                    entry.target.segments.first().unwrap().ident.span(),
                    format!(
                        "duplicate target `{}` (first used at span {:?})",
                        key, prev,
                    ),
                ));
            }
            seen.insert(key, entry.target.segments.first().unwrap().ident.span());
        }
        Ok(())
    }
}

/// The result of applying a plan: a new [`File`] and a span mapping.
#[derive(Debug)]
pub struct EditResult {
    pub file: File,
    pub mappings: Vec<AttrSpanMapping>,
}

/// A span mapping for a single edit.
///
/// For an add: `old_span` is `None`, `new_span` is the span of the added
/// attribute in the new AST.
///
/// For a remove: `old_span` is the span of the removed attribute in the
/// original source, `new_span` is `None`.
#[derive(Clone, Debug)]
pub struct AttrSpanMapping {
    pub target: String,
    pub kind: AttrEditKind,
    pub old_span: Option<Span>,
    pub new_span: Option<Span>,
}

// ---------- path resolution ----------

fn path_key(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Compare two paths by their segments (syn::Path does not implement
/// PartialEq).
fn paths_equal(a: &Path, b: &Path) -> bool {
    a.segments.len() == b.segments.len()
        && a.segments
            .iter()
            .zip(b.segments.iter())
            .all(|(x, y)| x.ident == y.ident)
}

fn item_ident(item: &Item) -> Option<&Ident> {
    match item {
        Item::Const(i) => Some(&i.ident),
        Item::Enum(i) => Some(&i.ident),
        Item::ExternCrate(i) => Some(&i.ident),
        Item::Fn(i) => Some(&i.sig.ident),
        Item::Macro(i) => i.ident.as_ref(),
        Item::Mod(i) => Some(&i.ident),
        Item::Static(i) => Some(&i.ident),
        Item::Struct(i) => Some(&i.ident),
        Item::Trait(i) => Some(&i.ident),
        Item::TraitAlias(i) => Some(&i.ident),
        Item::Type(i) => Some(&i.ident),
        Item::Union(i) => Some(&i.ident),
        _ => None,
    }
}

/// Resolve an explicit path to exactly one item, or fail with an ambiguity /
/// not-found error.
fn resolve_path<'a>(items: &'a [Item], path: &Path) -> Result<&'a Item> {
    let segments: Vec<&Ident> = path.segments.iter().map(|s| &s.ident).collect();
    if segments.is_empty() {
        return Err(syn::Error::new(path.span(), "empty target path"));
    }
    let mut current_items = items;
    let mut current_item: Option<&Item> = None;
    for (i, seg) in segments.iter().enumerate() {
        let matches: Vec<&Item> = current_items
            .iter()
            .filter(|item| item_ident(item) == Some(*seg))
            .collect();
        if matches.is_empty() {
            return Err(syn::Error::new(
                seg.span(),
                format!("no item named `{}` in this scope", seg),
            ));
        }
        if matches.len() > 1 {
            return Err(syn::Error::new(
                seg.span(),
                format!("ambiguous item `{}`: found {} matches", seg, matches.len()),
            ));
        }
        let item = matches[0];
        if i == segments.len() - 1 {
            current_item = Some(item);
        } else {
            match item {
                Item::Mod(m) => {
                    current_items = m
                        .content
                        .as_ref()
                        .map(|(_, items)| items.as_slice())
                        .unwrap_or(&[]);
                }
                _ => {
                    return Err(syn::Error::new(
                        seg.span(),
                        format!("`{}` is not a module", seg),
                    ));
                }
            }
        }
    }
    Ok(current_item.unwrap())
}

// ---------- edit conflict checks ----------

fn check_edit_conflicts(item: &Item, entry: &PlanEntry) -> Result<()> {
    // Reject add+remove of the same path within one entry first.
    let mut seen: HashMap<String, AttrEditKind> = HashMap::new();
    for edit in &entry.edits {
        let key = path_key(edit.spec().path());
        if let Some(prev) = seen.get(&key) {
            if *prev != edit.kind() {
                return Err(syn::Error::new(
                    edit.spec().path().span(),
                    format!("conflicting edits on `{}`", key),
                ));
            }
        }
        seen.insert(key, edit.kind());
    }
    // Then check against the item's existing attributes.
    let attrs = item_attrs(item);
    for edit in &entry.edits {
        let spec_path = edit.spec().path();
        let exists = attrs
            .iter()
            .any(|a| matches!(a.style, AttrStyle::Outer) && paths_equal(a.path(), spec_path));
        match edit {
            AttrEdit::Add(_) if exists => {
                return Err(syn::Error::new(
                    spec_path.span(),
                    format!(
                        "cannot add `{}`: a direct outer attribute with this path already exists",
                        path_key(spec_path)
                    ),
                ));
            }
            AttrEdit::Remove(_) if !exists => {
                return Err(syn::Error::new(
                    spec_path.span(),
                    format!(
                        "cannot remove `{}`: no direct outer attribute with this path",
                        path_key(spec_path)
                    ),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn item_attrs_mut(item: &mut Item) -> &mut Vec<Attribute> {
    match item {
        Item::Const(i) => &mut i.attrs,
        Item::Enum(i) => &mut i.attrs,
        Item::ExternCrate(i) => &mut i.attrs,
        Item::Fn(i) => &mut i.attrs,
        Item::ForeignMod(i) => &mut i.attrs,
        Item::Impl(i) => &mut i.attrs,
        Item::Macro(i) => &mut i.attrs,
        Item::Mod(i) => &mut i.attrs,
        Item::Static(i) => &mut i.attrs,
        Item::Struct(i) => &mut i.attrs,
        Item::Trait(i) => &mut i.attrs,
        Item::TraitAlias(i) => &mut i.attrs,
        Item::Type(i) => &mut i.attrs,
        Item::Union(i) => &mut i.attrs,
        Item::Use(i) => &mut i.attrs,
        _ => panic!("attr-plan: cannot edit attributes of this item"),
    }
}

// ---------- application ----------

/// Apply edits to the item at `path` within `items`, recursing into modules.
fn apply_to_items(
    items: &mut Vec<Item>,
    path: &[String],
    full_path: &str,
    edits: &[AttrEdit],
    mappings: &mut Vec<AttrSpanMapping>,
) {
    let target = &path[0];
    let idx = items
        .iter()
        .position(|item| item_ident(item).map(|i| i.to_string()) == Some(target.clone()))
        .expect("attr-plan: validated target path");
    let item = &mut items[idx];
    if path.len() == 1 {
        apply_edits_to_item(item, full_path, edits, mappings);
    } else {
        match item {
            Item::Mod(m) => {
                let sub = m
                    .content
                    .as_mut()
                    .expect("attr-plan: validated module content")
                    .1
                    .as_mut();
                apply_to_items(sub, &path[1..], full_path, edits, mappings);
            }
            _ => panic!("attr-plan: validated module"),
        }
    }
}

fn apply_edits_to_item(
    item: &mut Item,
    target: &str,
    edits: &[AttrEdit],
    mappings: &mut Vec<AttrSpanMapping>,
) {
    for edit in edits {
        match edit {
            AttrEdit::Add(spec) => {
                let attr = spec.to_attribute();
                let new_span = attr.path().span();
                item_attrs_mut(item).push(attr);
                mappings.push(AttrSpanMapping {
                    target: target.to_string(),
                    kind: AttrEditKind::Add,
                    old_span: None,
                    new_span: Some(new_span),
                });
            }
            AttrEdit::Remove(spec) => {
                let attrs = item_attrs_mut(item);
                let idx = attrs
                    .iter()
                    .position(|a| {
                        matches!(a.style, AttrStyle::Outer) && paths_equal(a.path(), spec.path())
                    })
                    .expect("attr-plan: validated remove target");
                let removed = attrs.remove(idx);
                let old_span = removed.path().span();
                mappings.push(AttrSpanMapping {
                    target: target.to_string(),
                    kind: AttrEditKind::Remove,
                    old_span: Some(old_span),
                    new_span: None,
                });
            }
        }
    }
}
