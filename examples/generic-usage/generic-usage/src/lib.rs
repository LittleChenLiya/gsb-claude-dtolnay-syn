//! Reusable analysis of generic parameter usage inside a single `syn::Item`.
//!
//! Given any item (struct, enum, trait, impl, function, ...) this analysis
//! reports, for every type / lifetime / const generic parameter declared by
//! that item:
//!
//! - the parameter kind and the source location of its declaration;
//! - the source locations of every position that references it, discovered by
//!   walking field types, function signatures, `where` clauses, generic
//!   parameter defaults, associated types and inline const expressions.
//!
//! Scoping follows Rust's rules:
//!
//! - generic parameters declared by nested items, methods, associated types
//!   (GATs) and higher-ranked trait bounds (`for<'a> ...`) shadow same-named
//!   outer parameters;
//! - nested *items* (an `fn` or `struct` inside a function body, items inside
//!   a `mod`, ...) cannot refer to the outer item's generic parameters at
//!   all, so references inside them never count as uses of the outer
//!   parameters;
//! - macro invocations and uninterpreted (`Verbatim`) tokens are *opaque*:
//!   their token stream may mention any parameter without the analysis being
//!   able to see it. When an item contains such regions, `unused_params`
//!   conservatively reports nothing, because no parameter can be proven
//!   unused. `syntactically_unused_params` remains available for callers that
//!   only care about the visible syntax tree.

use proc_macro2::Span;
use std::collections::HashMap;
use std::fmt;
use syn::visit::{self, Visit};
use syn::{
    BoundLifetimes, Expr, GenericParam, Generics, ImplItem, ImplItemFn, ImplItemType, Item,
    Lifetime, LifetimeParam, Macro, Pat, Path, PredicateType, TraitBound, TraitItem, TraitItemFn,
    TraitItemType, Type, TypeFnPtr,
};

/// The kind of a generic parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamKind {
    Type,
    Lifetime,
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

/// A source position: 1-based line, 0-based column.
///
/// Produced from `proc_macro2::Span::start`, which requires the
/// `span-locations` feature of `proc-macro2` (enabled by this crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl Location {
    fn of(span: Span) -> Self {
        let start = span.start();
        Location {
            line: start.line,
            column: start.column,
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Usage information for one generic parameter declared by the analyzed item.
#[derive(Debug)]
pub struct ParamUsage {
    pub kind: ParamKind,
    pub name: String,
    /// Where the parameter is declared in the item's generics.
    pub declared_at: Location,
    /// Every position at which the parameter is referenced, in the order the
    /// references were discovered during the traversal.
    pub used_at: Vec<Location>,
}

impl ParamUsage {
    pub fn is_used(&self) -> bool {
        !self.used_at.is_empty()
    }
}

/// The result of analyzing one item.
#[derive(Debug, Default)]
pub struct GenericUsageReport {
    /// One entry per generic parameter declared by the analyzed item, in
    /// declaration order.
    pub params: Vec<ParamUsage>,
    /// True if the item contains regions the analysis cannot see into: macro
    /// invocations or uninterpreted token streams. Parameters *might* be
    /// referenced from such regions.
    pub has_opaque_regions: bool,
}

impl GenericUsageReport {
    /// Look up a single parameter by kind and name.
    pub fn get(&self, kind: ParamKind, name: &str) -> Option<&ParamUsage> {
        self.params
            .iter()
            .find(|param| param.kind == kind && param.name == name)
    }

    /// Parameters that are provably unused.
    ///
    /// Conservative: if the item contains opaque regions (macro invocations,
    /// verbatim tokens) that could invisibly refer to any parameter, this
    /// returns an empty list because nothing can be proven unused.
    pub fn unused_params(&self) -> Vec<&ParamUsage> {
        if self.has_opaque_regions {
            return Vec::new();
        }
        self.syntactically_unused_params()
    }

    /// Parameters with no reference anywhere in the visible syntax tree,
    /// ignoring the possible presence of references hidden inside opaque
    /// regions.
    pub fn syntactically_unused_params(&self) -> Vec<&ParamUsage> {
        self.params
            .iter()
            .filter(|param| !param.is_used())
            .collect()
    }
}

/// Analyze the generic parameters declared by `item` and report every
/// position that uses them.
pub fn analyze_item(item: &Item) -> GenericUsageReport {
    let mut analyzer = Analyzer::default();
    analyzer.enter_item(item, /* root = */ true);
    analyzer.report
}

/// One lexical scope of generic parameters.
///
/// `params` maps a parameter to `Some(index)` when it is one of the analyzed
/// (root) item's parameters reported in `GenericUsageReport::params`, or to
/// `None` when it is a local parameter (of a nested item, method, GAT or
/// `for<...>` binder) that merely shadows an outer name.
struct Scope {
    /// A barrier scope hides every scope below it: nested items cannot refer
    /// to the generic parameters of the item that textually contains them.
    barrier: bool,
    params: HashMap<(ParamKind, String), Option<usize>>,
}

#[derive(Default)]
struct Analyzer {
    report: GenericUsageReport,
    scopes: Vec<Scope>,
}

fn generics_of(item: &Item) -> Option<&Generics> {
    match item {
        Item::Enum(item) => Some(&item.generics),
        Item::Fn(item) => Some(&item.sig.generics),
        Item::Impl(item) => Some(&item.generics),
        Item::Struct(item) => Some(&item.generics),
        Item::Trait(item) => Some(&item.generics),
        Item::TraitAlias(item) => Some(&item.generics),
        Item::Type(item) => Some(&item.generics),
        Item::Union(item) => Some(&item.generics),
        _ => None,
    }
}

impl Analyzer {
    fn with_scope(&mut self, barrier: bool, f: impl FnOnce(&mut Self)) {
        self.scopes.push(Scope {
            barrier,
            params: HashMap::new(),
        });
        f(self);
        self.scopes.pop();
    }

    /// Enter an item: push a scope, declare its generic parameters and visit
    /// its contents. Only the root item's parameters are recorded in the
    /// report; nested items act as barriers and their parameters only shadow.
    fn enter_item(&mut self, item: &Item, root: bool) {
        self.with_scope(!root, |this| {
            if let Some(generics) = generics_of(item) {
                this.declare(generics, /* record = */ root);
            }
            visit::visit_item(this, item);
        });
    }

    /// Declare the parameters of a generics list in the innermost scope.
    fn declare(&mut self, generics: &Generics, record: bool) {
        for param in &generics.params {
            let (kind, name, span) = match param {
                GenericParam::Type(param) => {
                    (ParamKind::Type, param.ident.to_string(), param.ident.span())
                }
                GenericParam::Lifetime(param) => (
                    ParamKind::Lifetime,
                    param.lifetime.ident.to_string(),
                    param.lifetime.span(),
                ),
                GenericParam::Const(param) => {
                    (ParamKind::Const, param.ident.to_string(), param.ident.span())
                }
            };
            let index = if record {
                let index = self.report.params.len();
                self.report.params.push(ParamUsage {
                    kind,
                    name: name.clone(),
                    declared_at: Location::of(span),
                    used_at: Vec::new(),
                });
                Some(index)
            } else {
                None
            };
            self.scopes
                .last_mut()
                .unwrap()
                .params
                .insert((kind, name), index);
        }
    }

    /// Declare the lifetimes of a `for<'a, ...>` binder in the innermost
    /// scope. These are always local: they shadow outer lifetimes but are not
    /// part of the report.
    fn declare_bound_lifetimes(&mut self, bound: &BoundLifetimes) {
        for param in &bound.lifetimes {
            if let GenericParam::Lifetime(param) = param {
                let name = param.lifetime.ident.to_string();
                self.scopes
                    .last_mut()
                    .unwrap()
                    .params
                    .insert((ParamKind::Lifetime, name), None);
            }
        }
    }

    /// Run `f` with the given optional `for<...>` binder in scope.
    fn with_bound_lifetimes(
        &mut self,
        lifetimes: &Option<BoundLifetimes>,
        f: impl FnOnce(&mut Self),
    ) {
        match lifetimes {
            Some(bound) => self.with_scope(/* barrier = */ false, |this| {
                this.declare_bound_lifetimes(bound);
                f(this);
            }),
            None => f(self),
        }
    }

    /// Resolve a name against the visible scopes, innermost first, not
    /// crossing a nested-item barrier. Returns `Some(Some(index))` for a
    /// reported root parameter, `Some(None)` for a shadowing local parameter
    /// and `None` if the name is not a visible generic parameter.
    fn resolve(&self, kind: ParamKind, name: &str) -> Option<Option<usize>> {
        for scope in self.scopes.iter().rev() {
            if let Some(found) = scope.params.get(&(kind, name.to_owned())) {
                return Some(*found);
            }
            if scope.barrier {
                return None;
            }
        }
        None
    }

    fn record_use(&mut self, kind: ParamKind, name: &str, span: Span) {
        if let Some(Some(index)) = self.resolve(kind, name) {
            self.report.params[index].used_at.push(Location::of(span));
        }
    }
}

impl<'ast> Visit<'ast> for Analyzer {
    fn visit_item(&mut self, node: &'ast Item) {
        // Only reached for nested items; the root item is entered directly
        // through `enter_item`.
        if let Item::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        self.enter_item(node, /* root = */ false);
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        if let ImplItem::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        visit::visit_impl_item(self, node);
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        if let TraitItem::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        visit::visit_trait_item(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        // A method's own generics shadow the impl's generics, which remain
        // visible underneath.
        self.with_scope(/* barrier = */ false, |this| {
            this.declare(&node.sig.generics, /* record = */ false);
            visit::visit_impl_item_fn(this, node);
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        self.with_scope(/* barrier = */ false, |this| {
            this.declare(&node.sig.generics, /* record = */ false);
            visit::visit_trait_item_fn(this, node);
        });
    }

    fn visit_impl_item_type(&mut self, node: &'ast ImplItemType) {
        // Generic associated types: `type Guard<'a> = ...;`
        self.with_scope(/* barrier = */ false, |this| {
            this.declare(&node.generics, /* record = */ false);
            visit::visit_impl_item_type(this, node);
        });
    }

    fn visit_trait_item_type(&mut self, node: &'ast TraitItemType) {
        self.with_scope(/* barrier = */ false, |this| {
            this.declare(&node.generics, /* record = */ false);
            visit::visit_trait_item_type(this, node);
        });
    }

    fn visit_type_fn_ptr(&mut self, node: &'ast TypeFnPtr) {
        // `for<'a> fn(&'a T)` — higher-ranked lifetimes shadow outer ones.
        self.with_bound_lifetimes(&node.lifetimes, |this| {
            visit::visit_type_fn_ptr(this, node);
        });
    }

    fn visit_trait_bound(&mut self, node: &'ast TraitBound) {
        // `for<'a> Trait<&'a T>` and `dyn for<'a> Fn(&'a T)`.
        self.with_bound_lifetimes(&node.lifetimes, |this| {
            visit::visit_trait_bound(this, node);
        });
    }

    fn visit_predicate_type(&mut self, node: &'ast PredicateType) {
        // `for<'a> T: Trait<'a>` in a where clause.
        self.with_bound_lifetimes(&node.lifetimes, |this| {
            visit::visit_predicate_type(this, node);
        });
    }

    fn visit_lifetime_param(&mut self, node: &'ast LifetimeParam) {
        // A lifetime declaration `'a: 'b` is not a use of `'a`; only its
        // bounds may use other lifetimes.
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        for bound in &node.bounds {
            self.visit_lifetime(bound);
        }
    }

    fn visit_path(&mut self, node: &'ast Path) {
        // Any path whose first segment names a visible type or const generic
        // parameter is a use: `T`, `T::Assoc`, `Vec<T>` (the inner `T` is
        // visited as its own path), `N` in an array length or const generic
        // argument, `Foo<N>`, paths in expressions and patterns, etc.
        if let Some(segment) = node.segments.first() {
            let name = segment.ident.to_string();
            let span = segment.ident.span();
            for kind in [ParamKind::Type, ParamKind::Const] {
                self.record_use(kind, &name, span);
            }
        }
        visit::visit_path(self, node);
    }

    fn visit_lifetime(&mut self, node: &'ast Lifetime) {
        let name = node.ident.to_string();
        let span = node.apostrophe;
        self.record_use(ParamKind::Lifetime, &name, span);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        // The token stream of a macro invocation is opaque: it may expand to
        // code referring to any of the item's generic parameters.
        self.report.has_opaque_regions = true;
        self.visit_path(&node.path);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        if let Expr::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        visit::visit_expr(self, node);
    }

    fn visit_type(&mut self, node: &'ast Type) {
        if let Type::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        visit::visit_type(self, node);
    }

    fn visit_pat(&mut self, node: &'ast Pat) {
        if let Pat::Verbatim(_) = node {
            self.report.has_opaque_regions = true;
            return;
        }
        visit::visit_pat(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::File;

    /// Location of the token `col_offset` bytes after the start of the first
    /// occurrence of `needle` in `src`.
    fn loc(src: &str, needle: &str, col_offset: usize) -> Location {
        let at = src.find(needle).expect("needle not found") + col_offset;
        let line = src[..at].bytes().filter(|&b| b == b'\n').count() + 1;
        let line_start = src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        Location {
            line,
            column: at - line_start,
        }
    }

    fn analyze_one(src: &str) -> GenericUsageReport {
        let item: Item = syn::parse_str(src).unwrap();
        analyze_item(&item)
    }

    fn analyze_all(src: &str) -> Vec<GenericUsageReport> {
        let file: File = syn::parse_str(src).unwrap();
        file.items.iter().map(analyze_item).collect()
    }

    #[test]
    fn struct_fields_where_and_array_len() {
        let src = "\
struct Pair<'a, T, U, const N: usize>
where
    T: Clone,
{
    first: &'a T,
    second: U,
    buf: [T; N],
}
";
        let report = analyze_one(src);
        assert_eq!(report.params.len(), 4);
        assert!(!report.has_opaque_regions);

        // Declaration positions.
        let a = report.get(ParamKind::Lifetime, "a").unwrap();
        assert_eq!(a.declared_at, loc(src, "'a, T", 0));
        let t = report.get(ParamKind::Type, "T").unwrap();
        assert_eq!(t.declared_at, loc(src, "T, U", 0));
        let u = report.get(ParamKind::Type, "U").unwrap();
        assert_eq!(u.declared_at, loc(src, "U, const", 0));
        let n = report.get(ParamKind::Const, "N").unwrap();
        assert_eq!(n.declared_at, loc(src, "N: usize", 0));

        // 'a: referenced by the `first` field type.
        assert_eq!(a.used_at, vec![loc(src, "'a T", 0)]);

        // T: where clause, `first` field, array element type.
        assert_eq!(
            t.used_at,
            vec![
                loc(src, "T: Clone", 0),
                loc(src, "'a T", 3),
                loc(src, "[T; N]", 1),
            ]
        );

        // U: `second` field.
        assert_eq!(u.used_at, vec![loc(src, "second: U", 8)]);

        // N: array length.
        assert_eq!(n.used_at, vec![loc(src, "[T; N]", 4)]);

        assert!(report.unused_params().is_empty());
    }

    #[test]
    fn impl_block_and_method_signatures() {
        let src = "\
impl<'a, T, U, const N: usize> Pair<'a, T, U, N> {
    fn first(&self) -> &'a T {
        self.first
    }
    fn buf(&self) -> &[T; N] {
        &self.buf
    }
}
";
        let report = analyze_one(src);
        assert_eq!(report.params.len(), 4);

        let a = report.get(ParamKind::Lifetime, "a").unwrap();
        assert_eq!(a.used_at, vec![loc(src, "Pair<'a", 5), loc(src, "&'a T", 1)]);

        let t = report.get(ParamKind::Type, "T").unwrap();
        assert_eq!(
            t.used_at,
            vec![
                loc(src, "Pair<'a, T", 9),
                loc(src, "&'a T", 4),
                loc(src, "[T; N]", 1),
            ]
        );

        let u = report.get(ParamKind::Type, "U").unwrap();
        assert_eq!(u.used_at, vec![loc(src, "T, U, N>", 3)]);

        let n = report.get(ParamKind::Const, "N").unwrap();
        assert_eq!(
            n.used_at,
            vec![loc(src, "T, U, N>", 6), loc(src, "[T; N]", 4)]
        );

        assert!(report.unused_params().is_empty());
    }

    #[test]
    fn higher_ranked_lifetimes_shadow_outer() {
        let src = "\
struct Callbacks<'a> {
    borrowed: &'a str,
    callback: for<'a> fn(&'a str) -> &'a str,
    handler: Box<dyn for<'a> Fn(&'a str)>,
}
";
        let report = analyze_one(src);
        let a = report.get(ParamKind::Lifetime, "a").unwrap();
        // Only the `borrowed` field refers to the struct's 'a. The `for<'a>`
        // binders introduce fresh lifetimes that shadow it.
        assert_eq!(a.used_at, vec![loc(src, "borrowed: &'a str", 11)]);
    }

    #[test]
    fn higher_ranked_lifetime_in_where_clause() {
        let src = "\
struct Wrap<T>
where
    for<'b> T: Extend<&'b str>,
{
    value: T,
}
";
        let report = analyze_one(src);
        let t = report.get(ParamKind::Type, "T").unwrap();
        assert_eq!(
            t.used_at,
            vec![loc(src, "for<'b> T", 8), loc(src, "value: T", 7)]
        );
    }

    #[test]
    fn nested_items_shadow_and_do_not_capture() {
        let src = "\
fn outer<T>(input: T) -> T {
    fn inner<T>(x: T) -> T {
        x
    }
    struct Local<T>(T);
    input
}
";
        let report = analyze_one(src);
        let t = report.get(ParamKind::Type, "T").unwrap();
        // Only the outer function's own signature refers to its T. The nested
        // items declare their own T which shadows, and cannot capture the
        // outer one.
        assert_eq!(
            t.used_at,
            vec![loc(src, "input: T", 7), loc(src, ") -> T {", 5)]
        );
    }

    #[test]
    fn associated_types_and_bounds() {
        let src = "\
trait Collect<T> {
    type Output: IntoIterator<Item = T>;
    type Guard<'a>: AsRef<T>;
}

impl<T> Collect<T> for Vec<T> {
    type Output = Vec<T>;
}
";
        let reports = analyze_all(src);
        assert_eq!(reports.len(), 2);

        // Uses through an associated type's bounds, including a GAT whose own
        // 'a parameter does not interfere with the trait's T.
        let t = reports[0].get(ParamKind::Type, "T").unwrap();
        assert_eq!(
            t.used_at,
            vec![loc(src, "Item = T", 7), loc(src, "AsRef<T>", 6)]
        );

        // Uses in the impl header (trait ref and self type) and in the
        // associated type's value.
        let t = reports[1].get(ParamKind::Type, "T").unwrap();
        assert_eq!(
            t.used_at,
            vec![
                loc(src, "impl<T> Collect<T>", 16),
                loc(src, "for Vec<T>", 8),
                loc(src, "= Vec<T>", 6),
            ]
        );
    }

    #[test]
    fn default_generics() {
        let src = "\
struct Defaulted<T, U = Vec<T>, const N: usize = 16> {
    slot: U,
    buf: [u8; N],
}
";
        let report = analyze_one(src);

        // T is used by U's default even though no field mentions it.
        let t = report.get(ParamKind::Type, "T").unwrap();
        assert_eq!(t.used_at, vec![loc(src, "Vec<T>", 4)]);

        let u = report.get(ParamKind::Type, "U").unwrap();
        assert_eq!(u.used_at, vec![loc(src, "slot: U", 6)]);

        let n = report.get(ParamKind::Const, "N").unwrap();
        assert_eq!(n.used_at, vec![loc(src, "[u8; N]", 5)]);

        assert!(report.unused_params().is_empty());
    }

    #[test]
    fn unused_param_is_reported() {
        let report = analyze_one("struct Spare<T, U>(T);");
        let t = report.get(ParamKind::Type, "T").unwrap();
        assert_eq!(t.used_at, vec![loc("struct Spare<T, U>(T);", "(T)", 1)]);
        let unused: Vec<&str> = report
            .unused_params()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(unused, ["U"]);
    }

    #[test]
    fn inline_const_expressions() {
        let src = "\
struct Matrix<const N: usize> {
    data: [u8; { N + 1 }],
    dims: [usize; N],
    cube: [[[u8; N]; N]; N],
}
";
        let report = analyze_one(src);
        let n = report.get(ParamKind::Const, "N").unwrap();
        assert_eq!(
            n.used_at,
            vec![
                loc(src, "{ N + 1 }", 2),
                loc(src, "[usize; N]", 8),
                // The three array lengths of `cube`: 17, 21, 25 on line 4.
                loc(src, "[[u8; N]", 6),
                loc(src, "[[u8; N]; N]", 10),
                loc(src, "[[u8; N]; N]; N]", 14),
            ]
        );
        assert!(report.unused_params().is_empty());
    }

    #[test]
    fn const_generic_arguments() {
        let src = "\
struct Holder<const N: usize> {
    inner: Array<N>,
    shifted: Array<{ N + 1 }>,
}
";
        let report = analyze_one(src);
        let n = report.get(ParamKind::Const, "N").unwrap();
        assert_eq!(
            n.used_at,
            vec![loc(src, "Array<N>", 6), loc(src, "{ N + 1 }", 2)]
        );
    }

    #[test]
    fn macro_invocation_is_opaque() {
        let src = "\
fn convert<T>(input: u8) -> u8 {
    magic!(T);
    input
}
";
        let report = analyze_one(src);
        assert!(report.has_opaque_regions);

        // Syntactically T is never used outside the macro...
        let t = report.get(ParamKind::Type, "T").unwrap();
        assert!(t.used_at.is_empty());
        assert_eq!(report.syntactically_unused_params().len(), 1);

        // ...but the macro body is an unknown scope that may refer to T, so
        // T must not be reported as provably unused.
        assert!(report.unused_params().is_empty());
    }

    #[test]
    fn no_macro_means_unused_is_provable() {
        let src = "\
fn convert<T>(input: u8) -> u8 {
    input
}
";
        let report = analyze_one(src);
        assert!(!report.has_opaque_regions);
        let unused: Vec<&str> = report
            .unused_params()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(unused, ["T"]);
    }

    #[test]
    fn macro_in_impl_marks_whole_item_opaque() {
        let src = "\
struct Opaque<T>(u8);

impl<T> Opaque<T> {
    fn name(&self) -> &'static str {
        stringify!(T)
    }
}
";
        let reports = analyze_all(src);
        // The struct itself contains no macro: T is provably unused there.
        assert!(!reports[0].has_opaque_regions);
        assert_eq!(reports[0].unused_params().len(), 1);
        // The impl contains a macro invocation.
        assert!(reports[1].has_opaque_regions);
        assert!(reports[1].unused_params().is_empty());
    }
}
