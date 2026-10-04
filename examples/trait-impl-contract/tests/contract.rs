use syn::{ItemImpl, ItemTrait};
use trait_impl_contract::{check_trait_impl, Difference, ItemKind, UnknownReason};

fn check(trait_src: &str, impl_src: &str) -> trait_impl_contract::ContractReport {
    let trait_def: ItemTrait = syn::parse_str(trait_src).unwrap();
    let impl_block: ItemImpl = syn::parse_str(impl_src).unwrap();
    check_trait_impl(&trait_def, &impl_block)
}

#[test]
fn generic_rename_is_isomorphic() {
    // `f<T>(T)` implemented as `f<U>(U)` is the same signature.
    let report = check(
        "trait Tr { fn f<T>(&self, x: T) -> T; }",
        "impl Tr for S { fn f<U>(&self, x: U) -> U { x } }",
    );
    assert!(report.is_empty(), "{:?}", report.mismatches);
}

#[test]
fn const_generic_rename_is_isomorphic() {
    let report = check(
        "trait Tr { fn f<const N: usize>(&self, x: [u8; N]); }",
        "impl Tr for S { fn f<const M: usize>(&self, x: [u8; M]) {} }",
    );
    assert!(report.is_empty(), "{:?}", report.mismatches);
}

#[test]
fn receiver_ref_vs_mut_is_mismatch() {
    let report = check(
        "trait Tr { fn f(&self); }",
        "impl Tr for S { fn f(&mut self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].name, "f");
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Receiver { .. }
    ));
    assert!(report.missing.is_empty());
}

#[test]
fn receiver_value_vs_ref_is_mismatch() {
    let report = check(
        "trait Tr { fn f(self); }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Receiver { .. }
    ));
}

#[test]
fn receiver_mut_value_binding_is_allowed() {
    // `mut self` vs `self` is a binding-mode detail, not a signature change.
    let report = check(
        "trait Tr { fn f(mut self); }",
        "impl Tr for S { fn f(self) {} }",
    );
    assert!(report.is_empty(), "{:?}", report.mismatches);
}

#[test]
fn default_method_is_not_required() {
    let report = check(
        "trait Tr { fn f(&self); fn g(&self) -> u32 { 0 } }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert!(report.is_empty(), "{:?}", report.missing);
}

#[test]
fn defaulted_assoc_items_are_not_required() {
    let report = check(
        "trait Tr { type A = u32; const C: u32 = 1; fn f(&self); }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert!(report.is_empty(), "{:?}", report.missing);
}

#[test]
fn missing_required_items_are_reported() {
    let report = check(
        "trait Tr { fn f(&self); type A; const C: u32; }",
        "impl Tr for S {}",
    );
    assert_eq!(report.missing.len(), 3);
    let mut kinds: Vec<_> = report.missing.iter().map(|m| m.kind).collect();
    kinds.sort_by_key(|k| match k {
        ItemKind::Method => 0,
        ItemKind::Type => 1,
        ItemKind::Const => 2,
    });
    assert_eq!(
        kinds,
        [ItemKind::Method, ItemKind::Type, ItemKind::Const]
    );
    assert!(report.mismatches.is_empty());
    assert!(!report.is_compatible());
}

#[test]
fn assoc_type_present_is_ok() {
    let report = check(
        "trait Tr { type A; fn f(&self) -> Self::A; }",
        "impl Tr for S { type A = u32; fn f(&self) -> u32 { 0 } }",
    );
    // `Self::A` vs `u32` cannot be decided statically: unknown, not mismatch.
    assert!(report.missing.is_empty());
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknowns.len(), 1);
}

#[test]
fn assoc_type_bounds_are_unknown() {
    let report = check(
        "trait Tr { type A: Clone; }",
        "impl Tr for S { type A = u32; }",
    );
    assert!(report.missing.is_empty());
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknowns.len(), 1);
    assert!(matches!(
        report.unknowns[0].reasons[0],
        UnknownReason::AssociatedTypeBounds
    ));
}

#[test]
fn assoc_const_type_mismatch() {
    let report = check(
        "trait Tr { const C: u32; }",
        "impl Tr for S { const C: i32 = 0; }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::ConstType { .. }
    ));
}

#[test]
fn abi_mismatch_is_reported() {
    let report = check(
        "trait Tr { extern \"C\" fn f(&self); }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    match &report.mismatches[0].differences[0] {
        Difference::Abi { expected, found } => {
            assert_eq!(expected, "C");
            assert_eq!(found, "Rust");
        }
        other => panic!("unexpected difference: {other:?}"),
    }
}

#[test]
fn matching_abi_is_ok() {
    let report = check(
        "trait Tr { extern \"C\" fn f(&self); }",
        "impl Tr for S { extern \"C\" fn f(&self) {} }",
    );
    assert!(report.is_empty(), "{:?}", report.mismatches);
}

#[test]
fn abi_system_vs_c_is_mismatch() {
    let report = check(
        "trait Tr { extern \"system\" fn f(&self); }",
        "impl Tr for S { extern \"C\" fn f(&self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Abi { .. }
    ));
}

#[test]
fn unsafe_and_async_mismatches() {
    let report = check(
        "trait Tr { unsafe fn f(&self); }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Safety { .. }
    ));

    let report = check(
        "trait Tr { async fn f(&self); }",
        "impl Tr for S { fn f(&self) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Asyncness { .. }
    ));
}

#[test]
fn generic_arity_mismatch() {
    let report = check(
        "trait Tr { fn f<T, U>(&self, x: T, y: U); }",
        "impl Tr for S { fn f<T>(&self, x: T, y: T) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::GenericArity {
            kind: "type",
            expected: 2,
            found: 1
        }
    ));
}

#[test]
fn concrete_type_difference_is_mismatch() {
    // Both sides are concrete primitives: provably incompatible.
    let report = check(
        "trait Tr { fn f(&self, x: i32); }",
        "impl Tr for S { fn f(&self, x: u32) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Parameter { position: 0, .. }
    ));
    assert!(report.unknowns.is_empty());
}

#[test]
fn generic_head_difference_is_mismatch() {
    let report = check(
        "trait Tr { fn f(&self, x: Vec<u8>); }",
        "impl Tr for S { fn f(&self, x: Option<u8>) {} }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::Parameter { position: 0, .. }
    ));
}

#[test]
fn alias_involved_difference_is_unknown_not_mismatch() {
    // `MyAlias` might expand to `u32`: cannot decide statically.
    let report = check(
        "trait Tr { fn f(&self, x: MyAlias); }",
        "impl Tr for S { fn f(&self, x: u32) {} }",
    );
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknowns.len(), 1);
    assert!(matches!(
        report.unknowns[0].reasons[0],
        UnknownReason::TypeAliasPossible { .. }
    ));
    // Unknown is not a proven incompatibility.
    assert!(report.is_compatible());
}

#[test]
fn where_clause_difference_is_unknown() {
    let report = check(
        "trait Tr { fn f<T>(&self, x: T) where T: Clone; }",
        "impl Tr for S { fn f<U>(&self, x: U) where U: Clone + Send {} }",
    );
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknowns.len(), 1);
    assert!(matches!(
        report.unknowns[0].reasons[0],
        UnknownReason::WhereClauseOrBounds
    ));
}

#[test]
fn equivalent_where_clause_after_rename_is_ok() {
    let report = check(
        "trait Tr { fn f<T>(&self, x: T) where T: Clone; }",
        "impl Tr for S { fn f<U>(&self, x: U) where U: Clone {} }",
    );
    assert!(report.is_empty(), "{:?}", report.unknowns);
}

#[test]
fn trait_level_generics_are_mapped_positionally() {
    // `trait Foo<A>` implemented as `impl<B> Foo<B> for S`: uses of `A` in
    // the trait correspond to uses of `B` in the impl.
    let report = check(
        "trait Foo<A> { fn get(&self) -> A; }",
        "impl<B> Foo<B> for S { fn get(&self) -> B { todo!() } }",
    );
    assert!(report.is_empty(), "{:?}", report.mismatches);
}

#[test]
fn trait_level_concrete_argument_makes_difference_unknown() {
    // `impl Foo<u32> for S`: the trait-side `A` is the concrete `u32` here,
    // but comparing `A` against arbitrary impl types needs care — stay
    // conservative and report unknown rather than a proven mismatch.
    let report = check(
        "trait Foo<A> { fn get(&self) -> A; }",
        "impl Foo<u32> for S { fn get(&self) -> u32 { 0 } }",
    );
    assert!(report.mismatches.is_empty());
}

#[test]
fn item_kind_collision_is_mismatch() {
    let report = check(
        "trait Tr { fn f(&self); }",
        "impl Tr for S { type f = u32; }",
    );
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].differences[0],
        Difference::ItemKind { .. }
    ));
}

#[test]
fn diagnostics_carry_both_side_spans() {
    let trait_def: ItemTrait = syn::parse_str(
        "trait Tr {\n    fn f(&self);\n    fn g(&self);\n}",
    )
    .unwrap();
    let impl_block: ItemImpl = syn::parse_str(
        "impl Tr for S {\n    fn f(&mut self) {}\n}",
    )
    .unwrap();
    let report = check_trait_impl(&trait_def, &impl_block);

    // The receiver mismatch points at `f` on both sides (line 2, 1-based).
    assert_eq!(report.mismatches.len(), 1);
    let m = &report.mismatches[0];
    assert_eq!(m.trait_span.start().line, 2);
    assert_eq!(m.impl_span.start().line, 2);

    // The missing `g` points at the trait item (line 3) and at the impl.
    assert_eq!(report.missing.len(), 1);
    let miss = &report.missing[0];
    assert_eq!(miss.name, "g");
    assert_eq!(miss.trait_span.start().line, 3);
    assert_eq!(miss.impl_span.start().line, 1);
}
