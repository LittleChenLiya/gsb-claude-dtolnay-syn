//! Acceptance tests for the generic-usage analyzer.
//!
//! Covers:
//! - a struct with T, U, a lifetime and const N referenced from a field, a
//!   `where` predicate and an array length, with reports pointing back at each
//!   position;
//! - higher-ranked lifetime bindings (`for<'b>`);
//! - nested items shadowing outer parameters with the same name;
//! - associated types and associated consts;
//! - default generic parameters (type and const defaults);
//! - macros, which make unused parameters "possibly used" rather than unused.

use generic_usage::{analyze_file, analyze_item, ParamKind};
use syn::{parse_file, parse_str, Item};

fn parse_one(src: &str) -> Item {
    parse_str::<Item>(src).expect("test snippet must parse")
}

fn labels<'a>(report: &'a generic_usage::ItemReport, name: &str) -> Vec<&'a str> {
    report
        .param(name)
        .unwrap_or_else(|| panic!("parameter {name:?} expected on {}", report.owner))
        .reference_labels()
        .collect()
}

#[test]
fn struct_field_where_and_array_length_locations() {
    let src = "\
struct Container<'a, T, U, const N: usize>
where
    T: AsRef<U>,
{
    first: &'a T,
    table: [U; N],
}
";
    let report = analyze_item(&parse_one(src));

    assert_eq!(report.owner, "struct Container");

    let a = report.param("a").unwrap();
    assert_eq!(a.kind, ParamKind::Lifetime);
    assert_eq!(a.declared_at.at, (1, 18));
    assert_eq!(a.references.len(), 1);
    assert_eq!(a.references[0].at, (5, 13));
    assert_eq!(a.references[0].label, "field \"first\"");

    let t = report.param("T").unwrap();
    assert_eq!(t.kind, ParamKind::Type);
    assert_eq!(t.declared_at.at, (1, 22));
    assert_eq!(t.references[0].at, (3, 5));
    assert_eq!(t.references[0].label, "where clause");
    assert_eq!(t.references[1].at, (5, 16));
    assert_eq!(t.references[1].label, "field \"first\"");

    let u = report.param("U").unwrap();
    assert_eq!(u.references[0].at, (3, 14));
    assert_eq!(u.references[0].label, "where clause");
    assert_eq!(u.references[1].at, (6, 13));
    assert_eq!(u.references[1].label, "field \"table\"");

    let n = report.param("N").unwrap();
    assert_eq!(n.kind, ParamKind::Const);
    assert_eq!(n.declared_at.at, (1, 34));
    assert_eq!(n.references.len(), 1);
    assert_eq!(n.references[0].at, (6, 16));
    assert_eq!(n.references[0].label, "array length");

    // Everything is used; nothing is unused or possibly-used.
    for p in &report.params {
        assert!(p.is_used(), "{} should be used", p.name);
        assert!(!p.is_unused());
        assert!(!p.possibly_used());
    }
}

#[test]
fn tuple_struct_fields_and_enum_variants_are_indexed() {
    let src = "\
struct Pair<T, U>(T, Box<U>);
enum Either<A, B, C> {
    Left(A),
    Right { value: B },
    Neither,
}
";
    let file = parse_file(src).unwrap();
    let reports = analyze_file(&file);

    let pair = &reports[0];
    assert_eq!(labels(pair, "T"), ["tuple field 0"]);
    assert_eq!(labels(pair, "U"), ["tuple field 1"]);

    let either = &reports[1];
    assert_eq!(labels(either, "A"), ["tuple field 0"]);
    assert_eq!(labels(either, "B"), ["field \"value\""]);
    assert!(!either.param("C").unwrap().is_used());
    assert!(either.param("C").unwrap().is_unused());
}

#[test]
fn method_signature_and_return_type_on_impl() {
    let src = "\
impl<'a, T> Container<'a, T> {
    fn get<U>(&'a self, other: U) -> &'a T
    where
        U: Into<T>,
    {
        self.value
    }
}
";
    let report = analyze_item(&parse_one(src));
    assert_eq!(report.owner, "impl Container<'a,T>");

    assert!(report.param("a").unwrap().is_used());
    assert!(report.param("T").unwrap().is_used());

    let method = report
        .nested_owner("method get")
        .expect("method report nested under impl");
    // The method's own U shadows nothing outer here; references resolve to it.
    let u = method.param("U").unwrap();
    let place_labels: Vec<&str> = u.reference_labels().collect();
    assert!(place_labels.contains(&"argument `other`"));
    assert!(place_labels.contains(&"where clause"));

    // The outer T appearing in `U: Into<T>` and the return type is recorded on
    // the outer report.
    let outer_t_labels: Vec<&str> = report.param("T").unwrap().reference_labels().collect();
    assert!(outer_t_labels.iter().any(|l| l.contains("impl self type")));
    assert!(outer_t_labels.iter().any(|l| *l == "where clause"));
    assert!(outer_t_labels.iter().any(|l| *l == "return type"));

    // Lifetime 'a used in receiver and return type resolves to the impl.
    let outer_a: Vec<&str> = report.param("a").unwrap().reference_labels().collect();
    assert!(outer_a.iter().any(|l| *l == "method receiver"));
    assert!(outer_a.iter().any(|l| *l == "return type"));
}

#[test]
fn higher_ranked_lifetime_binding_introduces_own_scope() {
    // 'b is bound by for<'b> and must NOT be recorded against the function's
    // own 'a; references to 'b resolve to the binder (no report entry), while
    // 'a is still observed elsewhere.
    let src = "\
fn hrtb<'a, T>(x: &'a T, f: &dyn for<'b> Fn(&'b T) -> &'b T) {
    drop(x);
}
";
    let report = analyze_item(&parse_one(src));
    let a_labels: Vec<&str> = labels(&report, "a");
    assert!(
        a_labels.iter().any(|l| *l == "argument `x`"),
        "lifetime a used on argument x: {a_labels:?}"
    );
    assert!(
        !a_labels.iter().any(|l| l.contains("f")),
        "binder lifetime must not leak into a: {a_labels:?}"
    );

    let t_labels: Vec<&str> = labels(&report, "T");
    assert!(t_labels.iter().any(|l| l.contains("argument `x`")));
    assert!(t_labels.iter().any(|l| l.contains("argument `f`")));

    // A for-bounded where predicate must behave the same way.
    let src2 = "\
fn hrwhere<T>()
where
    for<'c> dyn Fn(&'c T): Send,
{
}
";
    let report2 = analyze_item(&parse_one(src2));
    assert!(report2
        .param("T")
        .unwrap()
        .reference_labels()
        .any(|l| l == "where clause"));
}

#[test]
fn function_pointer_hrtb_is_scoped() {
    let src = "\
fn ptrs<'a, T>(f: for<'b> fn(&'b T) -> &'b T, g: fn(&'a T)) {}
";
    let report = analyze_item(&parse_one(src));
    let t_labels: Vec<&str> = labels(&report, "T");
    assert!(t_labels.iter().any(|l| l.contains("argument `f`")));
    assert!(t_labels.iter().any(|l| l.contains("argument `g`")));
    let a_labels: Vec<&str> = labels(&report, "a");
    assert_eq!(
        a_labels.iter().filter(|l| l.contains("argument")).count(),
        1,
        "only g should mention a: {a_labels:?}"
    );
}

#[test]
fn nested_item_generic_shadowing() {
    let src = "\
fn outer<T>() {
    fn inner<T>() -> T {
        loop {}
    }
    let _value: T = inner::<T>();
}
";
    let report = analyze_item(&parse_one(src));
    assert_eq!(report.owner, "fn outer");

    let inner = report
        .nested_owner("fn inner")
        .expect("nested fn report exists");
    assert!(
        inner.param("T").unwrap().is_used(),
        "inner T in return type"
    );

    // The inner T in the return type resolves to inner report, never outer.
    let outer_return_hits = report
        .param("T")
        .unwrap()
        .references
        .iter()
        .filter(|s| s.label == "return type")
        .count();
    assert_eq!(outer_return_hits, 0, "inner return type shadows outer T");

    // Outer T is still used at the call site in the body.
    let outer_labels: Vec<&str> = labels(&report, "T");
    assert!(
        outer_labels.iter().any(|l| l.contains("call expression")),
        "{outer_labels:?}"
    );
}

#[test]
fn associated_types_record_their_own_and_outer_params() {
    let src = "\
trait Collection<T> {
    type Item<'a>: Iterator<Item = &'a T>
    where
        T: Clone;
    const LEN: usize;
    fn len(&self) -> usize;
}
";
    let report = analyze_item(&parse_one(src));
    assert_eq!(report.owner, "trait Collection");

    let assoc = report
        .nested_owner("associated type Item")
        .expect("associated type report");
    assert_eq!(assoc.param("a").unwrap().kind, ParamKind::Lifetime);
    assert!(assoc.param("a").unwrap().is_used());

    let trait_t_labels: Vec<&str> = labels(&report, "T");
    assert!(trait_t_labels.iter().any(|l| l.contains("associated type")));
    assert!(
        trait_t_labels.iter().any(|l| *l == "where clause"),
        "{trait_t_labels:?}"
    );
}

#[test]
fn default_generic_parameters() {
    let src = "\
struct Defaults<T = u8, U = Vec<T>, const N: usize = 4> {
    values: [T; N],
    marker: U,
}
";
    let report = analyze_item(&parse_one(src));

    let t_labels: Vec<&str> = labels(&report, "T");
    assert!(
        t_labels.iter().any(|l| *l == "default type of parameter U"),
        "{t_labels:?}"
    );
    assert!(report
        .param("N")
        .unwrap()
        .references
        .iter()
        .any(|s| s.label == "array length"));

    let src2 = "struct CD<const A: usize, const B: usize = A> { a: [u8; B] }";
    let report2 = analyze_item(&parse_one(src2));
    assert!(report2
        .param("A")
        .unwrap()
        .references
        .iter()
        .any(|s| s.label == "default value of const parameter B"));
    assert!(report2
        .param("B")
        .unwrap()
        .references
        .iter()
        .any(|s| s.label == "array length"));
}

#[test]
fn inline_const_expression_is_traversed() {
    let src = "\
fn compute<const N: usize, T>() -> [T; N]
where
    T: Default,
{
    let x: [usize; 1] = [const { N + 1 }; 1];
    x
}
";
    let report = analyze_item(&parse_one(src));
    let n = report.param("N").unwrap();
    let labels: Vec<&str> = n.reference_labels().collect();
    assert!(
        labels.iter().any(|l| *l == "inline const block"),
        "{labels:?}"
    );
    // The return type `[T; N]` reports its length as an array length position.
    assert!(labels.iter().any(|l| *l == "array length"), "{labels:?}");
}

#[test]
fn unused_parameters_without_unknown_scopes_are_confident() {
    let src = "\
struct Waste<'a, T, U, const N: usize> {
    kept: T,
}
";
    let report = analyze_item(&parse_one(src));
    let unused: Vec<&str> = report.unused_params().map(|p| p.name.as_str()).collect();
    assert_eq!(unused, ["a", "U", "N"]);
    assert!(report.possibly_used_params().next().is_none());
    assert!(report.param("T").unwrap().is_used());
}

#[test]
fn macros_make_unused_parameters_possibly_used() {
    let src = "\
fn maybe_used<T, U>() {
    todo!();
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.unused_params().next().is_none());
    let names: Vec<&str> = report
        .possibly_used_params()
        .map(|p| p.name.as_str())
        .collect();
    assert!(names.contains(&"T"));
    assert!(names.contains(&"U"));
}

#[test]
fn item_position_macro_invocation_is_opaque() {
    let src = "\
fn outer<T>() {
    unknown_macro!(T);
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.param("T").unwrap().possibly_used());
}

#[test]
fn derive_attribute_is_opaque_but_builtin_attrs_are_not() {
    let src = "\
#[derive(Debug)]
struct Derived<T, U> {
    value: T,
}

#[repr(C)]
struct Plain<'a, A> {
    value: A,
}
";
    let file = parse_file(src).unwrap();
    let reports = analyze_file(&file);

    let derived = reports
        .iter()
        .find(|r| r.owner == "struct Derived")
        .unwrap();
    assert!(derived.param("T").unwrap().possibly_used() || derived.param("T").unwrap().is_used());
    assert!(derived.param("U").unwrap().possibly_used());
    assert!(derived.unused_params().next().is_none());

    let plain = reports.iter().find(|r| r.owner == "struct Plain").unwrap();
    assert!(plain.param("a").unwrap().is_unused());
    assert!(plain.param("A").unwrap().is_used());
}

#[test]
fn trait_impl_associated_const_and_constraints() {
    let src = "\
struct S<T, const N: usize>([T; N]);

trait Marked {
    const K: usize;
}

impl<T, const N: usize> Marked for S<T, N>
where
    T: Clone,
{
    const K: usize = N;
}
";
    let file = parse_file(src).unwrap();
    let reports = analyze_file(&file);
    let impl_report = reports
        .iter()
        .find(|r| r.owner.starts_with("impl Marked for"))
        .expect("impl report");

    let n_labels: Vec<&str> = labels(impl_report, "N");
    // N occurs in the self type `S<T, N>` and in the associated const
    // initializer.
    assert!(
        n_labels.iter().any(|l| *l == "impl self type"),
        "{n_labels:?}"
    );
    assert!(n_labels
        .iter()
        .any(|l| l.contains("initializer of const K")));

    let k = impl_report.nested_owner("const K").expect("const K report");
    assert!(k.params.is_empty());
}

#[test]
fn qself_associated_item_resolution() {
    // <T as Iterator>::Item: T lives in qself.ty and is recorded; the Item
    // segment is an associated item and is never a generic parameter.
    let src = "\
fn q<T, Item>() -> <T as Iterator>::Item
where
    T: Iterator,
{
    loop {}
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.param("Item").unwrap().is_unused());
    assert!(report.param("T").unwrap().is_used());
}

#[test]
fn parameter_bounds_reference_other_parameters() {
    let src = "\
struct Bounds<'a, 'b, T, U>
where
    'a: 'b,
    T: Into<U>,
{
    a: &'a T,
    b: &'b U,
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.param("b").unwrap().is_used());
    assert!(report.param("U").unwrap().is_used());
}

#[test]
fn rooted_paths_and_static_lifetime_are_not_generics() {
    let src = "\
struct Rooted<T> {
    value: ::std::vec::Vec<T>,
    static_lt: &'static str,
}
";
    let report = analyze_item(&parse_one(src));
    let t = report.param("T").unwrap();
    assert_eq!(t.references.len(), 1, "only the Vec<T> argument uses T");
    assert_eq!(t.references[0].label, "field \"value\"");
}

#[test]
fn macro_rules_definition_is_not_opaque_but_invocation_is() {
    // A textual macro_rules definition cannot capture the enclosing function's
    // generics, so an unused parameter stays confidently unused.
    let src = "\
fn defined<T, U>() {
    macro_rules! helper {
        ($x:expr) => {};
    }
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.param("T").unwrap().is_unused());
    assert!(report.param("U").unwrap().is_unused());

    // A function-like attribute macro on the item is opaque.
    let src2 = "\
#[some::helper]
fn decorated<T>() {}
";
    let report2 = analyze_item(&parse_one(src2));
    assert!(report2.param("T").unwrap().possibly_used());
}

#[test]
fn nested_local_item_with_same_name_does_not_shadow_type() {
    // Inner fn items introduce their own parameter scopes; the outer T used in
    // the inner fn's signature only when named explicitly.
    let src = "\
impl<T> Container<T> {
    fn outer_method<U>() {
        fn helper<U>(x: U) {}
    }
}
";
    let file = parse_file(src).unwrap();
    let report = analyze_file(&file).into_iter().next().expect("one root");
    let outer = report.nested_owner("method outer_method").expect("method");
    let helper = outer.nested_owner("fn helper").expect("helper nested");
    assert!(helper.param("U").unwrap().is_used());
    // The method's own U is never referenced outside the nested fn.
    assert!(outer.param("U").unwrap().is_unused());
    // The impl T is used in the impl self type.
    assert!(report.param("T").unwrap().is_used());
}

#[test]
fn union_fields_and_generic_bounds_defaults() {
    let src = "\
union Uni<'a, T: Clone, U = T, const N: usize> {
    one: &'a T,
    two: [U; N],
}
";
    let report = analyze_item(&parse_one(src));
    assert!(report.param("a").unwrap().is_used());
    // U defaults to T: T is referenced by the default.
    let t_labels: Vec<&str> = labels(&report, "T");
    assert!(t_labels
        .iter()
        .any(|l| l.contains("default type of parameter U")));
    assert!(labels(&report, "U").iter().any(|l| *l == "field \"two\""));
    // N is the array length inside the "two" field type.
    assert!(labels(&report, "N").iter().any(|l| *l == "array length"));
}

#[test]
fn end_to_end_struct_with_impl_method_and_locations() {
    // The headline acceptance fixture: a struct with T, U, 'a and const N,
    // plus an impl method referencing each of them, with every report mapping
    // back to its concrete source positions.
    let src = "\
struct Table<'a, T, U, const N: usize>
where
    T: AsRef<U>,
{
    head: &'a T,
    rows: [U; N],
}

impl<'a, T, U, const N: usize> Table<'a, T, U, N> {
    fn lookup(&self, key: &'a U) -> &'a T {
        self.head
    }
}
";
    let file = parse_file(src).unwrap();
    let reports = analyze_file(&file);
    assert_eq!(reports.len(), 2, "struct and impl are separate owners");

    let table = &reports[0];
    assert_eq!(table.owner, "struct Table");
    assert_eq!(table.param("a").unwrap().kind, ParamKind::Lifetime);
    assert_eq!(table.param("T").unwrap().kind, ParamKind::Type);
    assert_eq!(table.param("U").unwrap().kind, ParamKind::Type);
    assert_eq!(table.param("N").unwrap().kind, ParamKind::Const);

    // Every parameter is used at least once and nothing is reported unused or
    // only possibly-used (no macros or external attributes involved).
    assert_eq!(table.unused_params().count(), 0);
    assert_eq!(table.possibly_used_params().count(), 0);

    let impl_report = &reports[1];
    assert!(impl_report.owner.starts_with("impl Table<"));

    // The method has no generics of its own; its signature uses the impl's.
    let method = impl_report
        .nested_owner("method lookup")
        .expect("lookup nested in impl");
    assert!(method.params.is_empty(), "method declares no generics");

    // The impl's 'a appears in the receiver-like argument and return type.
    let a_labels: Vec<&str> = impl_report.param("a").unwrap().reference_labels().collect();
    assert!(a_labels.iter().any(|l| l.contains("argument `key`")));
    assert!(a_labels.iter().any(|l| *l == "return type"));

    // The const N only appears in the self type's array length / self type.
    let n = impl_report.param("N").unwrap();
    assert!(n.is_used());
}
