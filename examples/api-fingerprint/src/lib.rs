//! Compile-free public interface fingerprinting on top of `syn`.
//!
//! The input is a single parsed [`syn::File`] (or a source string, parsed with
//! [`syn::parse_file`]); no `rustc`, no name resolution and no macro expansion
//! are involved. The output is a [`Report`] of [`Entry`] records, one per
//! exported item, each carrying:
//!
//! - a **normalized signature** in which whitespace, comments, doc comments
//!   and function bodies have been erased, while generic constraints,
//!   visibility, field order, enum discriminants, ABI, `#[repr]` and other
//!   non-doc attributes are preserved;
//! - a **SHA-256 digest** of that normalized signature;
//! - a **source location** (1-based line/column) so a diff can point back at
//!   the original code.
//!
//! # What is considered exported
//!
//! Only items reachable through fully `pub` module chains from the crate root
//! are reported. Items inside private or `pub(crate)` modules are not exports
//! and never enter the report. `pub use` re-exports are recorded textually
//! (the external crate is never consulted); a re-export whose path cannot be
//! resolved locally (an external crate path or a glob) is kept with its
//! textual path *and* flagged [`Status::Unknown`].
//!
//! Macro invocations in item position and `pub mod foo;` declarations whose
//! file is not part of the input are unresolvable export boundaries: they are
//! recorded as `unknown` so that a diff can never report them as definitely
//! compatible.
//!
//! # Normalization rules
//!
//! - Whitespace, comments and `#[doc]` attributes are dropped; every token
//!   stream is re-rendered in a canonical spacing.
//! - Function bodies are dropped (only the signature is kept). Default
//!   bodies of trait methods and default values of trait consts are dropped
//!   the same way.
//! - Lifetime parameters explicitly bound by the item's own generics are
//!   alpha-renamed to `'l0`, `'l1`, ... in declaration order. Two
//!   independent lifetimes therefore keep distinct names, while a pure
//!   rename (`'a` -> `'b`) is invisible. Unbound lifetimes such as `'static`
//!   (and `for<'x>` binders inside types) are left untouched.
//! - Raw identifiers lose their `r#` prefix (`r#type` normalizes to `type`).
//! - Generic parameter defaults (`<T = u32>`) are preserved.
//! - All non-doc attributes (including `#[repr(...)]`) are preserved.

mod sha256;

pub use sha256::{sha256, sha256_hex};

use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use quote::ToTokens;
use std::collections::BTreeMap;
use std::fmt;
use syn::spanned::Spanned;
use syn::{Attribute, Generics, Ident, Item, UseTree, Visibility};

/// The kind of an exported item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Fn,
    /// A `pub fn` inside an inherent (`impl Type { ... }`) block.
    Method,
    Struct,
    Enum,
    Trait,
    /// One exported name of a `pub use` tree.
    Use,
    /// A `pub mod name;` declaration whose contents are not in the input.
    Mod,
    /// A macro invocation in item position of a public module.
    Macro,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fn => "fn",
            Kind::Method => "method",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Trait => "trait",
            Kind::Use => "use",
            Kind::Mod => "mod",
            Kind::Macro => "macro",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an export boundary could not be resolved statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// A macro invocation in item position: the generated interface cannot
    /// be seen without expansion.
    ItemMacro,
    /// `pub mod name;` — the module file is not part of the input AST.
    UnloadedModule,
    /// `pub use` of a path that does not start with `self`/`super`/`crate`:
    /// it points outside the input and cannot be expanded.
    ExternalUse,
    /// `pub use ...::*` — the exported set cannot be enumerated statically.
    GlobUse,
}

impl UnknownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnknownReason::ItemMacro => "item macro cannot be expanded",
            UnknownReason::UnloadedModule => "module file not present in input",
            UnknownReason::ExternalUse => "re-export of an external path cannot be expanded",
            UnknownReason::GlobUse => "glob re-export cannot be enumerated",
        }
    }
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an entry's interface is fully determined by the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Known,
    Unknown(UnknownReason),
}

impl Status {
    pub fn is_known(self) -> bool {
        matches!(self, Status::Known)
    }
}

/// One exported item (or unresolvable export boundary) of the input file.
#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: Kind,
    /// Module-qualified path, e.g. `outer::inner::greet`.
    pub path: String,
    /// Normalized signature (see crate docs). For `use`/`mod`/`macro`
    /// entries this is the normalized textual form of the item.
    pub signature: String,
    /// Hex SHA-256 of `kind \0 path \0 signature`.
    pub digest: String,
    pub status: Status,
    /// 1-based line and column of the item in the source, for diff output.
    pub line: usize,
    pub column: usize,
}

impl Entry {
    pub fn is_unknown(&self) -> bool {
        !self.status.is_known()
    }
}

/// The fingerprint of one parsed file.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub entries: Vec<Entry>,
}

impl Report {
    pub fn known(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.status.is_known())
    }

    pub fn unknown(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_unknown())
    }

    pub fn find(&self, kind: Kind, path: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.kind == kind && e.path == path)
    }
}

/// Parse `source` as a Rust file and fingerprint its public interface.
pub fn fingerprint_source(source: &str) -> Result<Report, syn::Error> {
    let file = syn::parse_file(source)?;
    Ok(fingerprint_file(&file))
}

/// Fingerprint the public interface of an already-parsed file.
pub fn fingerprint_file(file: &syn::File) -> Report {
    let mut collector = Collector { entries: Vec::new() };
    collector.collect_items(&file.items, "", true);
    collector
        .entries
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.kind.cmp(&b.kind)));
    Report {
        entries: collector.entries,
    }
}

// ---------------------------------------------------------------------------
// Traversal
// ---------------------------------------------------------------------------

struct Collector {
    entries: Vec<Entry>,
}

impl Collector {
    fn collect_items(&mut self, items: &[Item], module: &str, public: bool) {
        let mut macro_index = 0usize;
        for item in items {
            match item {
                Item::Fn(it) if public && is_pub(&it.vis) => self.record_fn(module, it),
                Item::Struct(it) if public && is_pub(&it.vis) => {
                    self.record_whole(module, Kind::Struct, &it.ident, &it.generics, it)
                }
                Item::Enum(it) if public && is_pub(&it.vis) => {
                    self.record_whole(module, Kind::Enum, &it.ident, &it.generics, it)
                }
                Item::Trait(it) if public && is_pub(&it.vis) => self.record_trait(module, it),
                Item::Use(it) if public && is_pub(&it.vis) => self.record_use(module, it),
                Item::Mod(it) => self.record_mod(module, it, public),
                Item::Macro(it) if public => {
                    self.record_macro(module, it, macro_index);
                    macro_index += 1;
                }
                Item::Impl(it) if public => self.record_impl(module, it),
                _ => {}
            }
        }
    }

    fn push(
        &mut self,
        module: &str,
        kind: Kind,
        name: &str,
        signature: String,
        status: Status,
        span: proc_macro2::Span,
    ) {
        let path = join_path(module, name);
        let digest = digest_of(kind, &path, &signature);
        let start = span.start();
        self.entries.push(Entry {
            kind,
            path,
            signature,
            digest,
            status,
            line: start.line,
            column: start.column + 1,
        });
    }

    fn record_fn(&mut self, module: &str, it: &syn::ItemFn) {
        let mut scope = Scope::default();
        scope.bind_generics(&it.sig.generics);
        let mut ts = TokenStream::new();
        push_attrs(&mut ts, &it.attrs);
        it.vis.to_tokens(&mut ts);
        it.sig.to_tokens(&mut ts);
        let signature = render(ts, &scope);
        let name = ident_string(&it.sig.ident);
        self.push(module, Kind::Fn, &name, signature, Status::Known, it.span());
    }

    /// Structs and enums: the whole item (attributes, visibility, generics,
    /// where-clause, fields/variants in order, discriminants) is normalized
    /// as one token stream; only doc attributes are stripped.
    fn record_whole(
        &mut self,
        module: &str,
        kind: Kind,
        ident: &Ident,
        generics: &Generics,
        item: &impl ToTokens,
    ) {
        let mut scope = Scope::default();
        scope.bind_generics(generics);
        let ts = strip_doc_tokens(item.to_token_stream());
        let signature = render(ts, &scope);
        let name = ident_string(ident);
        self.push(module, kind, &name, signature, Status::Known, item.span());
    }

    fn record_trait(&mut self, module: &str, it: &syn::ItemTrait) {
        let mut scope = Scope::default();
        scope.bind_generics(&it.generics);

        let mut head = TokenStream::new();
        push_attrs(&mut head, &it.attrs);
        it.vis.to_tokens(&mut head);
        it.unsafety.to_tokens(&mut head);
        it.trait_token.to_tokens(&mut head);
        it.ident.to_tokens(&mut head);
        it.generics.to_tokens(&mut head);
        it.colon_token.to_tokens(&mut head);
        it.supertraits.to_tokens(&mut head);
        it.generics.where_clause.to_tokens(&mut head);

        let mut signature = render(head, &scope);
        signature.push_str(" {");
        for (i, ti) in it.items.iter().enumerate() {
            match ti {
                syn::TraitItem::Fn(m) => {
                    // Default method bodies are function bodies: dropped.
                    let mut sc = scope.clone();
                    sc.bind_generics(&m.sig.generics);
                    let mut ts = TokenStream::new();
                    push_attrs(&mut ts, &m.attrs);
                    m.sig.to_tokens(&mut ts);
                    signature.push_str(&render(ts, &sc));
                    signature.push_str(";");
                }
                syn::TraitItem::Const(c) => {
                    // The default value of an associated const is treated
                    // like a function body: dropped. Its type is kept.
                    let mut ts = TokenStream::new();
                    push_attrs(&mut ts, &c.attrs);
                    c.const_token.to_tokens(&mut ts);
                    c.ident.to_tokens(&mut ts);
                    c.colon_token.to_tokens(&mut ts);
                    c.ty.to_tokens(&mut ts);
                    signature.push_str(&render(ts, &scope));
                    signature.push_str(";");
                }
                syn::TraitItem::Type(t) => {
                    let mut sc = scope.clone();
                    sc.bind_generics(&t.generics);
                    let ts = strip_doc_tokens(t.to_token_stream());
                    signature.push_str(&render(ts, &sc));
                }
                syn::TraitItem::Macro(m) => {
                    let ts = strip_doc_tokens(m.to_token_stream());
                    signature.push_str(&render(ts, &scope));
                    let path = join_path(module, &ident_string(&it.ident));
                    let name = format!("{}::<macro #{}>", path, i);
                    let mac_sig = render(strip_doc_tokens(m.to_token_stream()), &scope);
                    self.push(
                        "",
                        Kind::Macro,
                        &name,
                        mac_sig,
                        Status::Unknown(UnknownReason::ItemMacro),
                        m.span(),
                    );
                }
                other => {
                    let ts = strip_doc_tokens(other.to_token_stream());
                    signature.push_str(&render(ts, &scope));
                }
            }
        }
        signature.push_str("}");

        let name = ident_string(&it.ident);
        self.push(module, Kind::Trait, &name, signature, Status::Known, it.span());
    }

    fn record_use(&mut self, module: &str, it: &syn::ItemUse) {
        let mut ts = TokenStream::new();
        push_attrs(&mut ts, &it.attrs);
        it.vis.to_tokens(&mut ts);
        it.use_token.to_tokens(&mut ts);
        it.leading_colon.to_tokens(&mut ts);
        it.tree.to_tokens(&mut ts);
        it.semi_token.to_tokens(&mut ts);
        let signature = render(ts, &Scope::default());

        let mut leaves = Vec::new();
        flatten_use(&it.tree, Vec::new(), &mut leaves);
        for leaf in leaves {
            let status = if leaf.has_glob {
                Status::Unknown(UnknownReason::GlobUse)
            } else if leaf.external {
                Status::Unknown(UnknownReason::ExternalUse)
            } else {
                Status::Known
            };
            self.push(
                module,
                Kind::Use,
                &leaf.exported,
                signature.clone(),
                status,
                it.span(),
            );
        }
    }

    fn record_mod(&mut self, module: &str, it: &syn::ItemMod, public: bool) {
        let name = ident_string(&it.ident);
        let reachable = public && is_pub(&it.vis);
        match &it.content {
            Some((_, items)) => {
                self.collect_items(items, &join_path(module, &name), reachable);
            }
            None => {
                if reachable {
                    // `pub mod name;` — the file holding this module is not
                    // part of the input, so its exports are unknown.
                    let mut ts = TokenStream::new();
                    push_attrs(&mut ts, &it.attrs);
                    it.vis.to_tokens(&mut ts);
                    it.mod_token.to_tokens(&mut ts);
                    it.ident.to_tokens(&mut ts);
                    it.semi.to_tokens(&mut ts);
                    let signature = render(ts, &Scope::default());
                    self.push(
                        module,
                        Kind::Mod,
                        &name,
                        signature,
                        Status::Unknown(UnknownReason::UnloadedModule),
                        it.span(),
                    );
                }
            }
        }
    }

    fn record_macro(&mut self, module: &str, it: &syn::ItemMacro, index: usize) {
        let mut ts = TokenStream::new();
        push_attrs(&mut ts, &it.attrs);
        it.mac.to_tokens(&mut ts);
        it.semi_token.to_tokens(&mut ts);
        let signature = render(ts, &Scope::default());
        let name = format!("<macro #{}>", index);
        self.push(
            module,
            Kind::Macro,
            &name,
            signature,
            Status::Unknown(UnknownReason::ItemMacro),
            it.span(),
        );
    }

    fn record_impl(&mut self, module: &str, it: &syn::ItemImpl) {
        // Only inherent impls introduce new public names; trait impls are
        // covered by the trait's own entry.
        if it.trait_.is_some() {
            return;
        }
        let mut base = Scope::default();
        base.bind_generics(&it.generics);
        let self_name = render(strip_doc_tokens(it.self_ty.to_token_stream()), &base);
        for item in &it.items {
            if let syn::ImplItem::Fn(m) = item {
                if is_pub(&m.vis) {
                    let mut scope = base.clone();
                    scope.bind_generics(&m.sig.generics);
                    let mut ts = TokenStream::new();
                    push_attrs(&mut ts, &m.attrs);
                    m.vis.to_tokens(&mut ts);
                    m.sig.to_tokens(&mut ts);
                    let signature = render(ts, &scope);
                    let name = format!("{}::{}", self_name, ident_string(&m.sig.ident));
                    self.push(module, Kind::Method, &name, signature, Status::Known, m.span());
                }
            }
        }
    }
}

fn is_pub(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn join_path(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_owned()
    } else {
        format!("{}::{}", module, name)
    }
}

fn digest_of(kind: Kind, path: &str, signature: &str) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(kind.as_str().as_bytes());
    data.push(0);
    data.extend_from_slice(path.as_bytes());
    data.push(0);
    data.extend_from_slice(signature.as_bytes());
    sha256_hex(&data)
}

/// Identifier text without a raw `r#` prefix.
fn ident_string(ident: &Ident) -> String {
    let s = ident.to_string();
    match s.strip_prefix("r#") {
        Some(stripped) => stripped.to_owned(),
        None => s,
    }
}

// ---------------------------------------------------------------------------
// `pub use` flattening
// ---------------------------------------------------------------------------

struct UseLeaf {
    /// The name under which the item is exported.
    exported: String,
    has_glob: bool,
    external: bool,
}

fn flatten_use(tree: &UseTree, prefix: Vec<String>, out: &mut Vec<UseLeaf>) {
    match tree {
        UseTree::Path(p) => {
            let mut prefix = prefix;
            prefix.push(ident_string(&p.ident));
            flatten_use(&p.tree, prefix, out);
        }
        UseTree::Name(n) => {
            let external = is_external(&prefix, &ident_string(&n.ident));
            out.push(UseLeaf {
                exported: ident_string(&n.ident),
                has_glob: false,
                external,
            });
        }
        UseTree::Rename(r) => {
            let external = is_external(&prefix, &ident_string(&r.ident));
            out.push(UseLeaf {
                exported: ident_string(&r.rename),
                has_glob: false,
                external,
            });
        }
        UseTree::Glob(_) => {
            let external = match prefix.first() {
                Some(first) => !is_local_root(first),
                None => false,
            };
            out.push(UseLeaf {
                exported: "*".to_owned(),
                has_glob: true,
                external,
            });
        }
        UseTree::Group(g) => {
            for t in &g.items {
                flatten_use(t, prefix.clone(), out);
            }
        }
    }
}

fn is_local_root(segment: &str) -> bool {
    matches!(segment, "self" | "super" | "crate")
}

fn is_external(prefix: &[String], last: &str) -> bool {
    let first = prefix.first().map(String::as_str).unwrap_or(last);
    !is_local_root(first)
}

// ---------------------------------------------------------------------------
// Lifetime alpha-renaming scope
// ---------------------------------------------------------------------------

/// Maps explicitly bound lifetime parameter names to canonical names
/// `'l0`, `'l1`, ... in declaration order. Unbound lifetimes (e.g.
/// `'static`) are not in the map and are rendered unchanged, so two
/// independent lifetimes are never merged.
#[derive(Default, Clone)]
struct Scope {
    map: Vec<(String, String)>,
}

impl Scope {
    fn bind_generics(&mut self, generics: &Generics) {
        for param in generics.lifetimes() {
            let name = param.lifetime.ident.to_string();
            if !self.map.iter().any(|(k, _)| *k == name) {
                let canonical = format!("l{}", self.map.len());
                self.map.push((name, canonical));
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<&str> {
        self.map
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

// ---------------------------------------------------------------------------
// Token-level normalization
// ---------------------------------------------------------------------------

/// Drop `#[doc = ...]` attributes (doc comments) from a token stream,
/// recursively. All other attributes — notably `#[repr(...)]` — are kept.
fn strip_doc_tokens(ts: TokenStream) -> TokenStream {
    let mut out = TokenStream::new();
    let mut iter = ts.into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                let is_doc = match iter.peek() {
                    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Bracket => g
                        .stream()
                        .into_iter()
                        .next()
                        .map_or(false, |t| matches!(t, TokenTree::Ident(id) if id == "doc")),
                    _ => false,
                };
                if is_doc {
                    iter.next(); // drop the bracket group
                } else {
                    out.extend([TokenTree::Punct(p)]);
                }
            }
            TokenTree::Group(g) => {
                let inner = strip_doc_tokens(g.stream());
                out.extend([TokenTree::Group(Group::new(g.delimiter(), inner))]);
            }
            other => out.extend([other]),
        }
    }
    out
}

/// Copy non-doc attributes into a token stream.
fn push_attrs(ts: &mut TokenStream, attrs: &[Attribute]) {
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            attr.to_tokens(ts);
        }
    }
}

/// Render a token stream in canonical spacing, applying lifetime
/// alpha-renaming and raw-identifier normalization.
fn render(ts: TokenStream, scope: &Scope) -> String {
    let mut out = String::new();
    render_into(ts, scope, &mut out);
    out
}

fn render_into(ts: TokenStream, scope: &Scope, out: &mut String) {
    let mut iter = ts.into_iter();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Ident(id) => {
                push_word(out, &ident_string(&id));
            }
            TokenTree::Literal(lit) => {
                push_word(out, &lit.to_string());
            }
            TokenTree::Punct(p) => {
                let ch = p.as_char();
                if ch == '\'' {
                    // Lifetime: the following token is its name.
                    out.push('\'');
                    if let Some(TokenTree::Ident(id)) = iter.next() {
                        let name = id.to_string();
                        match scope.lookup(&name) {
                            Some(canonical) => out.push_str(canonical),
                            None => out.push_str(&name),
                        }
                    }
                } else {
                    out.push(ch);
                }
            }
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                render_into(g.stream(), scope, out);
                out.push_str(close);
            }
        }
    }
}

/// Push an identifier/literal, inserting a canonical single space only when
/// the previous character could otherwise merge with it.
fn push_word(out: &mut String, word: &str) {
    let needs_space = match out.chars().last() {
        None => false,
        Some(c) => c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '}' | '>'),
    };
    if needs_space {
        out.push(' ');
    }
    out.push_str(word);
}

// ---------------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, Copy)]
pub struct Change<'a> {
    pub kind: ChangeKind,
    pub old: Option<&'a Entry>,
    pub new: Option<&'a Entry>,
}

impl<'a> Change<'a> {
    pub fn entry(&self) -> &'a Entry {
        self.new.or(self.old).unwrap()
    }
}

/// The difference between two interface reports.
#[derive(Debug, Default)]
pub struct Diff<'a> {
    pub changes: Vec<Change<'a>>,
    /// Unknown-status entries from both reports. While any of these exist,
    /// the two interfaces cannot be certified as compatible.
    pub unknowns: Vec<&'a Entry>,
}

impl<'a> Diff<'a> {
    /// No known changes and no unknown boundaries.
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && self.unknowns.is_empty()
    }
}

/// Diff two reports, matching entries by `(kind, path)`.
pub fn diff<'a>(old: &'a Report, new: &'a Report) -> Diff<'a> {
    fn index(report: &Report) -> BTreeMap<(&str, Kind), &Entry> {
        report
            .entries
            .iter()
            .map(|e| ((e.path.as_str(), e.kind), e))
            .collect()
    }
    let old_map = index(old);
    let new_map = index(new);

    let mut changes = Vec::new();
    for e in &new.entries {
        match old_map.get(&(e.path.as_str(), e.kind)) {
            None => changes.push(Change {
                kind: ChangeKind::Added,
                old: None,
                new: Some(e),
            }),
            Some(o) if o.digest != e.digest => changes.push(Change {
                kind: ChangeKind::Changed,
                old: Some(o),
                new: Some(e),
            }),
            Some(_) => {}
        }
    }
    for e in &old.entries {
        if !new_map.contains_key(&(e.path.as_str(), e.kind)) {
            changes.push(Change {
                kind: ChangeKind::Removed,
                old: Some(e),
                new: None,
            });
        }
    }
    changes.sort_by(|a, b| {
        let (ea, eb) = (a.entry(), b.entry());
        ea.path
            .cmp(&eb.path)
            .then(ea.kind.cmp(&eb.kind))
            .then((a.kind as u8).cmp(&(b.kind as u8)))
    });

    let unknowns = old
        .entries
        .iter()
        .chain(new.entries.iter())
        .filter(|e| e.is_unknown())
        .collect();

    Diff { changes, unknowns }
}
