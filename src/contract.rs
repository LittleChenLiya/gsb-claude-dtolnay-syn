//! Static contract checking between a trait and its implementation.
//!
//! This module provides [`check`], which compares the associated items of a
//! trait definition ([`ItemTrait`]) with the items provided by an impl block
//! ([`ItemImpl`]) without involving rustc. It is intended for procedural
//! macros and other tooling that wants a best-effort, purely syntactic answer
//! to "does this impl satisfy the trait contract?".
//!
//! # What is checked
//!
//! For every associated item of the trait, the impl is searched for an item
//! with the same name. The report classifies the outcome as one of:
//!
//! - **Missing** — the trait requires the item (no default is provided) and the
//!   impl does not supply it. Default methods, associated constants with a
//!   default value, and associated types with a default are *not* reported as
//!   missing.
//! - **Mismatch** — the item exists on both sides and the difference is
//!   statically confirmable: generic parameter counts/kinds, parameter and
//!   return types, receiver kind (`self` vs `&mut self`, ...), `async`,
//!   `unsafe`, ABI (`extern "C"`), variadic arguments, and the types of
//!   associated constants.
//! - **Unknown** — the item cannot be decided from syntax alone: macro or
//!   verbatim items, type aliases that would need expanding, trait bounds that
//!   would need solving, and where clauses that are too complex to compare
//!   positionally.
//!
//! # Static decidability
//!
//! The comparison is purely syntactic, with a few deliberately conservative
//! rules:
//!
//! - Generic parameters are mapped **by position**, not by name: `fn f<T>(T)`
//!   and `fn f<U>(U)` are considered isomorphic.
//! - A trait-side path segment that names a generic parameter is required to
//!   name the parameter at the same position on the impl side; every other
//!   path segment must match verbatim.
//! - `Self` on the trait side matches `Self` (or the impl's self type name) on
//!   the impl side.
//! - Parenthesized and grouped types are compared through their inner type.
//! - `fn f()` and `fn f() -> ()` are considered equal.
//! - Array lengths and const generic arguments are compared only when they are
//!   literals or paths; anything more complex is reported as unknown.
//!
//! The following are **not** reported as mismatches:
//!
//! - The `const`-ness of a function (a `const fn` may implement a non-const
//!   trait method; it is not part of the type signature).
//! - The mutability of a by-value `self` binding (`self` vs `mut self`).
//! - Type alias expansion (`type X = ...`) and trait bound solving — these are
//!   reported as unknown instead.
//!
//! # Examples
//!
//! A missing required method:
//!
//! ```
//! use syn::contract::check;
//! use syn::{parse_quote, ItemImpl, ItemTrait};
//!
//! let trait_def: ItemTrait = parse_quote! {
//!     trait Foo {
//!         fn required(&self);
//!         fn provided(&self) {}
//!     }
//! };
//! let impl_def: ItemImpl = parse_quote! {
//!     impl Foo for () {
//!         fn provided(&self) {}
//!     }
//! };
//!
//! let report = check(&trait_def, &impl_def);
//! assert_eq!(report.missing.len(), 1);
//! assert_eq!(report.missing[0].name, "required");
//! ```
//!
//! A receiver mismatch:
//!
//! ```
//! use syn::contract::{check, Difference};
//! use syn::{parse_quote, ItemImpl, ItemTrait};
//!
//! let trait_def: ItemTrait = parse_quote! {
//!     trait Foo {
//!         fn f(&self);
//!     }
//! };
//! let impl_def: ItemImpl = parse_quote! {
//!     impl Foo for () {
//!         fn f(&mut self) {}
//!     }
//! };
//!
//! let report = check(&trait_def, &impl_def);
//! assert_eq!(report.mismatches.len(), 1);
//! assert!(report.mismatches[0]
//!     .differences
//!     .contains(&Difference::Receiver));
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use proc_macro2::Span;

use crate::expr::Expr;
use crate::generics::{
    BoundLifetimes, GenericParam, Generics, TypeParamBound, WhereClause, WherePredicate,
};
use crate::ident::Ident;
use crate::item::{
    FnArg, ImplItem, ImplItemConst, ImplItemFn, ImplItemType, ItemImpl, ItemTrait, Receiver,
    ReceiverKind, Safety, Signature, TraitItem, TraitItemConst, TraitItemType,
};
use crate::lifetime::Lifetime;
use crate::path::{GenericArgument, Path, PathArguments, PathSegment};
use crate::punctuated::Punctuated;
use crate::ty::{Abi, NamedArg, PointerMutability, ReturnType, Type, TypeFnPtr, TypePath, TypePtr};

/// The kind of an associated item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    /// An associated function or method.
    Fn,
    /// An associated constant.
    Const,
    /// An associated type.
    Type,
}

/// A required associated item that the impl does not provide.
#[derive(Debug, Clone)]
pub struct MissingItem {
    /// The kind of item that is missing.
    pub kind: ItemKind,
    /// The name of the item.
    pub name: String,
    /// Span of the item in the trait definition.
    pub trait_span: Span,
    /// Span of the impl block (no matching impl item exists).
    pub impl_span: Span,
}

/// A statically confirmable difference between two items with the same name.
#[derive(Debug, Clone)]
pub struct SignatureMismatch {
    /// The kind of item that mismatches.
    pub kind: ItemKind,
    /// The name of the item.
    pub name: String,
    /// Span of the item in the trait definition.
    pub trait_span: Span,
    /// Span of the item in the impl block.
    pub impl_span: Span,
    /// The confirmed differences between the two signatures.
    pub differences: Vec<Difference>,
}

/// A difference between a trait item and the corresponding impl item that can
/// be confirmed without involving rustc.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Difference {
    /// The number of generic parameters differs.
    GenericCount {
        /// Number of generic parameters on the trait side.
        trait_count: usize,
        /// Number of generic parameters on the impl side.
        impl_count: usize,
    },
    /// The generic parameter at the given position has a different kind
    /// (lifetime, type, or const).
    GenericKind {
        /// Position of the mismatching parameter.
        position: usize,
    },
    /// The bounds of the generic parameter at the given position differ.
    GenericBound {
        /// Position of the mismatching parameter.
        position: usize,
    },
    /// A where predicate differs or is present on only one side.
    WherePredicate,
    /// The receiver (`self`) differs.
    Receiver,
    /// `async` differs.
    Async,
    /// `unsafe` differs.
    Unsafe,
    /// The ABI (`extern "C"`) differs.
    Abi,
    /// Variadic arguments differ.
    Variadic,
    /// The number of function parameters differs.
    ParamCount {
        /// Number of parameters on the trait side.
        trait_count: usize,
        /// Number of parameters on the impl side.
        impl_count: usize,
    },
    /// The parameter at the given position has a different type.
    ParamType {
        /// Position of the mismatching parameter.
        position: usize,
    },
    /// The return type differs.
    ReturnType,
    /// The type of an associated constant differs.
    ConstType,
    /// The generics of an associated type differ.
    AssocTypeGenerics,
}

/// An item that cannot be decided from syntax alone.
#[derive(Debug, Clone)]
pub struct UnknownItem {
    /// The name of the item, if it has one.
    pub name: Option<String>,
    /// Span of the item in the trait definition, if present.
    pub trait_span: Option<Span>,
    /// Span of the item in the impl block, if present.
    pub impl_span: Option<Span>,
    /// The reason the item could not be decided.
    pub reason: UnknownReason,
}

/// The reason an item could not be decided statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnknownReason {
    /// The trait side contains a macro or verbatim item.
    TraitMacro,
    /// The impl side contains a macro or verbatim item.
    ImplMacro,
    /// The item exists on both sides but a type alias would need expanding.
    TypeAlias,
    /// The item exists on both sides but a trait bound would need solving.
    TraitBound,
    /// The item exists on both sides but the where clause is too complex.
    ComplexWhere,
    /// The item exists on both sides but an expression is too complex.
    ComplexExpr,
    /// The item exists on both sides but a type cannot be compared.
    ComplexType,
}

/// The result of checking a trait against an impl block.
#[derive(Debug, Clone, Default)]
pub struct ContractReport {
    /// Required items that the impl does not provide.
    pub missing: Vec<MissingItem>,
    /// Items present on both sides with statically confirmable differences.
    pub mismatches: Vec<SignatureMismatch>,
    /// Items that cannot be decided from syntax alone.
    pub unknown: Vec<UnknownItem>,
}

impl ContractReport {
    /// Returns `true` if no missing items, mismatches, or unknowns were found.
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty() && self.mismatches.is_empty() && self.unknown.is_empty()
    }
}

/// Compare the associated items of a trait definition with the items provided
/// by an impl block, without involving rustc.
///
/// See the [module-level documentation] for details on what is checked and how
/// to interpret the result.
///
/// [module-level documentation]: crate::contract
pub fn check(trait_def: &ItemTrait, impl_def: &ItemImpl) -> ContractReport {
    let mut report = ContractReport::default();
    let impl_span = impl_def.impl_token.span;
    let self_name = self_type_name(&impl_def.self_ty);

    for trait_item in &trait_def.items {
        match trait_item {
            TraitItem::Fn(tf) => {
                let name = tf.sig.ident.to_string();
                match find_impl_fn(impl_def, &name) {
                    Some(imf) => {
                        let (diffs, unknowns) =
                            compare_fn(&tf.sig, &imf.sig, self_name.as_deref());
                        push_result(
                            &mut report,
                            ItemKind::Fn,
                            &name,
                            tf.sig.ident.span(),
                            imf.sig.ident.span(),
                            diffs,
                            unknowns,
                        );
                    }
                    None if tf.default.is_none() => {
                        report.missing.push(MissingItem {
                            kind: ItemKind::Fn,
                            name,
                            trait_span: tf.sig.ident.span(),
                            impl_span,
                        });
                    }
                    None => {}
                }
            }
            TraitItem::Const(tc) => {
                let name = tc.ident.to_string();
                match find_impl_const(impl_def, &name) {
                    Some(ic) => {
                        let (diffs, unknowns) = compare_const(tc, ic);
                        push_result(
                            &mut report,
                            ItemKind::Const,
                            &name,
                            tc.ident.span(),
                            ic.ident.span(),
                            diffs,
                            unknowns,
                        );
                    }
                    None if tc.default.is_none() => {
                        report.missing.push(MissingItem {
                            kind: ItemKind::Const,
                            name,
                            trait_span: tc.ident.span(),
                            impl_span,
                        });
                    }
                    None => {}
                }
            }
            TraitItem::Type(tt) => {
                let name = tt.ident.to_string();
                match find_impl_type(impl_def, &name) {
                    Some(it) => {
                        let (diffs, unknowns) = compare_type_item(tt, it);
                        push_result(
                            &mut report,
                            ItemKind::Type,
                            &name,
                            tt.ident.span(),
                            it.ident.span(),
                            diffs,
                            unknowns,
                        );
                    }
                    None if tt.default.is_none() => {
                        report.missing.push(MissingItem {
                            kind: ItemKind::Type,
                            name,
                            trait_span: tt.ident.span(),
                            impl_span,
                        });
                    }
                    None => {}
                }
            }
            TraitItem::Macro(mac) => {
                report.unknown.push(UnknownItem {
                    name: None,
                    trait_span: Some(mac.mac.bang_token.span),
                    impl_span: None,
                    reason: UnknownReason::TraitMacro,
                });
            }
            TraitItem::Verbatim(_) => {
                report.unknown.push(UnknownItem {
                    name: None,
                    trait_span: None,
                    impl_span: None,
                    reason: UnknownReason::TraitMacro,
                });
            }
        }
    }

    for impl_item in &impl_def.items {
        match impl_item {
            ImplItem::Macro(mac) => {
                report.unknown.push(UnknownItem {
                    name: None,
                    trait_span: None,
                    impl_span: Some(mac.mac.bang_token.span),
                    reason: UnknownReason::ImplMacro,
                });
            }
            ImplItem::Verbatim(_) => {
                report.unknown.push(UnknownItem {
                    name: None,
                    trait_span: None,
                    impl_span: None,
                    reason: UnknownReason::ImplMacro,
                });
            }
            _ => {}
        }
    }

    report
}

fn push_result(
    report: &mut ContractReport,
    kind: ItemKind,
    name: &str,
    trait_span: Span,
    impl_span: Span,
    diffs: Vec<Difference>,
    unknowns: Vec<UnknownReason>,
) {
    if !diffs.is_empty() {
        report.mismatches.push(SignatureMismatch {
            kind,
            name: name.to_string(),
            trait_span,
            impl_span,
            differences: diffs,
        });
    }
    for reason in unknowns {
        report.unknown.push(UnknownItem {
            name: Some(name.to_string()),
            trait_span: Some(trait_span),
            impl_span: Some(impl_span),
            reason,
        });
    }
}

fn find_impl_fn<'a>(impl_def: &'a ItemImpl, name: &str) -> Option<&'a ImplItemFn> {
    impl_def.items.iter().find_map(|item| match item {
        ImplItem::Fn(f) if f.sig.ident == name => Some(f),
        _ => None,
    })
}

fn find_impl_const<'a>(impl_def: &'a ItemImpl, name: &str) -> Option<&'a ImplItemConst> {
    impl_def.items.iter().find_map(|item| match item {
        ImplItem::Const(c) if c.ident == name => Some(c),
        _ => None,
    })
}

fn find_impl_type<'a>(impl_def: &'a ItemImpl, name: &str) -> Option<&'a ImplItemType> {
    impl_def.items.iter().find_map(|item| match item {
        ImplItem::Type(t) if t.ident == name => Some(t),
        _ => None,
    })
}

/// The name of the impl's self type, if it is a simple path (`Foo`).
fn self_type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(tp) if tp.qself.is_none() => {
            tp.path.segments.first().map(|s| s.ident.to_string())
        }
        _ => None,
    }
}

/// A mapping from trait-side generic parameter names to impl-side names, built
/// positionally.
#[derive(Default, Clone)]
struct Mapping {
    types: Vec<(Ident, Ident)>,
    lifetimes: Vec<(Ident, Ident)>,
    consts: Vec<(Ident, Ident)>,
}

impl Mapping {
    fn map_type(&self, id: &Ident) -> Option<&Ident> {
        self.types.iter().find(|(t, _)| t == id).map(|(_, i)| i)
    }

    fn map_lifetime(&self, id: &Ident) -> Option<&Ident> {
        self.lifetimes.iter().find(|(t, _)| t == id).map(|(_, i)| i)
    }

    fn map_const(&self, id: &Ident) -> Option<&Ident> {
        self.consts.iter().find(|(t, _)| t == id).map(|(_, i)| i)
    }
}

/// The outcome of comparing two syntax nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Equal,
    Different,
    Unknown(UnknownReason),
}

impl Cmp {
    fn is_equal(self) -> bool {
        self == Cmp::Equal
    }

}

/// Compare the generics of a trait item and an impl item positionally.
///
/// Returns the mapping and a list of confirmed differences. If the parameter
/// counts or kinds do not line up, the mapping is `None` and the caller should
/// not attempt to compare the rest of the signature.
fn compare_generics(
    trait_g: &Generics,
    impl_g: &Generics,
) -> (Option<Mapping>, Vec<Difference>) {
    let mut diffs = Vec::new();
    let t_params: Vec<&GenericParam> = trait_g.params.iter().collect();
    let i_params: Vec<&GenericParam> = impl_g.params.iter().collect();

    if t_params.len() != i_params.len() {
        diffs.push(Difference::GenericCount {
            trait_count: t_params.len(),
            impl_count: i_params.len(),
        });
        return (None, diffs);
    }

    let mut mapping = Mapping::default();
    for (position, (t, i)) in t_params.iter().zip(i_params.iter()).enumerate() {
        match (t, i) {
            (GenericParam::Lifetime(tl), GenericParam::Lifetime(il)) => {
                mapping
                    .lifetimes
                    .push((tl.lifetime.ident.clone(), il.lifetime.ident.clone()));
                if !compare_lifetime_list(&tl.bounds, &il.bounds, &mapping).is_equal() {
                    diffs.push(Difference::GenericBound { position });
                }
            }
            (GenericParam::Type(tt), GenericParam::Type(it)) => {
                mapping.types.push((tt.ident.clone(), it.ident.clone()));
                if !compare_bounds(&tt.bounds, &it.bounds, &mapping).is_equal() {
                    diffs.push(Difference::GenericBound { position });
                }
            }
            (GenericParam::Const(tc), GenericParam::Const(ic)) => {
                mapping.consts.push((tc.ident.clone(), ic.ident.clone()));
                match compare_type(&tc.ty, &ic.ty, &mapping, None) {
                    Cmp::Equal => {}
                    Cmp::Different | Cmp::Unknown(_) => {
                        diffs.push(Difference::GenericBound { position });
                    }
                }
            }
            _ => {
                diffs.push(Difference::GenericKind { position });
            }
        }
    }

    if diffs
        .iter()
        .any(|d| matches!(d, Difference::GenericKind { .. }))
    {
        return (None, diffs);
    }

    (Some(mapping), diffs)
}

/// Compare two function signatures.
fn compare_fn(
    trait_sig: &Signature,
    impl_sig: &Signature,
    self_name: Option<&str>,
) -> (Vec<Difference>, Vec<UnknownReason>) {
    let mut diffs = Vec::new();
    let mut unknowns = Vec::new();

    let (mapping, generic_diffs) = compare_generics(&trait_sig.generics, &impl_sig.generics);
    diffs.extend(generic_diffs);

    let mapping = match mapping {
        Some(m) => m,
        None => return (diffs, unknowns),
    };

    // Where clauses.
    match compare_where(
        &trait_sig.generics.where_clause,
        &impl_sig.generics.where_clause,
        &mapping,
    ) {
        Cmp::Equal => {}
        Cmp::Different => diffs.push(Difference::WherePredicate),
        Cmp::Unknown(r) => unknowns.push(r),
    }

    // Receiver.
    match compare_receiver(trait_sig.receiver(), impl_sig.receiver(), &mapping) {
        Cmp::Equal => {}
        Cmp::Different => diffs.push(Difference::Receiver),
        Cmp::Unknown(r) => unknowns.push(r),
    }

    // Async.
    if trait_sig.asyncness.is_some() != impl_sig.asyncness.is_some() {
        diffs.push(Difference::Async);
    }

    // Safety.
    if !compare_safety(&trait_sig.safety, &impl_sig.safety) {
        diffs.push(Difference::Unsafe);
    }

    // ABI.
    match compare_abi(&trait_sig.abi, &impl_sig.abi) {
        Cmp::Equal => {}
        Cmp::Different => diffs.push(Difference::Abi),
        Cmp::Unknown(r) => unknowns.push(r),
    }

    // Variadic.
    if trait_sig.variadic.is_some() != impl_sig.variadic.is_some() {
        diffs.push(Difference::Variadic);
    }

    // Parameters.
    let t_inputs: Vec<&FnArg> = trait_sig.inputs.iter().collect();
    let i_inputs: Vec<&FnArg> = impl_sig.inputs.iter().collect();
    if t_inputs.len() != i_inputs.len() {
        diffs.push(Difference::ParamCount {
            trait_count: t_inputs.len(),
            impl_count: i_inputs.len(),
        });
    } else {
        for (position, (t, i)) in t_inputs.iter().zip(i_inputs.iter()).enumerate() {
            match compare_fn_arg(t, i, &mapping, self_name) {
                Cmp::Equal => {}
                Cmp::Different => diffs.push(Difference::ParamType { position }),
                Cmp::Unknown(r) => unknowns.push(r),
            }
        }
    }

    // Return type.
    match compare_return_type(&trait_sig.output, &impl_sig.output, &mapping, self_name) {
        Cmp::Equal => {}
        Cmp::Different => diffs.push(Difference::ReturnType),
        Cmp::Unknown(r) => unknowns.push(r),
    }

    (diffs, unknowns)
}

fn compare_const(
    trait_item: &TraitItemConst,
    impl_item: &ImplItemConst,
) -> (Vec<Difference>, Vec<UnknownReason>) {
    let mut diffs = Vec::new();
    let mut unknowns = Vec::new();

    let (mapping, generic_diffs) = compare_generics(&trait_item.generics, &impl_item.generics);
    diffs.extend(generic_diffs);

    if let Some(mapping) = mapping {
        match compare_type(&trait_item.ty, &impl_item.ty, &mapping, None) {
            Cmp::Equal => {}
            Cmp::Different => diffs.push(Difference::ConstType),
            Cmp::Unknown(r) => unknowns.push(r),
        }
    }

    (diffs, unknowns)
}

fn compare_type_item(
    trait_item: &TraitItemType,
    impl_item: &ImplItemType,
) -> (Vec<Difference>, Vec<UnknownReason>) {
    let mut diffs = Vec::new();
    let mut unknowns = Vec::new();

    let (mapping, generic_diffs) = compare_generics(&trait_item.generics, &impl_item.generics);
    if !generic_diffs.is_empty() {
        diffs.push(Difference::AssocTypeGenerics);
    }

    if let Some(mapping) = mapping {
        // If the trait imposes bounds, the impl type must satisfy them, which
        // requires trait solving. The one statically decidable case is when the
        // impl provides exactly the default type, which is known to satisfy the
        // bounds.
        if !trait_item.bounds.is_empty() {
            let satisfies_default = match &trait_item.default {
                Some((_, default_ty)) => {
                    compare_type(default_ty, &impl_item.ty, &mapping, None).is_equal()
                }
                None => false,
            };
            if !satisfies_default {
                unknowns.push(UnknownReason::TraitBound);
            }
        }
    }

    (diffs, unknowns)
}

fn compare_safety(t: &Safety, i: &Safety) -> bool {
    matches!(
        (t, i),
        (Safety::Safe(_), Safety::Safe(_))
            | (Safety::Unsafe(_), Safety::Unsafe(_))
            | (Safety::Default, Safety::Default)
    )
}

fn compare_abi(t: &Option<Abi>, i: &Option<Abi>) -> Cmp {
    match (t, i) {
        (None, None) => Cmp::Equal,
        (Some(_), None) | (None, Some(_)) => Cmp::Different,
        (Some(ta), Some(ia)) => {
            let tn = ta
                .name
                .as_ref()
                .map(|l| l.value())
                .unwrap_or_else(|| "C".to_string());
            let in_ = ia
                .name
                .as_ref()
                .map(|l| l.value())
                .unwrap_or_else(|| "C".to_string());
            if tn == in_ {
                Cmp::Equal
            } else {
                Cmp::Different
            }
        }
    }
}

fn compare_receiver(t: Option<&Receiver>, i: Option<&Receiver>, mapping: &Mapping) -> Cmp {
    match (t, i) {
        (None, None) => Cmp::Equal,
        (Some(_), None) | (None, Some(_)) => Cmp::Different,
        (Some(tr), Some(ir)) => match (&tr.kind, &ir.kind) {
            (ReceiverKind::Value, ReceiverKind::Value) => Cmp::Equal,
            (ReceiverKind::Reference(_, tl, tm), ReceiverKind::Reference(_, il, im)) => {
                if tm.is_some() != im.is_some() {
                    return Cmp::Different;
                }
                compare_lifetime_opt(tl.as_ref(), il.as_ref(), mapping)
            }
            (ReceiverKind::Typed(_, tty), ReceiverKind::Typed(_, ity)) => {
                compare_type(tty, ity, mapping, None)
            }
            _ => Cmp::Different,
        },
    }
}

fn compare_fn_arg(t: &FnArg, i: &FnArg, mapping: &Mapping, self_name: Option<&str>) -> Cmp {
    match (t, i) {
        (FnArg::Receiver(_), FnArg::Receiver(_)) => {
            // Receivers are handled separately by compare_receiver.
            Cmp::Equal
        }
        (FnArg::Typed(tp), FnArg::Typed(ip)) => {
            compare_type(&tp.ty, &ip.ty, mapping, self_name)
        }
        _ => Cmp::Different,
    }
}

fn compare_return_type(
    t: &ReturnType,
    i: &ReturnType,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    match (t, i) {
        (ReturnType::Default, ReturnType::Default) => Cmp::Equal,
        (ReturnType::Default, ReturnType::Type(_, ty))
        | (ReturnType::Type(_, ty), ReturnType::Default) => {
            if is_unit(ty) {
                Cmp::Equal
            } else {
                Cmp::Different
            }
        }
        (ReturnType::Type(_, tty), ReturnType::Type(_, ity)) => {
            compare_type(tty, ity, mapping, self_name)
        }
    }
}

fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn compare_where(
    t: &Option<WhereClause>,
    i: &Option<WhereClause>,
    mapping: &Mapping,
) -> Cmp {
    match (t, i) {
        (None, None) => Cmp::Equal,
        (Some(_), None) | (None, Some(_)) => Cmp::Different,
        (Some(tw), Some(iw)) => {
            if tw.predicates.len() != iw.predicates.len() {
                return Cmp::Different;
            }
            for (tp, ip) in tw.predicates.iter().zip(iw.predicates.iter()) {
                match compare_where_predicate(tp, ip, mapping) {
                    Cmp::Equal => {}
                    Cmp::Different => return Cmp::Different,
                    Cmp::Unknown(r) => return Cmp::Unknown(r),
                }
            }
            Cmp::Equal
        }
    }
}

fn compare_where_predicate(t: &WherePredicate, i: &WherePredicate, mapping: &Mapping) -> Cmp {
    match (t, i) {
        (WherePredicate::Lifetime(tl), WherePredicate::Lifetime(il)) => {
            let c = compare_lifetime(&tl.lifetime, &il.lifetime, mapping);
            if !c.is_equal() {
                return c;
            }
            compare_lifetime_list(&tl.bounds, &il.bounds, mapping)
        }
        (WherePredicate::Type(tp), WherePredicate::Type(ip)) => {
            let extended = match (&tp.lifetimes, &ip.lifetimes) {
                (Some(tl), Some(il)) => match extend_bound_lifetimes(tl, il, mapping) {
                    Some(m) => m,
                    None => return Cmp::Unknown(UnknownReason::ComplexWhere),
                },
                (Some(_), None) | (None, Some(_)) => return Cmp::Different,
                (None, None) => mapping.clone(),
            };
            let c = compare_type(&tp.bounded_ty, &ip.bounded_ty, &extended, None);
            if !c.is_equal() {
                return c;
            }
            compare_bounds(&tp.bounds, &ip.bounds, &extended)
        }
        _ => Cmp::Different,
    }
}

/// Compare two types, renaming trait-side generic parameters to their impl-side
/// counterparts by position.
fn compare_type(t: &Type, i: &Type, mapping: &Mapping, self_name: Option<&str>) -> Cmp {
    match (t, i) {
        (Type::Paren(tp), _) => compare_type(&tp.elem, i, mapping, self_name),
        (_, Type::Paren(ip)) => compare_type(t, &ip.elem, mapping, self_name),
        (Type::Group(tg), _) => compare_type(&tg.elem, i, mapping, self_name),
        (_, Type::Group(ig)) => compare_type(t, &ig.elem, mapping, self_name),
        (Type::Never(_), Type::Never(_)) => Cmp::Equal,
        (Type::Infer(_), Type::Infer(_)) => Cmp::Equal,
        (Type::Infer(_), _) | (_, Type::Infer(_)) => {
            Cmp::Unknown(UnknownReason::ComplexType)
        }
        (Type::Path(tp), Type::Path(ip)) => compare_path_type(tp, ip, mapping, self_name),
        (Type::Reference(tr), Type::Reference(ir)) => {
            if tr.mutability.is_some() != ir.mutability.is_some() {
                return Cmp::Different;
            }
            let c = compare_lifetime_opt(tr.lifetime.as_ref(), ir.lifetime.as_ref(), mapping);
            if !c.is_equal() {
                return c;
            }
            compare_type(&tr.elem, &ir.elem, mapping, self_name)
        }
        (Type::Ptr(tp), Type::Ptr(ip)) => {
            if ptr_mut(tp) != ptr_mut(ip) {
                return Cmp::Different;
            }
            compare_type(&tp.elem, &ip.elem, mapping, self_name)
        }
        (Type::Array(ta), Type::Array(ia)) => {
            let c = compare_type(&ta.elem, &ia.elem, mapping, self_name);
            if !c.is_equal() {
                return c;
            }
            compare_expr(&ta.len, &ia.len, mapping)
        }
        (Type::Slice(ts), Type::Slice(is_)) => {
            compare_type(&ts.elem, &is_.elem, mapping, self_name)
        }
        (Type::Tuple(tt), Type::Tuple(it)) => {
            compare_type_list(&tt.elems, &it.elems, mapping, self_name)
        }
        (Type::FnPtr(tf), Type::FnPtr(if_)) => {
            compare_fn_ptr(tf, if_, mapping, self_name)
        }
        (Type::ImplTrait(ti), Type::ImplTrait(ii)) => {
            compare_bounds(&ti.bounds, &ii.bounds, mapping)
        }
        (Type::ImplTrait(_), _) | (_, Type::ImplTrait(_)) => {
            Cmp::Unknown(UnknownReason::ComplexType)
        }
        (Type::TraitObject(to), Type::TraitObject(io)) => {
            compare_bounds(&to.bounds, &io.bounds, mapping)
        }
        (Type::Macro(_), _) | (_, Type::Macro(_)) => {
            Cmp::Unknown(UnknownReason::TypeAlias)
        }
        (Type::Verbatim(_), _) | (_, Type::Verbatim(_)) => {
            Cmp::Unknown(UnknownReason::ComplexType)
        }
        _ => Cmp::Different,
    }
}

fn ptr_mut(p: &TypePtr) -> bool {
    matches!(p.mutability, PointerMutability::Mut(_))
}

fn compare_type_list(
    t: &Punctuated<Type, Token![,]>,
    i: &Punctuated<Type, Token![,]>,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    if t.len() != i.len() {
        return Cmp::Different;
    }
    for (tt, it) in t.iter().zip(i.iter()) {
        match compare_type(tt, it, mapping, self_name) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

fn compare_path_type(
    t: &TypePath,
    i: &TypePath,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    // `<T as Trait>::Assoc` requires normalizing the projection, which cannot
    // be done without rustc.
    if t.qself.is_some() || i.qself.is_some() {
        return Cmp::Unknown(UnknownReason::TypeAlias);
    }
    compare_path(&t.path, &i.path, mapping, self_name)
}

fn compare_path(
    t: &Path,
    i: &Path,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    if t.leading_colon.is_some() != i.leading_colon.is_some() {
        return Cmp::Different;
    }
    let t_segs: Vec<&PathSegment> = t.segments.iter().collect();
    let i_segs: Vec<&PathSegment> = i.segments.iter().collect();
    if t_segs.len() != i_segs.len() {
        return Cmp::Different;
    }
    // `T::Assoc` where `T` is a generic parameter (or `Self`) is an associated
    // projection that requires normalization.
    if t_segs.len() > 1 {
        let first = &t_segs[0].ident;
        let is_param = first == "Self"
            || mapping.map_type(first).is_some()
            || mapping.map_const(first).is_some();
        if is_param {
            return Cmp::Unknown(UnknownReason::TypeAlias);
        }
    }
    for (ts, is_) in t_segs.iter().zip(i_segs.iter()) {
        let ident_matches = if ts.ident == "Self" {
            is_.ident == "Self" || self_name.is_some_and(|n| is_.ident == n)
        } else if let Some(mapped) = mapping.map_type(&ts.ident) {
            &is_.ident == mapped
        } else if let Some(mapped) = mapping.map_const(&ts.ident) {
            &is_.ident == mapped
        } else {
            ts.ident == is_.ident
        };
        if !ident_matches {
            return Cmp::Different;
        }
        match compare_path_arguments(&ts.arguments, &is_.arguments, mapping, self_name) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

fn compare_path_arguments(
    t: &PathArguments,
    i: &PathArguments,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    match (t, i) {
        (PathArguments::None, PathArguments::None) => Cmp::Equal,
        (PathArguments::None, _) | (_, PathArguments::None) => Cmp::Different,
        (PathArguments::AngleBracketed(ta), PathArguments::AngleBracketed(ia)) => {
            if ta.colon2_token.is_some() != ia.colon2_token.is_some() {
                return Cmp::Different;
            }
            compare_generic_args(&ta.args, &ia.args, mapping, self_name)
        }
        (PathArguments::Parenthesized(tp), PathArguments::Parenthesized(ip)) => {
            let c = compare_named_args(&tp.inputs, &ip.inputs, mapping, self_name);
            if !c.is_equal() {
                return c;
            }
            compare_return_type(&tp.output, &ip.output, mapping, self_name)
        }
        _ => Cmp::Different,
    }
}

fn compare_generic_args(
    t: &Punctuated<GenericArgument, Token![,]>,
    i: &Punctuated<GenericArgument, Token![,]>,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    if t.len() != i.len() {
        return Cmp::Different;
    }
    for (ta, ia) in t.iter().zip(i.iter()) {
        match compare_generic_arg(ta, ia, mapping, self_name) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

fn compare_generic_arg(
    t: &GenericArgument,
    i: &GenericArgument,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    match (t, i) {
        (GenericArgument::Lifetime(tl), GenericArgument::Lifetime(il)) => {
            compare_lifetime(tl, il, mapping)
        }
        (GenericArgument::Type(tt), GenericArgument::Type(it)) => {
            compare_type(tt, it, mapping, self_name)
        }
        (GenericArgument::Const(te), GenericArgument::Const(ie)) => {
            compare_expr(te, ie, mapping)
        }
        (GenericArgument::AssocType(ta), GenericArgument::AssocType(ia)) => {
            if ta.ident != ia.ident {
                return Cmp::Different;
            }
            compare_type(&ta.ty, &ia.ty, mapping, self_name)
        }
        (GenericArgument::AssocConst(ta), GenericArgument::AssocConst(ia)) => {
            if ta.ident != ia.ident {
                return Cmp::Different;
            }
            compare_expr(&ta.value, &ia.value, mapping)
        }
        (GenericArgument::Constraint(ta), GenericArgument::Constraint(ia)) => {
            if ta.ident != ia.ident {
                return Cmp::Different;
            }
            compare_bounds(&ta.bounds, &ia.bounds, mapping)
        }
        _ => Cmp::Different,
    }
}

fn compare_fn_ptr(
    t: &TypeFnPtr,
    i: &TypeFnPtr,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    let extended = match (&t.lifetimes, &i.lifetimes) {
        (Some(tl), Some(il)) => match extend_bound_lifetimes(tl, il, mapping) {
            Some(m) => m,
            None => return Cmp::Unknown(UnknownReason::ComplexType),
        },
        (Some(_), None) | (None, Some(_)) => return Cmp::Different,
        (None, None) => mapping.clone(),
    };
    if t.unsafety.is_some() != i.unsafety.is_some() {
        return Cmp::Different;
    }
    match compare_abi(&t.abi, &i.abi) {
        Cmp::Equal => {}
        Cmp::Different => return Cmp::Different,
        Cmp::Unknown(r) => return Cmp::Unknown(r),
    }
    if t.variadic.is_some() != i.variadic.is_some() {
        return Cmp::Different;
    }
    let c = compare_named_args(&t.inputs, &i.inputs, &extended, self_name);
    if !c.is_equal() {
        return c;
    }
    compare_return_type(&t.output, &i.output, &extended, self_name)
}

fn compare_named_args(
    t: &Punctuated<NamedArg, Token![,]>,
    i: &Punctuated<NamedArg, Token![,]>,
    mapping: &Mapping,
    self_name: Option<&str>,
) -> Cmp {
    if t.len() != i.len() {
        return Cmp::Different;
    }
    for (ta, ia) in t.iter().zip(i.iter()) {
        match compare_type(&ta.ty, &ia.ty, mapping, self_name) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

/// Extend a mapping with the higher-ranked lifetimes of a `for<...>` binder,
/// if the two binders line up positionally.
fn extend_bound_lifetimes(
    t: &BoundLifetimes,
    i: &BoundLifetimes,
    mapping: &Mapping,
) -> Option<Mapping> {
    let t_params: Vec<&GenericParam> = t.lifetimes.iter().collect();
    let i_params: Vec<&GenericParam> = i.lifetimes.iter().collect();
    if t_params.len() != i_params.len() {
        return None;
    }
    let mut extended = mapping.clone();
    for (tp, ip) in t_params.iter().zip(i_params.iter()) {
        match (tp, ip) {
            (GenericParam::Lifetime(tl), GenericParam::Lifetime(il)) => {
                extended
                    .lifetimes
                    .push((tl.lifetime.ident.clone(), il.lifetime.ident.clone()));
            }
            _ => return None,
        }
    }
    Some(extended)
}

fn compare_bounds(
    t: &Punctuated<TypeParamBound, Token![+]>,
    i: &Punctuated<TypeParamBound, Token![+]>,
    mapping: &Mapping,
) -> Cmp {
    if t.len() != i.len() {
        return Cmp::Different;
    }
    for (tb, ib) in t.iter().zip(i.iter()) {
        match compare_bound(tb, ib, mapping) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

fn compare_bound(t: &TypeParamBound, i: &TypeParamBound, mapping: &Mapping) -> Cmp {
    match (t, i) {
        (TypeParamBound::Trait(tb), TypeParamBound::Trait(ib)) => {
            if tb.maybe.is_some() != ib.maybe.is_some() {
                return Cmp::Different;
            }
            if tb.paren_token.is_some() != ib.paren_token.is_some() {
                return Cmp::Different;
            }
            let extended = match (&tb.lifetimes, &ib.lifetimes) {
                (Some(tl), Some(il)) => match extend_bound_lifetimes(tl, il, mapping) {
                    Some(m) => m,
                    None => return Cmp::Unknown(UnknownReason::ComplexType),
                },
                (Some(_), None) | (None, Some(_)) => return Cmp::Different,
                (None, None) => mapping.clone(),
            };
            compare_path(&tb.path, &ib.path, &extended, None)
        }
        (TypeParamBound::Lifetime(tl), TypeParamBound::Lifetime(il)) => {
            compare_lifetime(tl, il, mapping)
        }
        (TypeParamBound::PreciseCapture(tp), TypeParamBound::PreciseCapture(ip)) => {
            compare_precise_capture(tp, ip, mapping)
        }
        (TypeParamBound::Verbatim(_), _) | (_, TypeParamBound::Verbatim(_)) => {
            Cmp::Unknown(UnknownReason::ComplexType)
        }
        _ => Cmp::Different,
    }
}

fn compare_precise_capture(
    t: &crate::generics::PreciseCapture,
    i: &crate::generics::PreciseCapture,
    mapping: &Mapping,
) -> Cmp {
    if t.params.len() != i.params.len() {
        return Cmp::Different;
    }
    for (tp, ip) in t.params.iter().zip(i.params.iter()) {
        match (tp, ip) {
            (crate::generics::CapturedParam::Lifetime(tl), crate::generics::CapturedParam::Lifetime(il)) => {
                let expected = mapping.map_lifetime(&tl.ident).unwrap_or(&tl.ident);
                if expected != &il.ident {
                    return Cmp::Different;
                }
            }
            (crate::generics::CapturedParam::Ident(tid), crate::generics::CapturedParam::Ident(iid)) => {
                let expected = mapping
                    .map_type(tid)
                    .or_else(|| mapping.map_const(tid))
                    .unwrap_or(tid);
                if expected != iid {
                    return Cmp::Different;
                }
            }
            _ => return Cmp::Unknown(UnknownReason::ComplexType),
        }
    }
    Cmp::Equal
}

fn compare_lifetime_list(
    t: &Punctuated<Lifetime, Token![+]>,
    i: &Punctuated<Lifetime, Token![+]>,
    mapping: &Mapping,
) -> Cmp {
    if t.len() != i.len() {
        return Cmp::Different;
    }
    for (tl, il) in t.iter().zip(i.iter()) {
        match compare_lifetime(tl, il, mapping) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Unknown(r) => return Cmp::Unknown(r),
        }
    }
    Cmp::Equal
}

fn compare_lifetime(t: &Lifetime, i: &Lifetime, mapping: &Mapping) -> Cmp {
    let expected = mapping.map_lifetime(&t.ident).unwrap_or(&t.ident);
    if expected == &i.ident {
        Cmp::Equal
    } else {
        Cmp::Different
    }
}

fn compare_lifetime_opt(
    t: Option<&Lifetime>,
    i: Option<&Lifetime>,
    mapping: &Mapping,
) -> Cmp {
    match (t, i) {
        (None, None) => Cmp::Equal,
        (Some(tl), Some(il)) => compare_lifetime(tl, il, mapping),
        (Some(_), None) | (None, Some(_)) => Cmp::Different,
    }
}

fn compare_expr(t: &Expr, i: &Expr, mapping: &Mapping) -> Cmp {
    match (t, i) {
        (Expr::Lit(tl), Expr::Lit(il)) => {
            if lit_eq(&tl.lit, &il.lit) {
                Cmp::Equal
            } else {
                Cmp::Different
            }
        }
        (Expr::Path(tp), Expr::Path(ip)) => {
            if tp.qself.is_some() || ip.qself.is_some() {
                return Cmp::Unknown(UnknownReason::ComplexExpr);
            }
            compare_path(&tp.path, &ip.path, mapping, None)
        }
        _ => Cmp::Unknown(UnknownReason::ComplexExpr),
    }
}

fn lit_eq(t: &crate::lit::Lit, i: &crate::lit::Lit) -> bool {
    use crate::lit::Lit;
    match (t, i) {
        (Lit::Str(a), Lit::Str(b)) => a.value() == b.value(),
        (Lit::ByteStr(a), Lit::ByteStr(b)) => a.value() == b.value(),
        (Lit::CStr(a), Lit::CStr(b)) => a.value() == b.value(),
        (Lit::Byte(a), Lit::Byte(b)) => a.value() == b.value(),
        (Lit::Char(a), Lit::Char(b)) => a.value() == b.value(),
        (Lit::Int(a), Lit::Int(b)) => a.base10_digits() == b.base10_digits(),
        (Lit::Float(a), Lit::Float(b)) => a.base10_digits() == b.base10_digits(),
        (Lit::Bool(a), Lit::Bool(b)) => a.value() == b.value(),
        (Lit::Verbatim(a), Lit::Verbatim(b)) => a.to_string() == b.to_string(),
        _ => false,
    }
}

impl core::fmt::Display for ContractReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        for item in &self.missing {
            if !first {
                writeln!(f)?;
            }
            first = false;
            write!(
                f,
                "missing {} item `{}` (trait span: {:?}, impl span: {:?})",
                item.kind, item.name, item.trait_span, item.impl_span
            )?;
        }
        for mismatch in &self.mismatches {
            if !first {
                writeln!(f)?;
            }
            first = false;
            write!(
                f,
                "signature mismatch in {} item `{}` (trait span: {:?}, impl span: {:?}): ",
                mismatch.kind, mismatch.name, mismatch.trait_span, mismatch.impl_span
            )?;
            for (i, diff) in mismatch.differences.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{diff}")?;
            }
        }
        for unknown in &self.unknown {
            if !first {
                writeln!(f)?;
            }
            first = false;
            match &unknown.name {
                Some(name) => write!(
                    f,
                    "cannot statically decide item `{}` (trait span: {:?}, impl span: {:?}): {}",
                    name, unknown.trait_span, unknown.impl_span, unknown.reason
                )?,
                None => write!(
                    f,
                    "cannot statically decide item (trait span: {:?}, impl span: {:?}): {}",
                    unknown.trait_span, unknown.impl_span, unknown.reason
                )?,
            }
        }
        if first {
            write!(f, "no issues")?;
        }
        Ok(())
    }
}

impl core::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ItemKind::Fn => f.write_str("fn"),
            ItemKind::Const => f.write_str("const"),
            ItemKind::Type => f.write_str("type"),
        }
    }
}

impl core::fmt::Display for Difference {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Difference::GenericCount {
                trait_count,
                impl_count,
            } => write!(
                f,
                "generic count differs (trait has {trait_count}, impl has {impl_count})"
            ),
            Difference::GenericKind { position } => {
                write!(f, "generic parameter at position {position} has a different kind")
            }
            Difference::GenericBound { position } => {
                write!(f, "bounds of generic parameter at position {position} differ")
            }
            Difference::WherePredicate => f.write_str("where predicate differs"),
            Difference::Receiver => f.write_str("receiver (`self`) differs"),
            Difference::Async => f.write_str("`async` differs"),
            Difference::Unsafe => f.write_str("`unsafe` differs"),
            Difference::Abi => f.write_str("ABI (`extern`) differs"),
            Difference::Variadic => f.write_str("variadic arguments differ"),
            Difference::ParamCount {
                trait_count,
                impl_count,
            } => write!(
                f,
                "parameter count differs (trait has {trait_count}, impl has {impl_count})"
            ),
            Difference::ParamType { position } => {
                write!(f, "parameter at position {position} has a different type")
            }
            Difference::ReturnType => f.write_str("return type differs"),
            Difference::ConstType => f.write_str("associated constant type differs"),
            Difference::AssocTypeGenerics => {
                f.write_str("associated type generics differ")
            }
        }
    }
}

impl core::fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UnknownReason::TraitMacro => {
                f.write_str("trait side contains a macro or verbatim item")
            }
            UnknownReason::ImplMacro => {
                f.write_str("impl side contains a macro or verbatim item")
            }
            UnknownReason::TypeAlias => f.write_str("type alias would need expanding"),
            UnknownReason::TraitBound => f.write_str("trait bound would need solving"),
            UnknownReason::ComplexWhere => f.write_str("where clause is too complex"),
            UnknownReason::ComplexExpr => f.write_str("expression is too complex"),
            UnknownReason::ComplexType => f.write_str("type cannot be compared"),
        }
    }
}
