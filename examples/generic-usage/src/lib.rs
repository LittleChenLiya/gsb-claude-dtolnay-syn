//! Reusable analysis of how an item uses the generics it declares.
//!
//! Given a parsed [`syn::Item`], [`analyze_item`] lists every type, lifetime
//! and const generic parameter that the item declares and records every place
//! at which each one is referenced: field types, function signatures, `where`
//! predicates, default values and inline `const` blocks. Each record carries
//! both the declaration site and the reference sites, rendered as
//! `line:column` coordinates, so a report can point straight back at the
//! source positions.
//!
//! # Scope and shadowing
//!
//! Parameters are resolved against a stack of scopes. A nested item (an impl,
//! a trait, a function item, an impl item, ...) pushes its own generics on top
//! of the stack, so a nested parameter with the same name as an outer one
//! correctly shadows the outer one, and references resolve to the innermost
//! declaration. Higher-ranked lifetime binders (`for<'a>`) introduce their own
//! scope as well.
//!
//! # Unknown scopes
//!
//! Token streams that `syn` does not descend into are treated as *opaque*:
//! macro invocations and non-builtin attributes (such as `derive(...)` proc
//! macros) could expand to code using any in-scope parameter, so parameters
//! of an item containing such a scope are reported as possibly used
//! ([`ParamReport::possibly_used`]) instead of being confidently listed as
//! unused. A parameter is reported as unused only when every relevant scope
//! was inspected.
//!
//! The analysis is a read-only [`syn::visit::Visit`] traversal and works on
//! tokens parsed outside of a procedural macro as well.

use proc_macro2::Span;
use std::collections::HashSet;
use std::fmt;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Attribute, BoundLifetimes, ExprConst, Field, ForeignItemFn, ForeignItemType, GenericArgument,
    GenericParam, Generics, ImplItemConst, ImplItemFn, ImplItemType, Item, ItemConst, ItemEnum,
    ItemFn, ItemForeignMod, ItemImpl, ItemMacro, ItemStatic, ItemStruct, ItemTrait, ItemTraitAlias,
    ItemType, ItemUnion, Lifetime, Path, PredicateType, ReturnType, Signature, TraitBound,
    TraitItemConst, TraitItemFn, TraitItemType, TypeArray, TypeFnPtr, WhereClause, WherePredicate,
};

/// Kind of a generic parameter declared by an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamKind {
    /// A type parameter, such as the `T` in `<T>`.
    Type,
    /// A lifetime parameter, such as the `'a` in `<'a>`.
    Lifetime,
    /// A const generic parameter, such as the `N` in `const N: usize`.
    Const,
}

impl fmt::Display for ParamKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            ParamKind::Type => "type",
            ParamKind::Lifetime => "lifetime",
            ParamKind::Const => "const",
        })
    }
}

/// A source coordinate (`line:column`, both 1-based) together with a label
/// describing what syntactic position the coordinate points at.
#[derive(Clone, Debug)]
pub struct Site {
    /// `line:column` of the first token (column is 1-based).
    pub at: (usize, usize),
    /// Human readable role of this position, e.g. `field "items"`.
    pub label: String,
}

impl Site {
    fn new(span: Span, label: impl Into<String>) -> Self {
        let start = span.start();
        Site {
            at: (start.line, start.column + 1),
            label: label.into(),
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{} ({})", self.at.0, self.at.1, self.label)
    }
}

/// Usage report for a single declared generic parameter.
#[derive(Clone, Debug)]
pub struct ParamReport {
    /// Type, lifetime or const.
    pub kind: ParamKind,
    /// Parameter name *without* the leading apostrophe for lifetimes.
    pub name: String,
    /// Where the parameter was declared (the owner's generic parameter list).
    pub declared_at: Site,
    /// Every inspected position that references it, apart from the
    /// declaration itself.
    pub references: Vec<Site>,
    /// True if some opaque scope (a macro or non-builtin attribute) within the
    /// parameter's item could potentially be using it.
    pub unknown_scope: bool,
}

impl ParamReport {
    /// A parameter is used when it was observed at least once.
    pub fn is_used(&self) -> bool {
        !self.references.is_empty()
    }

    /// A parameter is *confidently* unused only when no opaque scope in its
    /// item might be referencing it. When
    /// [`unknown_scope`](ParamReport::unknown_scope) is true the parameter is
    /// reported as possibly used instead.
    pub fn is_unused(&self) -> bool {
        !self.is_used() && !self.unknown_scope
    }

    /// True when no reference was observed but an opaque scope such as a macro
    /// expansion could be using the parameter.
    pub fn possibly_used(&self) -> bool {
        !self.is_used() && self.unknown_scope
    }

    /// Labels of every reference site, in traversal order.
    pub fn reference_labels(&self) -> impl Iterator<Item = &str> {
        self.references.iter().map(|site| site.label.as_str())
    }
}

/// Report for one generic owner (an item or a nested generic scope).
#[derive(Clone, Debug)]
pub struct ItemReport {
    /// Name of the owning item: a struct/function/trait name, or
    /// `impl Trait for Type` for an impl block.
    pub owner: String,
    /// Reports for the parameters declared directly on this owner, in source
    /// order.
    pub params: Vec<ParamReport>,
    /// Reports for nested generic owners discovered inside this item.
    pub nested: Vec<ItemReport>,
}

impl ItemReport {
    /// Parameters of this owner that are confidently unused.
    pub fn unused_params(&self) -> impl Iterator<Item = &ParamReport> {
        self.params.iter().filter(|p| p.is_unused())
    }

    /// Parameters with no observed use but with an opaque scope that might use
    /// them: these must never be claimed as unused.
    pub fn possibly_used_params(&self) -> impl Iterator<Item = &ParamReport> {
        self.params.iter().filter(|p| p.possibly_used())
    }

    /// Find the report for a parameter declared directly on this owner.
    pub fn param(&self, name: &str) -> Option<&ParamReport> {
        self.params.iter().find(|p| p.name == name)
    }

    /// Look up a parameter in this owner or any of its nested owners.
    pub fn find_param(&self, name: &str) -> Option<&ParamReport> {
        self.param(name)
            .or_else(|| self.nested.iter().find_map(|n| n.find_param(name)))
    }

    /// Find the nested owner report with the given owner name.
    pub fn nested_owner(&self, name: &str) -> Option<&ItemReport> {
        self.nested.iter().find(|n| n.owner == name)
    }
}

/// Analyze the generic parameter usage of a single item.
///
/// Nested generic owners (impl blocks, methods, associated items, ...) are
/// reported recursively under [`ItemReport::nested`].
pub fn analyze_item(item: &Item) -> ItemReport {
    let mut analyzer = Analyzer::new();
    analyzer.visit_item(item);
    let mut roots = analyzer.finish();
    assert_eq!(
        roots.len(),
        1,
        "visit_item produces exactly one root report"
    );
    roots.remove(0)
}

/// Analyze every top-level item in a parsed file.
pub fn analyze_file(file: &syn::File) -> Vec<ItemReport> {
    let mut analyzer = Analyzer::new();
    analyzer.visit_file(file);
    analyzer.finish()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Namespace {
    Type,
    Value,
    Lifetime,
}

struct Frame {
    /// Index into `results` of the owner this frame belongs to:
    /// - Some for a generic owner (also inherited by foreign-block and
    ///   binder frames),
    /// - None at file scope.
    owner_report: Option<usize>,
    /// True for the frame that itself declares a generic owner; false for
    /// pass-through frames (extern blocks, binders, file-scope frames).
    is_owner: bool,
    /// Set when an opaque scope (macro/non-builtin attribute) was encountered
    /// while this frame was active, including before params were declared.
    opaque: bool,
    /// (name, parameter index) pairs; index usize::MAX marks shadowing locals.
    types: Vec<(String, usize)>,
    values: Vec<(String, usize)>,
    lifetimes: Vec<(String, usize)>,
}

struct Analyzer {
    results: Vec<ItemReport>,
    /// Parent owner report for each (nested) report; root reports have none.
    parent_of: Vec<Option<usize>>,
    frames: Vec<Frame>,
    /// Describes the syntactic position currently being traversed.
    places: Vec<String>,
}

impl Analyzer {
    fn new() -> Self {
        Analyzer {
            results: Vec::new(),
            parent_of: Vec::new(),
            frames: Vec::new(),
            places: Vec::new(),
        }
    }

    fn push_report(&mut self, owner: String, parent: Option<usize>) -> usize {
        let idx = self.results.len();
        self.results.push(ItemReport {
            owner,
            params: Vec::new(),
            nested: Vec::new(),
        });
        self.parent_of.push(parent);
        idx
    }

    /// Nearest open generic-owner report, if any.
    /// The nearest enclosing frame that *declares* an owner, skipping
    /// pass-through frames (extern blocks, lifetime binders).
    fn current_owner(&self) -> Option<usize> {
        self.frames
            .iter()
            .rev()
            .find(|frame| frame.is_owner)
            .and_then(|frame| frame.owner_report)
    }

    /// Detach nested reports into a tree and return the root reports.
    fn finish(mut self) -> Vec<ItemReport> {
        fn empty_report() -> ItemReport {
            ItemReport {
                owner: String::new(),
                params: Vec::new(),
                nested: Vec::new(),
            }
        }
        // Children are always declared after their parents, so moving in
        // reverse order attaches fully-assembled subtrees first.
        for idx in (0..self.results.len()).rev() {
            if let Some(parent) = self.parent_of[idx] {
                let child = std::mem::replace(&mut self.results[idx], empty_report());
                self.results[parent].nested.push(child);
            }
        }
        self.results
            .iter_mut()
            .enumerate()
            .filter(|(idx, _)| self.parent_of[*idx].is_none())
            .map(|(_, report)| std::mem::replace(report, empty_report()))
            .collect()
    }

    fn report(&mut self, idx: usize) -> &mut ItemReport {
        &mut self.results[idx]
    }

    fn declare(&mut self, report_idx: usize, kind: ParamKind, name: String, span: Span) -> usize {
        let param_idx = self.report(report_idx).params.len();
        let unknown_scope = self
            .frames
            .iter()
            .rev()
            .find(|frame| frame.owner_report == Some(report_idx))
            .is_some_and(|frame| frame.opaque);
        self.report(report_idx).params.push(ParamReport {
            kind,
            name: name.clone(),
            declared_at: Site::new(span, format!("declaration of parameter {name}")),
            references: Vec::new(),
            unknown_scope,
        });
        let frame = self.frames.last_mut().expect("scope must be open");
        let entry = (name, param_idx);
        match kind {
            ParamKind::Type => frame.types.push(entry),
            ParamKind::Const => frame.values.push(entry),
            ParamKind::Lifetime => frame.lifetimes.push(entry),
        }
        param_idx
    }

    /// Resolve a name from the innermost scope outward. Returns the report
    /// index and the parameter's index within that report.
    fn resolve(&self, ns: Namespace, name: &str) -> Option<(usize, usize)> {
        for frame in self.frames.iter().rev() {
            let candidates: &[(String, usize)] = match ns {
                Namespace::Type => &frame.types,
                Namespace::Value => &frame.values,
                Namespace::Lifetime => &frame.lifetimes,
            };
            if let Some((_, param_idx)) = candidates.iter().rev().find(|(n, _)| n == name) {
                return match frame.owner_report {
                    Some(report_idx) if *param_idx != usize::MAX => Some((report_idx, *param_idx)),
                    // A binder lifetime or a shadowing local: the name is
                    // bound, but no parameter record receives the reference.
                    _ => None,
                };
            }
        }
        None
    }

    fn record(&mut self, ns: Namespace, name: &str, span: Span) {
        if let Some((report_idx, param_idx)) = self.resolve(ns, name) {
            let label = self
                .places
                .last()
                .cloned()
                .unwrap_or_else(|| "item".to_owned());
            self.report(report_idx).params[param_idx]
                .references
                .push(Site::new(span, label));
        }
    }

    /// Mark every parameter in every currently open owner scope as having an
    /// unknown scope: opaque tokens may expand to references to them.
    fn mark_opaque(&mut self) {
        let affected: Vec<usize> = self
            .frames
            .iter_mut()
            .filter_map(|frame| {
                frame.opaque = true;
                frame.owner_report
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for report_idx in affected {
            for param in &mut self.report(report_idx).params {
                param.unknown_scope = true;
            }
        }
    }

    fn push_frame(
        &mut self,
        owner_report: Option<usize>,
        inherited: Option<usize>,
        is_owner: bool,
    ) {
        self.frames.push(Frame {
            owner_report: own_or_inherited(owner_report, inherited),
            is_owner,
            opaque: false,
            types: Vec::new(),
            values: Vec::new(),
            lifetimes: Vec::new(),
        });
    }

    fn push_binder(&mut self) {
        let owner = self.current_owner();
        self.push_frame(None, owner, false);
    }

    fn pop_scope(&mut self) {
        self.frames.pop();
    }
}

/// Attributes whose expansion cannot name the annotated item's generics.
///
/// `derive` itself is *not* in the list: a derive proc-macro receives the
/// whole item and may emit code that uses any of its parameters.
fn attr_is_builtin(attr: &Attribute) -> bool {
    const BUILTIN: &[&str] = &[
        "cfg",
        "cfg_attr",
        "automatically_derived",
        "allow",
        "warn",
        "deny",
        "forbid",
        "deprecated",
        "doc",
        "must_use",
        "non_exhaustive",
        "repr",
        "crate_type",
        "no_std",
        "no_core",
        "no_main",
        "no_start",
        "path",
        "recursion_limit",
        "type_length_limit",
        "feature",
        "global_allocator",
        "windows_subsystem",
        "no_mangle",
        "export_name",
        "link_section",
        "inline",
        "cold",
        "link",
        "link_name",
        "test",
        "ignore",
        "should_panic",
        "proc_macro",
        "proc_macro_derive",
        "proc_macro_attribute",
        "marker",
        "fundamental",
        "maybe_unused",
    ];
    let path = attr.meta.path();
    match path.segments.first() {
        Some(first) if BUILTIN.contains(&first.ident.to_string().as_str()) => true,
        // Inert tool namespaces such as `clippy::*` and `rustfmt::*`.
        Some(first) if path.segments.len() > 1 => {
            matches!(first.ident.to_string().as_str(), "clippy" | "rustfmt")
        }
        _ => false,
    }
}

/// Walk a function signature in the context already opened by its owner.
fn own_or_inherited(own: Option<usize>, inherited: Option<usize>) -> Option<usize> {
    own.or(inherited)
}

fn visit_signature(this: &mut Analyzer, sig: &Signature) {
    for input in &sig.inputs {
        match input {
            syn::FnArg::Receiver(receiver) => {
                this.places.push("method receiver".to_owned());
                match &receiver.kind {
                    syn::ReceiverKind::Reference(_, lifetime, _) => {
                        if let Some(lifetime) = lifetime {
                            this.visit_lifetime(lifetime);
                        }
                    }
                    syn::ReceiverKind::Typed(_, ty) => this.visit_type(ty),
                    syn::ReceiverKind::Value => {}
                    _ => {}
                }
                this.places.pop();
            }
            syn::FnArg::Typed(pat) => {
                let name = match &*pat.pat {
                    syn::Pat::Ident(ident) => ident.ident.to_string(),
                    _ => "_".to_owned(),
                };
                this.places.push(format!("argument `{name}`"));
                this.visit_type(&pat.ty);
                this.places.pop();
            }
        }
    }
    if let ReturnType::Type(_, output) = &sig.output {
        this.places.push("return type".to_owned());
        this.visit_type(output);
        this.places.pop();
    }
}

fn compact(tokens: proc_macro2::TokenStream) -> String {
    tokens.to_string().replace(' ', "")
}

fn impl_owner_name(node: &ItemImpl) -> String {
    use quote::ToTokens;
    let self_ty = compact(node.self_ty.to_token_stream());
    match &node.trait_ {
        Some((trait_path, _)) => {
            let trait_path = compact(trait_path.to_token_stream());
            format!("impl {trait_path} for {self_ty}")
        }
        None => format!("impl {self_ty}"),
    }
}

impl Analyzer {
    /// Register the parameters of a generics list and visit their bounds and
    /// defaults, so cross-references such as `T: Into<U>` or
    /// `const M: usize = N` resolve within the owner's own scope.
    fn declare_generics(&mut self, generics: &Generics, report_idx: usize) {
        for param in &generics.params {
            match param {
                GenericParam::Type(param) => {
                    self.declare(
                        report_idx,
                        ParamKind::Type,
                        param.ident.to_string(),
                        param.ident.span(),
                    );
                    let label = format!("bounds of parameter {}", param.ident);
                    self.places.push(label);
                    for bound in &param.bounds {
                        self.visit_type_param_bound(bound);
                    }
                    self.places.pop();
                    if let Some((_, default)) = &param.default {
                        let label = format!("default type of parameter {}", param.ident);
                        self.places.push(label);
                        self.visit_type(default);
                        self.places.pop();
                    }
                }
                GenericParam::Lifetime(param) => {
                    self.declare(
                        report_idx,
                        ParamKind::Lifetime,
                        param.lifetime.ident.to_string(),
                        param.lifetime.span(),
                    );
                    // A bound on a declared lifetime, as in `'a: 'b`, uses the
                    // lifetimes on the right side.
                    let label = format!("bounds of lifetime parameter {}", param.lifetime.ident);
                    self.places.push(label);
                    for bound in &param.bounds {
                        self.visit_lifetime(bound);
                    }
                    self.places.pop();
                }
                GenericParam::Const(param) => {
                    self.declare(
                        report_idx,
                        ParamKind::Const,
                        param.ident.to_string(),
                        param.ident.span(),
                    );
                    let label = format!("type of const parameter {}", param.ident);
                    self.places.push(label);
                    self.visit_type(&param.ty);
                    self.places.pop();
                    if let Some((_, default)) = &param.default {
                        let label = format!("default value of const parameter {}", param.ident);
                        self.places.push(label);
                        self.visit_expr(default);
                        self.places.pop();
                    }
                }
            }
        }
    }

    fn visit_where(&mut self, where_clause: &WhereClause) {
        self.places.push("where clause".to_owned());
        for predicate in &where_clause.predicates {
            match predicate {
                WherePredicate::Lifetime(predicate) => {
                    self.visit_lifetime(&predicate.lifetime);
                    for bound in &predicate.bounds {
                        self.visit_lifetime(bound);
                    }
                }
                WherePredicate::Type(predicate) => {
                    self.visit_predicate_type_with_binder(predicate);
                }
                _ => {}
            }
        }
        self.places.pop();
    }

    /// Visit a for-bounded type predicate such as
    /// `for<'c> Foo<'c>: Trait<'c>`, keeping the binder lifetime scope open
    /// over both sides of the predicate.
    fn visit_predicate_type_with_binder(&mut self, node: &PredicateType) {
        let depth = self.frames.len();
        if let Some(lifetimes) = &node.lifetimes {
            self.push_binder_scope(lifetimes);
        }
        self.visit_type(&node.bounded_ty);
        for bound in &node.bounds {
            self.visit_type_param_bound(bound);
        }
        while self.frames.len() > depth {
            self.pop_scope();
        }
    }

    fn push_binder_scope(&mut self, lifetimes: &BoundLifetimes) {
        self.push_binder();
        for param in &lifetimes.lifetimes {
            if let GenericParam::Lifetime(param) = param {
                let frame = self.frames.last_mut().unwrap();
                frame
                    .lifetimes
                    .push((param.lifetime.ident.to_string(), usize::MAX));
                for bound in &param.bounds {
                    self.visit_lifetime(bound);
                }
            }
        }
    }
}

impl Analyzer {
    fn visit_fields(&mut self, fields: &syn::Fields) {
        match fields {
            syn::Fields::Named(fields) => {
                for field in &fields.named {
                    self.visit_field_labeled(field, None);
                }
            }
            syn::Fields::Unnamed(fields) => {
                for (index, field) in fields.unnamed.iter().enumerate() {
                    self.visit_field_labeled(field, Some(index));
                }
            }
            syn::Fields::Unit => {}
        }
    }

    fn visit_field_labeled(&mut self, field: &Field, index: Option<usize>) {
        self.visit_attrs(&field.attrs);
        let label = match (&field.ident, index) {
            (Some(ident), _) => format!("field \"{ident}\""),
            (None, Some(index)) => format!("tuple field {index}"),
            (None, None) => "unnamed field".to_owned(),
        };
        self.places.push(label);
        self.visit_type(&field.ty);
        self.places.pop();
    }

    /// Mark active owner scopes opaque if any of the attributes is non-builtin.
    fn visit_attrs(&mut self, attrs: &[Attribute]) {
        if attrs.iter().any(|attr| !attr_is_builtin(attr)) {
            self.mark_opaque();
        }
    }

    /// Common driver for every node that owns a generics list.
    fn generic_owner(
        &mut self,
        owner: String,
        generics: &Generics,
        attrs: &[Attribute],
        body: impl FnOnce(&mut Self),
    ) {
        let parent = self.current_owner();
        let report_idx = self.push_report(owner, parent);
        self.push_frame(Some(report_idx), parent, true);

        // Attributes are inspected once the owner frame is open so that opaque
        // helper attributes such as derive(...) can mark this owner.
        self.visit_attrs(attrs);
        self.declare_generics(generics, report_idx);
        if let Some(where_clause) = &generics.where_clause {
            self.visit_where(where_clause);
        }

        body(self);

        self.pop_scope();
    }

    /// Scan the segments of a path for references in the given namespace.
    ///
    /// With qualified self (`<T as Trait>::Assoc`), segments before
    /// `qself_position` belong to the separately visited `qself.ty`; a leading
    /// `::` means the path is crate- or extern-rooted and can never resolve to
    /// a generic parameter.
    fn scan_path(&mut self, path: &Path, ns: Namespace, qself_position: Option<usize>) {
        for (i, segment) in path.segments.iter().enumerate() {
            // With qualified self the only possible generic-parameter segment
            // is the first one that follows the qself type; later segments are
            // trait or associated-item names.
            // `qself_position` is the index of the trait segment; the qself
            // type itself occupies the segments before it and is visited
            // separately. Everything from the trait onward is a trait or
            // associated-item name and therefore not a generic parameter.
            let candidate = match qself_position {
                None => true,
                Some(position) => i < position,
            };
            if candidate && path.leading_colon.is_none() {
                let name = segment.ident.to_string();
                let span = segment.ident.span();
                match ns {
                    Namespace::Type => {
                        // Type position first resolves type parameters; a bare
                        // single-segment identity can also be a const generic
                        // (const generics are parsed as `Type` arguments when
                        // written without a block).
                        if self.resolve(Namespace::Type, &name).is_none()
                            && segment.arguments.is_none()
                        {
                            self.record(Namespace::Value, &name, span);
                        } else {
                            self.record(Namespace::Type, &name, span);
                        }
                    }
                    Namespace::Value => self.record(Namespace::Value, &name, span),
                    Namespace::Lifetime => unreachable!(),
                }
            }
            match &segment.arguments {
                syn::PathArguments::None => {}
                syn::PathArguments::AngleBracketed(args) => {
                    for arg in &args.args {
                        self.visit_generic_argument(arg);
                    }
                }
                syn::PathArguments::Parenthesized(args) => {
                    for input in &args.inputs {
                        self.visit_named_arg(input);
                    }
                    if let ReturnType::Type(_, output) = &args.output {
                        self.visit_type(output);
                    }
                }
            }
        }
    }

    fn visit_generic_argument(&mut self, arg: &GenericArgument) {
        match arg {
            GenericArgument::Lifetime(lifetime) => self.visit_lifetime(lifetime),
            GenericArgument::Type(ty) => self.visit_type(ty),
            GenericArgument::Const(expr) => {
                // Inherit the surrounding place (self type, field type, ...).
                self.visit_expr(expr);
            }
            GenericArgument::AssocType(binding) => {
                if let Some(generics) = &binding.generics {
                    for arg in &generics.args {
                        self.visit_generic_argument(arg);
                    }
                }
                let label = format!("associated type binding `{}`", binding.ident);
                self.places.push(label);
                self.visit_type(&binding.ty);
                self.places.pop();
            }
            GenericArgument::AssocConst(binding) => self.visit_expr(&binding.value),
            GenericArgument::Constraint(constraint) => {
                for bound in &constraint.bounds {
                    self.visit_type_param_bound(bound);
                }
            }
            _ => {}
        }
    }

    /// A local `const X` or `static X` item shadows in-scope type and value
    /// names within its initializer.
    fn shadow_local(&mut self, name: &str) {
        let frame = self.frames.last_mut().expect("item must be within a scope");
        frame.types.push((name.to_owned(), usize::MAX));
        frame.values.push((name.to_owned(), usize::MAX));
    }

    fn unshadow_local(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        frame.types.pop();
        frame.values.pop();
    }
}

impl<'ast> Analyzer {
    fn visit_static_body(&mut self, ident: &syn::Ident, ty: &syn::Type, expr: &syn::Expr) {
        let opened = self.frames.is_empty();
        if opened {
            self.push_frame(None, None, false);
        }
        let label = format!("type of static {ident}");
        self.places.push(label);
        self.visit_type(ty);
        self.places.pop();
        self.shadow_local(&ident.to_string());
        let label = format!("initializer of static {ident}");
        self.places.push(label);
        self.visit_expr(expr);
        self.places.pop();
        self.unshadow_local();
        if opened {
            self.pop_scope();
        }
    }
}

impl<'ast> Visit<'ast> for Analyzer {
    fn visit_file(&mut self, file: &'ast syn::File) {
        for item in &file.items {
            self.visit_item(item);
        }
    }

    // ---- generic owners ---------------------------------------------------

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        let owner = format!("struct {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            this.visit_fields(&node.fields);
        });
    }

    fn visit_item_union(&mut self, node: &'ast ItemUnion) {
        let owner = format!("union {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            for field in &node.fields.named {
                this.visit_field_labeled(field, None);
            }
        });
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        let owner = format!("enum {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            for variant in &node.variants {
                this.visit_attrs(&variant.attrs);
                this.visit_fields(&variant.fields);
                if let Some((_, discriminant)) = &variant.discriminant {
                    let label = format!("discriminant of variant {}", variant.ident);
                    this.places.push(label);
                    this.visit_expr(discriminant);
                    this.places.pop();
                }
            }
        });
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let owner = format!("fn {}", node.sig.ident);
        self.generic_owner(owner, &node.sig.generics, &node.attrs, |this| {
            visit_signature(this, &node.sig);
            this.visit_block(&node.block);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let owner = impl_owner_name(node);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            this.places.push("impl self type".to_owned());
            this.visit_type(&node.self_ty);
            this.places.pop();
            if let Some((trait_path, _)) = &node.trait_ {
                this.places.push("impl trait".to_owned());
                this.scan_path(trait_path, Namespace::Type, None);
                this.places.pop();
            }
            for item in &node.items {
                this.visit_impl_item(item);
            }
        });
    }

    fn visit_item_trait(&mut self, node: &'ast ItemTrait) {
        let owner = format!("trait {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            for superbound in &node.supertraits {
                this.visit_type_param_bound(superbound);
            }
            for item in &node.items {
                this.visit_trait_item(item);
            }
        });
    }

    fn visit_item_trait_alias(&mut self, node: &'ast ItemTraitAlias) {
        let owner = format!("trait alias {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            for bound in &node.bounds {
                this.visit_type_param_bound(bound);
            }
        });
    }

    fn visit_item_type(&mut self, node: &'ast ItemType) {
        let owner = format!("type {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            let label = format!("aliased type of {}", node.ident);
            this.places.push(label);
            this.visit_type(&node.ty);
            this.places.pop();
        });
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast ItemForeignMod) {
        // An extern block itself has no generics; its items are separate
        // owners, but keep a frame open so that local items inside are
        // well-formed.
        let owner = self.current_owner();
        self.push_frame(None, owner, false);
        for item in &node.items {
            self.visit_foreign_item(item);
        }
        self.pop_scope();
    }

    fn visit_item_macro(&mut self, node: &'ast ItemMacro) {
        // A `macro_rules!` definition is textual and does not reference the
        // enclosing item's generics. Any other item-position macro invocation
        // (`sql!(T)`, `serde_json!(...)`, ...) could expand to uses of every
        // parameter in scope, so its tokens are treated as opaque.
        let is_definition = node.ident.is_some();
        if !is_definition {
            self.mark_opaque();
        }
    }

    fn visit_item_const(&mut self, node: &'ast ItemConst) {
        // A free const item can declare its own generics.
        let owner = format!("const {}", node.ident);
        let opened_outer = self.frames.is_empty();
        if opened_outer {
            self.push_frame(None, None, false);
        }
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            let label = format!("type of const {}", node.ident);
            this.places.push(label);
            this.visit_type(&node.ty);
            this.places.pop();
            // The const name shadows the type/value namespaces in its initializer.
            this.shadow_local(&node.ident.to_string());
            let label = format!("initializer of const {}", node.ident);
            this.places.push(label);
            this.visit_expr(&node.expr);
            this.places.pop();
            this.unshadow_local();
        });
        if opened_outer {
            self.pop_scope();
        }
    }
    // Second Visit impl block for the remaining node kinds.
    fn visit_item_static(&mut self, node: &'ast ItemStatic) {
        self.visit_static_body(&node.ident, &node.ty, &node.expr);
    }

    // ---- impl items -------------------------------------------------------

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let owner = format!("method {}", node.sig.ident);
        self.generic_owner(owner, &node.sig.generics, &node.attrs, |this| {
            visit_signature(this, &node.sig);
            this.visit_block(&node.block);
        });
    }

    fn visit_impl_item_const(&mut self, node: &'ast ImplItemConst) {
        let owner = format!("const {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            let label = format!("type of const {}", node.ident);
            this.places.push(label);
            this.visit_type(&node.ty);
            this.places.pop();
            let label = format!("initializer of const {}", node.ident);
            this.places.push(label);
            this.visit_expr(&node.expr);
            this.places.pop();
        });
    }

    fn visit_impl_item_type(&mut self, node: &'ast ImplItemType) {
        let owner = format!("associated type {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            let label = format!("value of associated type {}", node.ident);
            this.places.push(label);
            this.visit_type(&node.ty);
            this.places.pop();
        });
    }

    // ---- trait items ------------------------------------------------------

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        let owner = format!("trait method {}", node.sig.ident);
        self.generic_owner(owner, &node.sig.generics, &node.attrs, |this| {
            visit_signature(this, &node.sig);
            if let Some(block) = &node.default {
                this.visit_block(block);
            }
        });
    }

    fn visit_trait_item_const(&mut self, node: &'ast TraitItemConst) {
        let owner = format!("trait const {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            let label = format!("type of const {}", node.ident);
            this.places.push(label);
            this.visit_type(&node.ty);
            this.places.pop();
            if let Some((_, expr)) = &node.default {
                let label = format!("default of const {}", node.ident);
                this.places.push(label);
                this.visit_expr(expr);
                this.places.pop();
            }
        });
    }

    fn visit_trait_item_type(&mut self, node: &'ast TraitItemType) {
        // Associated type generics, bounds and default:
        // `type Item<'a>: Iterator<Item = &'a T> = Foo<'a>;`
        let owner = format!("associated type {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |this| {
            for bound in &node.bounds {
                this.visit_type_param_bound(bound);
            }
            if let Some((_, default)) = &node.default {
                let label = format!("default of associated type {}", node.ident);
                this.places.push(label);
                this.visit_type(default);
                this.places.pop();
            }
        });
    }

    // ---- foreign items ----------------------------------------------------

    fn visit_foreign_item_fn(&mut self, node: &'ast ForeignItemFn) {
        let owner = format!("extern fn {}", node.sig.ident);
        self.generic_owner(owner, &node.sig.generics, &node.attrs, |this| {
            visit_signature(this, &node.sig);
        });
    }

    fn visit_foreign_item_type(&mut self, node: &'ast ForeignItemType) {
        let owner = format!("extern type {}", node.ident);
        self.generic_owner(owner, &node.generics, &node.attrs, |_| {});
    }
    // Third Visit impl block: expression/type leaf handling and HRTB scopes.

    fn visit_macro(&mut self, _node: &'ast syn::Macro) {
        // syn does not descend into a macro's tokens; the expansion could
        // reference every parameter currently in scope.
        self.mark_opaque();
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        // Attributes are handled conservatively: only the meta path is examined to
        // decide whether the attribute is an opaque (non-builtin) helper. The
        // attribute's token contents are never traversed as ordinary code.
        if !attr_is_builtin(node) {
            self.mark_opaque();
        }
    }

    fn visit_type_array(&mut self, node: &'ast TypeArray) {
        // The element type is visited in whatever place encloses the array.
        self.visit_type(&node.elem);
        // Only the length expression gets the "array length" position label.
        self.places.push("array length".to_owned());
        self.visit_expr(&node.len);
        self.places.pop();
    }

    fn visit_expr_const(&mut self, node: &'ast ExprConst) {
        self.places.push("inline const block".to_owned());
        self.visit_block(&node.block);
        self.places.pop();
    }

    fn visit_expr_array(&mut self, node: &'ast syn::ExprArray) {
        // A plain `[a, b, c]` literal; elements inherit the surrounding place.
        for element in &node.elems {
            self.visit_expr(element);
        }
    }

    fn visit_expr_repeat(&mut self, node: &'ast syn::ExprRepeat) {
        self.visit_expr(&node.expr);
        self.places.push("array length".to_owned());
        self.visit_expr(&node.len);
        self.places.pop();
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        self.visit_attrs(&node.attrs);
        // Default traversal; without a surrounding place label, references in the
        // initializer inherit the enclosing context.
        syn::visit::visit_local(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        // Label uses that occur within a function-call expression so reports can
        // distinguish call sites from other body expressions.
        self.places.push("call expression".to_owned());
        syn::visit::visit_expr_call(self, node);
        self.places.pop();
    }

    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        let qself_position = node.qself.as_ref().map(|q| q.position);
        if let Some(qself) = &node.qself {
            self.visit_type(&qself.ty);
        }
        self.scan_path(&node.path, Namespace::Type, qself_position);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        let qself_position = node.qself.as_ref().map(|q| q.position);
        if let Some(qself) = &node.qself {
            self.visit_type(&qself.ty);
        }
        self.scan_path(&node.path, Namespace::Value, qself_position);
    }

    fn visit_lifetime(&mut self, node: &'ast Lifetime) {
        if node.ident == "_" {
            return;
        }
        let span = node
            .apostrophe
            .span()
            .join(node.ident.span())
            .unwrap_or_else(|| node.ident.span());
        self.record(Namespace::Lifetime, &node.ident.to_string(), span);
    }

    // Higher-ranked binders introduce a lifetime scope.

    fn visit_bound_lifetimes(&mut self, _node: &'ast BoundLifetimes) {
        // Binder scopes are opened by the enclosing visitors
        // (`visit_type_fn_ptr`, `visit_trait_bound`,
        // `visit_predicate_type_with_binder`), which keep the scope alive
        // across the whole node. A standalone `BoundLifetimes` must not push a
        // scope that dies before the following path is visited.
    }

    fn visit_type_fn_ptr(&mut self, node: &'ast TypeFnPtr) {
        self.visit_attrs(&node.attrs);
        let depth = self.frames.len();
        if let Some(lifetimes) = &node.lifetimes {
            self.push_binder_scope(lifetimes);
        }
        for input in &node.inputs {
            self.visit_named_arg(input);
        }
        if let Some(variadic) = &node.variadic {
            self.visit_fn_ptr_variadic(variadic);
        }
        self.visit_return_type(&node.output);
        while self.frames.len() > depth {
            self.pop_scope();
        }
    }

    fn visit_trait_bound(&mut self, node: &'ast TraitBound) {
        let depth = self.frames.len();
        if let Some(lifetimes) = &node.lifetimes {
            self.push_binder_scope(lifetimes);
        }
        self.scan_path(&node.path, Namespace::Type, None);
        while self.frames.len() > depth {
            self.pop_scope();
        }
    }

    fn visit_generics(&mut self, _node: &'ast Generics) {
        // Generics are declared explicitly by each owner handler so that the
        // scope remains open while the owner body is traversed.
    }

    fn visit_signature(&mut self, _node: &'ast Signature) {
        // Signatures are visited through `visit_signature` after the owner
        // scope has been pushed.
    }
}
