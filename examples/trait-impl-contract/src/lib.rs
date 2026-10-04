//! Static contract checking between a `trait` definition and one of its
//! `impl` blocks, built purely on [`syn`] syntax trees.
//!
//! The checker never invokes `rustc` and never mutates the input AST: both
//! sides are borrowed immutably and every diagnostic carries the spans of the
//! relevant items on the trait side *and* the impl side.
//!
//! # What is checked
//!
//! Every associated item required by the trait — a method without a default
//! body, an associated type without a default, an associated const without a
//! default value — must appear in the impl under the same name and the same
//! item kind. Items that have a default are *not* required, so a missing
//! default method is never reported.
//!
//! For items present on both sides, signatures are compared. Generic
//! parameters (type, const and lifetime) are matched **positionally**, so
//! renaming a parameter is not a difference: a trait method `f<T>(T)`
//! implemented as `f<U>(U)` is isomorphic and accepted.
//!
//! # Statically decidable scope
//!
//! The following are reported as **mismatches** (proven incompatible from
//! syntax alone):
//!
//! - receiver shape: `self` / `&self` / `&mut self` /
//!   `self: Box<Self>` categories, including `&self` vs `&mut self`;
//! - `const`, `async`, `safe` / `unsafe` qualifiers;
//! - the ABI string (`extern "C"` vs `extern "system"` vs the
//!   default `"Rust"` ABI);
//! - generic parameter arity (number of type or const parameters);
//! - argument count, variadic `...`;
//! - structural type differences in which no type alias could be involved,
//!   e.g. `i32` vs `u32`, `Vec<T>` vs `Option<T>`,
//!   `&T` vs `T`, tuples of different arity.
//!
//! The following are reported as **unknown** (cannot be decided without name
//! resolution, alias expansion or trait solving — *not* incompatibilities):
//!
//! - any type difference in which a bare non-generic identifier appears where
//!   the two sides differ (it might be a type alias);
//! - `Self` written on one side and the concrete type on the other;
//! - syntactically different `where` clauses or generic bounds (they may
//!   be semantically equivalent, e.g. bound order);
//! - array length expressions and const generic arguments;
//! - lifetime parameter arity differences;
//! - associated type bounds (`type A: Clone;`) — whether the impl's
//!   concrete type satisfies them needs trait solving;
//! - qualified paths (`<T as Tr>::Assoc`), macro-generated items.
//!
//! Unknowns are reported separately from proven mismatches, so callers can
//! decide how conservatively to treat them.
//!
//! # Example 1: isomorphic implementation (generic rename + default method)
//!
//! ```
//! use syn::{parse_quote, ItemImpl, ItemTrait};
//! use trait_impl_contract::check_trait_impl;
//!
//! let trait_def: ItemTrait = parse_quote! {
//!     trait Convert {
//!         fn f<T>(&self, x: T) -> T;
//!         fn defaulted(&self) -> u32 { 0 } // has a default: not required
//!     }
//! };
//! let impl_block: ItemImpl = parse_quote! {
//!     impl Convert for S {
//!         fn f<U>(&self, x: U) -> U { x } // renamed T -> U: still isomorphic
//!     }
//! };
//!
//! let report = check_trait_impl(&trait_def, &impl_block);
//! assert!(report.missing.is_empty());      // `defaulted` is not required
//! assert!(report.mismatches.is_empty());   // f<T>(T) == f<U>(U)
//! assert!(report.unknowns.is_empty());
//! ```
//!
//! # Example 2: receiver difference is a proven mismatch
//!
//! ```
//! use syn::{parse_quote, ItemImpl, ItemTrait};
//! use trait_impl_contract::{check_trait_impl, Difference};
//!
//! let trait_def: ItemTrait = parse_quote! {
//!     trait Counter {
//!         fn bump(&self);
//!     }
//! };
//! let impl_block: ItemImpl = parse_quote! {
//!     impl Counter for C {
//!         fn bump(&mut self) {}
//!     }
//! };
//!
//! let report = check_trait_impl(&trait_def, &impl_block);
//! assert_eq!(report.mismatches.len(), 1);
//! assert!(matches!(
//!     report.mismatches[0].differences[0],
//!     Difference::Receiver { .. }
//! ));
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{
    Abi, FnArg, GenericArgument, Generics, ImplItem, ImplItemConst, ImplItemFn, ImplItemType,
    ItemImpl, ItemTrait, PatType, PathArguments, Receiver, ReceiverKind, ReturnType, Safety,
    Signature, TraitItem, TraitItemConst, TraitItemType, Type, TypePath,
};

/// Entry point: check that `impl_block` satisfies the contract of
/// `trait_def`.
///
/// Both syntax trees are only borrowed; nothing is mutated. The caller is
/// expected to pass a trait and an impl of that same trait (the impl header
/// path is used to map the trait's own generic parameters, but a name
/// mismatch between `trait_def` and the impl's trait path is not
/// verified).
pub fn check_trait_impl(trait_def: &ItemTrait, impl_block: &ItemImpl) -> ContractReport {
    let mut report = ContractReport::default();
    let outer = outer_param_map(trait_def, impl_block);
    let impl_span = impl_block.impl_token.span();

    // Index the impl's associated items by name.
    let mut fns: HashMap<String, &ImplItemFn> = HashMap::new();
    let mut types: HashMap<String, &ImplItemType> = HashMap::new();
    let mut consts: HashMap<String, &ImplItemConst> = HashMap::new();
    for item in &impl_block.items {
        match item {
            ImplItem::Fn(f) => {
                fns.entry(f.sig.ident.to_string()).or_insert(f);
            }
            ImplItem::Type(t) => {
                types.entry(t.ident.to_string()).or_insert(t);
            }
            ImplItem::Const(c) => {
                consts.entry(c.ident.to_string()).or_insert(c);
            }
            _ => {}
        }
    }

    for item in &trait_def.items {
        match item {
            TraitItem::Fn(f) => {
                // A default body means the method is not required.
                if f.default.is_some() {
                    continue;
                }
                let name = f.sig.ident.to_string();
                let trait_span = f.sig.ident.span();
                if let Some(impl_fn) = fns.get(&name) {
                    let cmp = compare_fn_sigs(&f.sig, &impl_fn.sig, &outer);
                    report.mismatches.extend(cmp.mismatch(
                        ItemKind::Method,
                        &name,
                        trait_span,
                        impl_fn.sig.ident.span(),
                    ));
                    report.unknowns.extend(cmp.unknown(
                        ItemKind::Method,
                        &name,
                        trait_span,
                        impl_fn.sig.ident.span(),
                    ));
                } else if types.contains_key(&name) || consts.contains_key(&name) {
                    report.mismatches.push(SignatureMismatch {
                        kind: ItemKind::Method,
                        name,
                        differences: vec![Difference::ItemKind {
                            expected: "method",
                            found: "associated type or const",
                        }],
                        trait_span,
                        impl_span,
                    });
                } else {
                    report.missing.push(MissingItem {
                        kind: ItemKind::Method,
                        name,
                        trait_span,
                        impl_span,
                    });
                }
            }
            TraitItem::Type(t) => {
                // A defaulted associated type is not required.
                if t.default.is_some() {
                    continue;
                }
                let name = t.ident.to_string();
                let trait_span = t.ident.span();
                if let Some(impl_ty) = types.get(&name) {
                    let cmp = compare_assoc_types(t, impl_ty);
                    report.mismatches.extend(cmp.mismatch(
                        ItemKind::Type,
                        &name,
                        trait_span,
                        impl_ty.ident.span(),
                    ));
                    report.unknowns.extend(cmp.unknown(
                        ItemKind::Type,
                        &name,
                        trait_span,
                        impl_ty.ident.span(),
                    ));
                } else if fns.contains_key(&name) || consts.contains_key(&name) {
                    report.mismatches.push(SignatureMismatch {
                        kind: ItemKind::Type,
                        name,
                        differences: vec![Difference::ItemKind {
                            expected: "associated type",
                            found: "method or const",
                        }],
                        trait_span,
                        impl_span,
                    });
                } else {
                    report.missing.push(MissingItem {
                        kind: ItemKind::Type,
                        name,
                        trait_span,
                        impl_span,
                    });
                }
            }
            TraitItem::Const(c) => {
                // A defaulted associated const is not required.
                if c.default.is_some() {
                    continue;
                }
                let name = c.ident.to_string();
                let trait_span = c.ident.span();
                if let Some(impl_c) = consts.get(&name) {
                    let cmp = compare_consts(c, impl_c, &outer);
                    report.mismatches.extend(cmp.mismatch(
                        ItemKind::Const,
                        &name,
                        trait_span,
                        impl_c.ident.span(),
                    ));
                    report.unknowns.extend(cmp.unknown(
                        ItemKind::Const,
                        &name,
                        trait_span,
                        impl_c.ident.span(),
                    ));
                } else if fns.contains_key(&name) || types.contains_key(&name) {
                    report.mismatches.push(SignatureMismatch {
                        kind: ItemKind::Const,
                        name,
                        differences: vec![Difference::ItemKind {
                            expected: "associated const",
                            found: "method or associated type",
                        }],
                        trait_span,
                        impl_span,
                    });
                } else {
                    report.missing.push(MissingItem {
                        kind: ItemKind::Const,
                        name,
                        trait_span,
                        impl_span,
                    });
                }
            }
            _ => {}
        }
    }

    report
}

/// The outcome of a contract check, split into three disjoint categories.
#[derive(Debug, Default)]
pub struct ContractReport {
    /// Required associated items that the impl does not provide.
    pub missing: Vec<MissingItem>,
    /// Signature differences that are statically provable from syntax.
    pub mismatches: Vec<SignatureMismatch>,
    /// Pairs whose compatibility cannot be decided without name resolution
    /// or trait solving. These are *not* proven incompatibilities.
    pub unknowns: Vec<UnknownItem>,
}

impl ContractReport {
    /// No missing items and no proven mismatches. Unknowns may still exist.
    pub fn is_compatible(&self) -> bool {
        self.missing.is_empty() && self.mismatches.is_empty()
    }

    /// No findings of any kind.
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.mismatches.is_empty() && self.unknowns.is_empty()
    }
}

/// Which kind of associated item a diagnostic refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Method,
    Type,
    Const,
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            ItemKind::Method => "method",
            ItemKind::Type => "associated type",
            ItemKind::Const => "associated const",
        };
        f.write_str(s)
    }
}

/// A required trait item with no counterpart in the impl.
#[derive(Debug)]
pub struct MissingItem {
    pub kind: ItemKind,
    pub name: String,
    /// Span of the required item's name in the trait.
    pub trait_span: Span,
    /// Span of the `impl` keyword of the impl block lacking the item.
    pub impl_span: Span,
}

/// A matched pair of items whose signatures provably differ.
#[derive(Debug)]
pub struct SignatureMismatch {
    pub kind: ItemKind,
    pub name: String,
    pub differences: Vec<Difference>,
    /// Span of the item's name in the trait.
    pub trait_span: Span,
    /// Span of the item's name in the impl.
    pub impl_span: Span,
}

/// A matched pair of items whose compatibility cannot be decided statically.
#[derive(Debug)]
pub struct UnknownItem {
    pub kind: ItemKind,
    pub name: String,
    pub reasons: Vec<UnknownReason>,
    /// Span of the item's name in the trait.
    pub trait_span: Span,
    /// Span of the item's name in the impl.
    pub impl_span: Span,
}

/// A single statically provable signature difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// The impl provides an item of a different kind under the same name.
    ItemKind {
        expected: &'static str,
        found: &'static str,
    },
    Constness {
        expected: bool,
        found: bool,
    },
    Asyncness {
        expected: bool,
        found: bool,
    },
    Safety {
        expected: &'static str,
        found: &'static str,
    },
    /// Different ABI strings, e.g. `extern "C"` vs the default
    /// `"Rust"` ABI.
    Abi { expected: String, found: String },
    /// Different receiver shapes, e.g. `&self` vs `&mut self`.
    Receiver { expected: String, found: String },
    /// Different numbers of generic parameters of the given kind
    /// (`"type"` or `"const"`).
    GenericArity {
        kind: &'static str,
        expected: usize,
        found: usize,
    },
    /// Different types of a const generic parameter.
    ConstParamType {
        name: String,
        expected: String,
        found: String,
    },
    /// Different number of function arguments (receiver excluded).
    ParameterCount { expected: usize, found: usize },
    /// Different types of the function argument at `position` (0-based,
    /// receiver excluded).
    Parameter {
        position: usize,
        expected: String,
        found: String,
    },
    ReturnType { expected: String, found: String },
    /// Different types of an associated const.
    ConstType { expected: String, found: String },
    Variadic { expected: bool, found: bool },
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Difference::ItemKind { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
            Difference::Constness { expected, found } => {
                write!(f, "constness differs: expected const={expected}, found const={found}")
            }
            Difference::Asyncness { expected, found } => {
                write!(f, "asyncness differs: expected async={expected}, found async={found}")
            }
            Difference::Safety { expected, found } => {
                write!(f, "safety differs: expected {expected}, found {found}")
            }
            Difference::Abi { expected, found } => {
                write!(f, "ABI differs: expected extern \"{expected}\", found extern \"{found}\"")
            }
            Difference::Receiver { expected, found } => {
                write!(f, "receiver differs: expected {expected}, found {found}")
            }
            Difference::GenericArity {
                kind,
                expected,
                found,
            } => write!(
                f,
                "generic {kind} parameter count differs: expected {expected}, found {found}"
            ),
            Difference::ConstParamType {
                name,
                expected,
                found,
            } => write!(
                f,
                "const parameter {name} type differs: expected {expected}, found {found}"
            ),
            Difference::ParameterCount { expected, found } => {
                write!(f, "argument count differs: expected {expected}, found {found}")
            }
            Difference::Parameter {
                position,
                expected,
                found,
            } => write!(
                f,
                "type of argument #{position} differs: expected {expected}, found {found}"
            ),
            Difference::ReturnType { expected, found } => {
                write!(f, "return type differs: expected {expected}, found {found}")
            }
            Difference::ConstType { expected, found } => {
                write!(f, "associated const type differs: expected {expected}, found {found}")
            }
            Difference::Variadic { expected, found } => {
                write!(f, "variadic differs: expected {expected}, found {found}")
            }
        }
    }
}

/// A reason why compatibility could not be decided statically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// A bare (non-generic, non-primitive) identifier is involved in a type
    /// difference; it may be a type alias that resolves the difference.
    TypeAliasPossible { context: String },
    /// The `where` clauses or generic bounds differ syntactically; they
    /// may still be semantically equivalent.
    WhereClauseOrBounds,
    /// Lifetime parameter arity differs.
    LifetimeArity,
    /// An array length or const generic argument is an expression whose
    /// equality cannot be decided syntactically.
    ConstExpression,
    /// The trait's associated type has bounds; whether the impl's concrete
    /// type satisfies them requires trait solving.
    AssociatedTypeBounds,
    /// A qualified path (`<T as Tr>::Assoc`) or other construct
    /// requiring name resolution is involved.
    NameResolution,
    /// A trait-level generic argument of the impl is not a simple parameter
    /// (e.g. `impl Trait<u32> for S`), so uses of that parameter cannot
    /// be compared reliably.
    TraitGenericArgument,
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UnknownReason::TypeAliasPossible { context } => {
                write!(f, "{context}: type difference may be explained by a type alias")
            }
            UnknownReason::WhereClauseOrBounds => {
                write!(f, "where clauses or generic bounds differ syntactically")
            }
            UnknownReason::LifetimeArity => {
                write!(f, "lifetime parameter count differs")
            }
            UnknownReason::ConstExpression => {
                write!(f, "const expression cannot be compared syntactically")
            }
            UnknownReason::AssociatedTypeBounds => {
                write!(
                    f,
                    "cannot statically verify that the impl's type satisfies the \
                     associated type bounds"
                )
            }
            UnknownReason::NameResolution => {
                write!(f, "construct requires name resolution to compare")
            }
            UnknownReason::TraitGenericArgument => {
                write!(f, "trait-level generic argument is not a simple parameter")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Positional generic parameter mapping
// ---------------------------------------------------------------------------

/// Positional rename map from the trait's generic parameters to the impl's.
///
/// Type and const parameter names share one map (they cannot collide within
/// one generics list); lifetimes live in a separate namespace.
#[derive(Clone, Default)]
struct ParamMap {
    idents: HashMap<String, String>,
    lifetimes: HashMap<String, String>,
    /// Trait-level parameters whose impl argument is not a simple identifier
    /// (e.g. `impl Trait<u32> for S`). Any proven difference involving
    /// these is downgraded to `Unknown`.
    opaque: HashSet<String>,
}

impl ParamMap {
    fn rename_ident(&self, name: &str) -> String {
        self.idents
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }
}

/// Build the parameter map for the trait's *own* generics from the impl
/// header, e.g. `trait Foo<A>` + `impl Foo<B> for S` maps
/// `A -> B`.
fn outer_param_map(trait_def: &ItemTrait, impl_block: &ItemImpl) -> ParamMap {
    let mut map = ParamMap::default();
    let Some((trait_path, _)) = &impl_block.trait_ else {
        return map;
    };
    let args = trait_path.segments.last().map(|seg| &seg.arguments);
    let type_params: Vec<_> = trait_def.generics.type_params().collect();
    let const_params: Vec<_> = trait_def.generics.const_params().collect();
    let lifetime_params: Vec<_> = trait_def.generics.lifetimes().collect();
    if type_params.is_empty() && const_params.is_empty() && lifetime_params.is_empty() {
        return map;
    }
    let Some(PathArguments::AngleBracketed(args)) = args else {
        // Trait has generics but the impl path provides no (or parenthesized)
        // arguments: treat every parameter as opaque.
        for p in type_params {
            map.opaque.insert(p.ident.to_string());
        }
        for p in const_params {
            map.opaque.insert(p.ident.to_string());
        }
        return map;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    for param in type_params {
        match type_args.next() {
            Some(Type::Path(p))
                if p.qself.is_none()
                    && p.path.segments.len() == 1
                    && p.path.segments[0].arguments.is_none() =>
            {
                map.idents.insert(
                    param.ident.to_string(),
                    p.path.segments[0].ident.to_string(),
                );
            }
            _ => {
                map.opaque.insert(param.ident.to_string());
            }
        }
    }
    let mut const_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Const(_) => Some(a),
        _ => None,
    });
    for param in const_params {
        // A const argument that is a bare identifier can be mapped; anything
        // else makes the parameter opaque.
        let mapped = match const_args.next() {
            Some(GenericArgument::Const(syn::Expr::Path(p)))
                if p.path.segments.len() == 1 && p.qself.is_none() =>
            {
                Some(p.path.segments[0].ident.to_string())
            }
            _ => None,
        };
        match mapped {
            Some(name) => {
                map.idents.insert(param.ident.to_string(), name);
            }
            None => {
                map.opaque.insert(param.ident.to_string());
            }
        }
    }
    let mut lifetime_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Lifetime(l) => Some(l),
        _ => None,
    });
    for param in lifetime_params {
        if let Some(l) = lifetime_args.next() {
            map.lifetimes
                .insert(param.lifetime.ident.to_string(), l.ident.to_string());
        }
    }
    map
}

/// Apply the rename map to a token stream.
fn rename_tokens(ts: TokenStream, map: &ParamMap) -> TokenStream {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    let mut out: Vec<TokenTree> = Vec::with_capacity(tokens.len());
    let mut idx = 0;
    while idx < tokens.len() {
        match &tokens[idx] {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                match map.idents.get(&name) {
                    Some(new) => out.push(TokenTree::Ident(proc_macro2::Ident::new(
                        new,
                        ident.span(),
                    ))),
                    None => out.push(TokenTree::Ident(ident.clone())),
                }
            }
            TokenTree::Punct(p) if p.as_char() == '\'' => {
                out.push(TokenTree::Punct(p.clone()));
                if let Some(TokenTree::Ident(ident)) = tokens.get(idx + 1) {
                    if let Some(new) = map.lifetimes.get(&ident.to_string()) {
                        out.push(TokenTree::Ident(proc_macro2::Ident::new(
                            new,
                            ident.span(),
                        )));
                        idx += 2;
                        continue;
                    }
                }
            }
            TokenTree::Group(group) => {
                let mut renamed = proc_macro2::Group::new(
                    group.delimiter(),
                    rename_tokens(group.stream(), map),
                );
                renamed.set_span(group.span());
                out.push(TokenTree::Group(renamed));
            }
            other => out.push(other.clone()),
        }
        idx += 1;
    }
    out.into_iter().collect()
}

/// Normalized string form of a syntax node after applying the rename map.
fn normed<T: ToTokens>(node: &T, map: &ParamMap) -> String {
    rename_tokens(node.to_token_stream(), map).to_string()
}

/// Normalized string form of an impl-side node (no renaming).
fn normed_impl<T: ToTokens>(node: &T) -> String {
    node.to_token_stream().to_string()
}

/// Does the node's token stream mention any opaque trait-level parameter?
fn mentions_opaque<T: ToTokens>(node: &T, map: &ParamMap) -> bool {
    fn scan(ts: TokenStream, opaque: &HashSet<String>) -> bool {
        ts.into_iter().any(|tt| match tt {
            TokenTree::Ident(ident) => opaque.contains(&ident.to_string()),
            TokenTree::Group(g) => scan(g.stream(), opaque),
            _ => false,
        })
    }
    !map.opaque.is_empty() && scan(node.to_token_stream(), &map.opaque)
}

// ---------------------------------------------------------------------------
// Type comparison
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Same,
    Different,
    Unknown,
}

impl Verdict {
    fn and(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Different, _) | (_, Verdict::Different) => Verdict::Different,
            (Verdict::Unknown, _) | (_, Verdict::Unknown) => Verdict::Unknown,
            _ => Verdict::Same,
        }
    }
}

const PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f16", "f32", "f64", "f128",
];

/// A bare identifier path that is neither a primitive nor a known generic
/// parameter might be a type alias.
fn alias_suspect(ty: &Type, map: &ParamMap) -> bool {
    match unwrap_group(ty) {
        Type::Path(p) => alias_suspect_path(p, map),
        _ => false,
    }
}

fn alias_suspect_path(p: &TypePath, map: &ParamMap) -> bool {
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return false;
    }
    let seg = &p.path.segments[0];
    if !seg.arguments.is_none() {
        return false;
    }
    let name = seg.ident.to_string();
    if name == "Self" || PRIMITIVES.contains(&name.as_str()) {
        return false;
    }
    // A generic parameter (under either its trait-side or impl-side name) is
    // a real parameter, not an alias.
    !map.idents.contains_key(&name) && !map.idents.values().any(|v| v == &name)
}

fn unwrap_group(ty: &Type) -> &Type {
    match ty {
        Type::Group(g) => unwrap_group(&g.elem),
        Type::Paren(p) => unwrap_group(&p.elem),
        other => other,
    }
}

/// Compare two types, applying the positional rename map to the trait side.
///
/// `Different` is only returned when no type alias could explain the
/// difference; anything requiring alias expansion or name resolution yields
/// `Unknown`.
fn compare_types(trait_ty: &Type, impl_ty: &Type, map: &ParamMap) -> Verdict {
    if normed(trait_ty, map) == normed_impl(impl_ty) {
        return Verdict::Same;
    }
    let verdict = compare_types_inner(trait_ty, impl_ty, map);
    if verdict == Verdict::Different && mentions_opaque(trait_ty, map) {
        return Verdict::Unknown;
    }
    verdict
}

fn compare_types_inner(trait_ty: &Type, impl_ty: &Type, map: &ParamMap) -> Verdict {
    let t = unwrap_group(trait_ty);
    let i = unwrap_group(impl_ty);
    match (t, i) {
        (Type::Reference(a), Type::Reference(b)) => {
            if a.mutability.is_some() != b.mutability.is_some() {
                return Verdict::Different;
            }
            // Lifetime names are insignificant (positional mapping was
            // already applied to the token comparison above).
            compare_types(&a.elem, &b.elem, map)
        }
        (Type::Ptr(a), Type::Ptr(b)) => {
            if std::mem::discriminant(&a.mutability) != std::mem::discriminant(&b.mutability)
            {
                return Verdict::Different;
            }
            compare_types(&a.elem, &b.elem, map)
        }
        (Type::Path(a), Type::Path(b)) => compare_type_paths(a, b, map),
        (Type::Tuple(a), Type::Tuple(b)) => {
            if a.elems.len() != b.elems.len() {
                return Verdict::Different;
            }
            a.elems
                .iter()
                .zip(b.elems.iter())
                .fold(Verdict::Same, |acc, (x, y)| {
                    acc.and(compare_types(x, y, map))
                })
        }
        (Type::Slice(a), Type::Slice(b)) => compare_types(&a.elem, &b.elem, map),
        (Type::Array(a), Type::Array(b)) => {
            let elem = compare_types(&a.elem, &b.elem, map);
            // Array lengths are const expressions: syntactic inequality does
            // not prove semantic inequality.
            let len = if normed(&a.len, map) == normed_impl(&b.len) {
                Verdict::Same
            } else {
                Verdict::Unknown
            };
            elem.and(len)
        }
        // Anything else (trait objects, impl Trait, bare fn types, macros,
        // qualified paths, ...) cannot be decomposed reliably; if the token
        // comparison above already failed, we cannot prove a difference.
        _ => {
            if alias_suspect(t, map) || alias_suspect(i, map) {
                Verdict::Unknown
            } else if std::mem::discriminant(t) == std::mem::discriminant(i) {
                // Same shape but token-different internals we do not
                // decompose (e.g. `dyn A` vs `dyn B`, `fn(u8)` vs `fn(u16)`).
                Verdict::Unknown
            } else {
                match (t, i) {
                    // Never and Infer are leaf tokens; equality was already
                    // established by the token comparison.
                    (Type::Never(_), Type::Never(_)) => Verdict::Same,
                    (Type::Infer(_), Type::Infer(_)) => Verdict::Same,
                    // Different type constructors with no alias suspect:
                    // e.g. `&T` vs `(T, T)`, `[T]` vs `u32`.
                    (
                        Type::Reference(_) | Type::Ptr(_) | Type::Tuple(_) | Type::Slice(_)
                        | Type::Array(_),
                        _,
                    )
                    | (
                        _,
                        Type::Reference(_) | Type::Ptr(_) | Type::Tuple(_) | Type::Slice(_)
                        | Type::Array(_),
                    ) => Verdict::Different,
                    _ => Verdict::Unknown,
                }
            }
        }
    }
}

fn compare_type_paths(a: &TypePath, b: &TypePath, map: &ParamMap) -> Verdict {
    if a.qself.is_some() || b.qself.is_some() {
        // `<T as Tr>::Assoc` requires trait resolution.
        return Verdict::Unknown;
    }
    let sa: Vec<_> = a.path.segments.iter().collect();
    let sb: Vec<_> = b.path.segments.iter().collect();
    if sa.len() != sb.len() {
        // Different path lengths (e.g. `Self::A` vs `u32`) can only be
        // reconciled by name resolution.
        return Verdict::Unknown;
    }
    for (x, y) in sa.iter().zip(sb.iter()) {
        let xn = map.rename_ident(&x.ident.to_string());
        if xn != y.ident.to_string() {
            return if sa.len() == 1 {
                // Single-segment paths: a generic head like `Vec` vs `Option`
                // is structural; a bare identifier may be an alias.
                if alias_suspect_path(a, map) || alias_suspect_path(b, map) {
                    Verdict::Unknown
                } else {
                    Verdict::Different
                }
            } else {
                // Multi-segment paths may route through aliases or associated
                // items; proving difference needs name resolution.
                Verdict::Unknown
            };
        }
        let args = compare_path_arguments(&x.arguments, &y.arguments, map);
        if args != Verdict::Same {
            return args;
        }
    }
    Verdict::Same
}

fn compare_path_arguments(a: &PathArguments, b: &PathArguments, map: &ParamMap) -> Verdict {
    match (a, b) {
        (PathArguments::None, PathArguments::None) => Verdict::Same,
        (PathArguments::AngleBracketed(x), PathArguments::AngleBracketed(y)) => {
            if x.args.len() != y.args.len() {
                // Defaulted generic arguments (e.g. `Vec<T>` vs
                // `Vec<T, Global>`) make arity differences undecidable.
                return Verdict::Unknown;
            }
            x.args
                .iter()
                .zip(y.args.iter())
                .fold(Verdict::Same, |acc, (p, q)| {
                    acc.and(compare_generic_arguments(p, q, map))
                })
        }
        // `Fn(A) -> B` sugar, or one side with arguments and one without:
        // decidable only up to aliases and defaults.
        _ => Verdict::Unknown,
    }
}

fn compare_generic_arguments(a: &GenericArgument, b: &GenericArgument, map: &ParamMap) -> Verdict {
    match (a, b) {
        (GenericArgument::Type(x), GenericArgument::Type(y)) => compare_types(x, y, map),
        // Lifetime names are insignificant.
        (GenericArgument::Lifetime(_), GenericArgument::Lifetime(_)) => Verdict::Same,
        // Const expressions and associated bindings/constraints: syntactic
        // inequality proves nothing.
        _ => {
            if normed(a, map) == normed_impl(b) {
                Verdict::Same
            } else {
                Verdict::Unknown
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Signature comparison
// ---------------------------------------------------------------------------

struct Comparison {
    differences: Vec<Difference>,
    unknowns: Vec<UnknownReason>,
}

impl Comparison {
    fn new() -> Self {
        Comparison {
            differences: Vec::new(),
            unknowns: Vec::new(),
        }
    }

    fn mismatch(
        &self,
        kind: ItemKind,
        name: &str,
        trait_span: Span,
        impl_span: Span,
    ) -> Option<SignatureMismatch> {
        if self.differences.is_empty() {
            None
        } else {
            Some(SignatureMismatch {
                kind,
                name: name.to_string(),
                differences: self.differences.clone(),
                trait_span,
                impl_span,
            })
        }
    }

    fn unknown(
        &self,
        kind: ItemKind,
        name: &str,
        trait_span: Span,
        impl_span: Span,
    ) -> Option<UnknownItem> {
        if self.unknowns.is_empty() {
            None
        } else {
            Some(UnknownItem {
                kind,
                name: name.to_string(),
                reasons: self.unknowns.clone(),
                trait_span,
                impl_span,
            })
        }
    }
}

fn safety_str(safety: &Safety) -> &'static str {
    match safety {
        Safety::Safe(_) => "safe",
        Safety::Unsafe(_) => "unsafe",
        Safety::Default => "default",
    }
}

fn abi_name(abi: &Option<Abi>) -> String {
    match abi {
        None => "Rust".to_string(),
        Some(abi) => abi
            .name
            .as_ref()
            .map(|name| name.value())
            .unwrap_or_else(|| "C".to_string()),
    }
}

fn receiver_str(receiver: Option<&Receiver>) -> String {
    match receiver {
        None => "(none)".to_string(),
        Some(r) => normed_impl(r),
    }
}

fn compare_receivers(
    trait_recv: Option<&Receiver>,
    impl_recv: Option<&Receiver>,
    map: &ParamMap,
) -> Verdict {
    match (trait_recv, impl_recv) {
        (None, None) => Verdict::Same,
        (Some(a), Some(b)) => match (&a.kind, &b.kind) {
            // `self` vs `mut self` is an allowed binding-mode difference.
            (ReceiverKind::Value, ReceiverKind::Value) => Verdict::Same,
            (ReceiverKind::Reference(_, _, m1), ReceiverKind::Reference(_, _, m2)) => {
                if m1.is_some() == m2.is_some() {
                    Verdict::Same
                } else {
                    Verdict::Different
                }
            }
            (ReceiverKind::Typed(_, t1), ReceiverKind::Typed(_, t2)) => {
                compare_types(t1, t2, map)
            }
            _ => Verdict::Different,
        },
        _ => Verdict::Different,
    }
}

/// Extend `map` with the method-level generic parameters and compare the
/// generic structure of both signatures.
fn compare_generics(t: &Generics, i: &Generics, map: &mut ParamMap, cmp: &mut Comparison) {
    let tps: Vec<_> = t.type_params().collect();
    let ips: Vec<_> = i.type_params().collect();
    if tps.len() != ips.len() {
        cmp.differences.push(Difference::GenericArity {
            kind: "type",
            expected: tps.len(),
            found: ips.len(),
        });
    }
    for (tp, ip) in tps.iter().zip(ips.iter()) {
        map.idents
            .insert(tp.ident.to_string(), ip.ident.to_string());
    }

    let cps: Vec<_> = t.const_params().collect();
    let cips: Vec<_> = i.const_params().collect();
    if cps.len() != cips.len() {
        cmp.differences.push(Difference::GenericArity {
            kind: "const",
            expected: cps.len(),
            found: cips.len(),
        });
    }
    for (cp, cip) in cps.iter().zip(cips.iter()) {
        map.idents
            .insert(cp.ident.to_string(), cip.ident.to_string());
    }
    // Const parameter types are part of the signature and concretely
    // comparable (`const N: usize` vs `const N: u32`).
    for (cp, cip) in cps.iter().zip(cips.iter()) {
        if let Verdict::Different = compare_types(&cp.ty, &cip.ty, map) {
            cmp.differences.push(Difference::ConstParamType {
                name: cp.ident.to_string(),
                expected: normed(&cp.ty, map),
                found: normed_impl(&cip.ty),
            });
        }
    }

    if t.lifetimes().count() != i.lifetimes().count() {
        cmp.unknowns.push(UnknownReason::LifetimeArity);
    } else {
        for (tl, il) in t.lifetimes().zip(i.lifetimes()) {
            map.lifetimes.insert(
                tl.lifetime.ident.to_string(),
                il.lifetime.ident.to_string(),
            );
        }
    }

    // Bounds and where clauses: only syntactic equality (after renaming) is
    // decidable; any difference may still be semantically equivalent.
    for (tp, ip) in tps.iter().zip(ips.iter()) {
        if normed(&tp.bounds, map) != normed_impl(&ip.bounds)
            || tp.colon_token.is_some() != ip.colon_token.is_some()
        {
            cmp.unknowns.push(UnknownReason::WhereClauseOrBounds);
            break;
        }
    }
    let tw = t.where_clause.as_ref().map(|w| normed(w, map));
    let iw = i.where_clause.as_ref().map(normed_impl);
    if tw != iw {
        cmp.unknowns.push(UnknownReason::WhereClauseOrBounds);
    }
}

fn typed_args(sig: &Signature) -> Vec<&PatType> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pt) => Some(pt),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn compare_fn_sigs(t: &Signature, i: &Signature, outer: &ParamMap) -> Comparison {
    let mut cmp = Comparison::new();

    if t.constness.is_some() != i.constness.is_some() {
        cmp.differences.push(Difference::Constness {
            expected: t.constness.is_some(),
            found: i.constness.is_some(),
        });
    }
    if t.asyncness.is_some() != i.asyncness.is_some() {
        cmp.differences.push(Difference::Asyncness {
            expected: t.asyncness.is_some(),
            found: i.asyncness.is_some(),
        });
    }
    if safety_str(&t.safety) != safety_str(&i.safety) {
        cmp.differences.push(Difference::Safety {
            expected: safety_str(&t.safety),
            found: safety_str(&i.safety),
        });
    }
    let (ta, ia) = (abi_name(&t.abi), abi_name(&i.abi));
    if ta != ia {
        cmp.differences.push(Difference::Abi {
            expected: ta,
            found: ia,
        });
    }

    let mut map = outer.clone();
    compare_generics(&t.generics, &i.generics, &mut map, &mut cmp);

    match compare_receivers(t.receiver(), i.receiver(), &map) {
        Verdict::Same => {}
        Verdict::Different => cmp.differences.push(Difference::Receiver {
            expected: receiver_str(t.receiver()),
            found: receiver_str(i.receiver()),
        }),
        Verdict::Unknown => cmp.unknowns.push(UnknownReason::TypeAliasPossible {
            context: "receiver type".to_string(),
        }),
    }

    let tp = typed_args(t);
    let ip = typed_args(i);
    if tp.len() != ip.len() {
        cmp.differences.push(Difference::ParameterCount {
            expected: tp.len(),
            found: ip.len(),
        });
    } else {
        for (pos, (a, b)) in tp.iter().zip(ip.iter()).enumerate() {
            match compare_types(&a.ty, &b.ty, &map) {
                Verdict::Same => {}
                Verdict::Different => cmp.differences.push(Difference::Parameter {
                    position: pos,
                    expected: normed(&a.ty, &map),
                    found: normed_impl(&b.ty),
                }),
                Verdict::Unknown => cmp.unknowns.push(UnknownReason::TypeAliasPossible {
                    context: format!("type of argument #{pos}"),
                }),
            }
        }
    }

    match compare_return_types(&t.output, &i.output, &map) {
        Verdict::Same => {}
        Verdict::Different => cmp.differences.push(Difference::ReturnType {
            expected: return_type_str(&t.output, &map),
            found: return_type_str_impl(&i.output),
        }),
        Verdict::Unknown => cmp.unknowns.push(UnknownReason::TypeAliasPossible {
            context: "return type".to_string(),
        }),
    }

    if t.variadic.is_some() != i.variadic.is_some() {
        cmp.differences.push(Difference::Variadic {
            expected: t.variadic.is_some(),
            found: i.variadic.is_some(),
        });
    }

    cmp
}

fn is_unit(ty: &Type) -> bool {
    matches!(unwrap_group(ty), Type::Tuple(t) if t.elems.is_empty())
}

fn compare_return_types(t: &ReturnType, i: &ReturnType, map: &ParamMap) -> Verdict {
    match (t, i) {
        (ReturnType::Default, ReturnType::Default) => Verdict::Same,
        (ReturnType::Type(_, a), ReturnType::Type(_, b)) => compare_types(a, b, map),
        (ReturnType::Default, ReturnType::Type(_, b))
        | (ReturnType::Type(_, b), ReturnType::Default) => {
            if is_unit(b) {
                Verdict::Same
            } else if alias_suspect(b, map) {
                Verdict::Unknown
            } else {
                Verdict::Different
            }
        }
    }
}

fn return_type_str(output: &ReturnType, map: &ParamMap) -> String {
    match output {
        ReturnType::Default => "()".to_string(),
        ReturnType::Type(_, ty) => normed(ty, map),
    }
}

fn return_type_str_impl(output: &ReturnType) -> String {
    match output {
        ReturnType::Default => "()".to_string(),
        ReturnType::Type(_, ty) => normed_impl(ty),
    }
}

fn compare_assoc_types(t: &TraitItemType, i: &ImplItemType) -> Comparison {
    let mut cmp = Comparison::new();
    let (tt, it) = (
        t.generics.type_params().count(),
        i.generics.type_params().count(),
    );
    if tt != it {
        cmp.differences.push(Difference::GenericArity {
            kind: "type",
            expected: tt,
            found: it,
        });
    }
    let (tc, ic) = (
        t.generics.const_params().count(),
        i.generics.const_params().count(),
    );
    if tc != ic {
        cmp.differences.push(Difference::GenericArity {
            kind: "const",
            expected: tc,
            found: ic,
        });
    }
    if t.generics.lifetimes().count() != i.generics.lifetimes().count() {
        cmp.unknowns.push(UnknownReason::LifetimeArity);
    }
    if !t.bounds.is_empty() {
        // Whether the impl's concrete type satisfies the bounds requires
        // trait solving.
        cmp.unknowns.push(UnknownReason::AssociatedTypeBounds);
    }
    cmp
}

fn compare_consts(t: &TraitItemConst, i: &ImplItemConst, outer: &ParamMap) -> Comparison {
    let mut cmp = Comparison::new();
    match compare_types(&t.ty, &i.ty, outer) {
        Verdict::Same => {}
        Verdict::Different => cmp.differences.push(Difference::ConstType {
            expected: normed(&t.ty, outer),
            found: normed_impl(&i.ty),
        }),
        Verdict::Unknown => cmp.unknowns.push(UnknownReason::TypeAliasPossible {
            context: "associated const type".to_string(),
        }),
    }
    cmp
}
