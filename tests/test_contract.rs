#![cfg(feature = "full")]

use syn::contract::{check, Difference, ItemKind, UnknownReason};
use syn::{parse_quote, ItemImpl, ItemTrait};

fn trait_impl(trait_src: ItemTrait, impl_src: ItemImpl) -> syn::contract::ContractReport {
    check(&trait_src, &impl_src)
}

#[test]
fn generic_rename_is_isomorphic() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) -> T;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) -> U {}
        }
    };
    let report = trait_impl(t, i);
    assert!(report.is_ok(), "expected no diagnostics, got {report:?}");
}

#[test]
fn generic_rename_with_bounds() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T: Clone>(x: T) -> T;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U: Clone>(x: U) -> U {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn generic_bound_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T: Clone>(x: T);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::GenericBound { position: 0 }));
}

#[test]
fn generic_count_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U, V>(x: U) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].differences.contains(&Difference::GenericCount {
        trait_count: 1,
        impl_count: 2
    }));
}

#[test]
fn generic_kind_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<const N: usize>(x: [u8; N]) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::GenericKind { position: 0 }));
}

#[test]
fn receiver_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(&mut self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Receiver));
}

#[test]
fn receiver_value_vs_ref() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Receiver));
}

#[test]
fn receiver_mut_binding_irrelevant() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(mut self) {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn receiver_self_typing() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(self: Box<Self>);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(self: Box<Self>) {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn receiver_self_typing_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(self: Box<Self>);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(self: &Self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Receiver));
}

#[test]
fn default_method_missing_is_ok() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn required(&self);
            fn provided(&self) {}
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn required(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert!(report.is_ok());
}

#[test]
fn missing_required_method() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn a(&self);
            fn b(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn a(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].name, "b");
    assert_eq!(report.missing[0].kind, ItemKind::Fn);
}

#[test]
fn async_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            async fn f(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].differences.contains(&Difference::Async));
}

#[test]
fn unsafe_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            unsafe fn f(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Unsafe));
}

#[test]
fn abi_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            extern "C" fn f();
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            extern "Rust" fn f() {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].differences.contains(&Difference::Abi));
}

#[test]
fn abi_implicit_c_matches_explicit_c() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            extern "C" fn f();
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            extern fn f() {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn param_type_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: i32);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: u32) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ParamType { position: 0 }));
}

#[test]
fn param_count_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: i32, y: i32);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: i32) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].differences.contains(&Difference::ParamCount {
        trait_count: 2,
        impl_count: 1
    }));
}

#[test]
fn return_type_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f() -> i32;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f() -> u32 {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ReturnType));
}

#[test]
fn unit_return_matches_default() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f();
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f() -> () {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn variadic_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: i32, ...);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: i32) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Variadic));
}

#[test]
fn where_predicate_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) where T: Clone;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::WherePredicate));
}

#[test]
fn where_predicate_renamed() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) where T: Clone;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) where U: Clone {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn complex_where_is_unknown() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) where <T as Iterator>::Item: Clone;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) where <U as Iterator>::Item: Clone {}
        }
    };
    let report = trait_impl(t, i);
    assert!(report.mismatches.is_empty(), "unexpected mismatch: {report:?}");
    assert_eq!(report.unknown.len(), 1);
    assert!(report.unknown[0].reason == UnknownReason::ComplexWhere || report.unknown[0].reason == UnknownReason::TypeAlias);
}

#[test]
fn missing_assoc_const() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            const C: i32;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {}
    };
    let report = trait_impl(t, i);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].name, "C");
    assert_eq!(report.missing[0].kind, ItemKind::Const);
}

#[test]
fn assoc_const_default_not_missing() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            const C: i32 = 0;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {}
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn assoc_const_type_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            const C: i32;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            const C: u32 = 0;
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ConstType));
}

#[test]
fn missing_assoc_type() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {}
    };
    let report = trait_impl(t, i);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].name, "Item");
    assert_eq!(report.missing[0].kind, ItemKind::Type);
}

#[test]
fn assoc_type_provided() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            type Item = u8;
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn assoc_type_default_not_missing() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item = u8;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {}
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn assoc_type_bounds_is_unknown() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item: Clone;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            type Item = u8;
        }
    };
    let report = trait_impl(t, i);
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknown.len(), 1);
    assert_eq!(report.unknown[0].reason, UnknownReason::TraitBound);
}

#[test]
fn assoc_type_bounds_with_default_ok() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item: Clone = u8;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            type Item = u8;
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn assoc_type_generics_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            type Item<T>;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            type Item = u8;
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::AssocTypeGenerics));
}

#[test]
fn macro_item_is_unknown() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn required(&self);
            mac!();
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn required(&self) {}
        }
    };
    let report = trait_impl(t, i);
    assert!(report.mismatches.is_empty());
    assert_eq!(report.unknown.len(), 1);
    assert_eq!(report.unknown[0].reason, UnknownReason::TraitMacro);
}

#[test]
fn impl_macro_item_is_unknown() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn required(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn required(&self) {}
            mac!();
        }
    };
    let report = trait_impl(t, i);
    assert!(report.is_ok() || report.unknown.len() == 1);
    assert_eq!(report.unknown.len(), 1);
    assert_eq!(report.unknown[0].reason, UnknownReason::ImplMacro);
}

#[test]
fn array_length_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: [u8; 3]);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: [u8; 4]) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ParamType { position: 0 }));
}

#[test]
fn array_length_renamed() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<const N: usize>(x: [u8; N]);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<const M: usize>(x: [u8; M]) {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn tuple_type_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: (i32, u32));
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: (i32, i64)) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ParamType { position: 0 }));
}

#[test]
fn reference_lifetime_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<'a>(x: &'a i32);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<'b>(x: &'b i32) {}
        }
    };
    assert!(trait_impl(t, i).is_ok());
}

#[test]
fn reference_mutability_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: &i32);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: &mut i32) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ParamType { position: 0 }));
}

#[test]
fn fn_pointer_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f(x: fn(i32) -> i32);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(x: fn(i32) -> u32) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::ParamType { position: 0 }));
}

#[test]
fn spans_are_present() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn required(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {}
    };
    let report = trait_impl(t, i);
    assert_eq!(report.missing.len(), 1);
    let m = &report.missing[0];
    assert!(m.trait_span.start().line > 0);
    assert!(m.impl_span.start().line > 0);
}

#[test]
fn original_ast_unchanged() {
    // check 只接受 & 引用, 原 AST 不改
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) -> T;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) -> U {}
        }
    };
    let _ = check(&t, &i);
    let _ = check(&t, &i);
    assert!(check(&t, &i).is_ok());
}

#[test]
fn multiple_missing_and_mismatch() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn a(&self);
            fn b(&self);
            const C: i32;
            type Item;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn a(&mut self) {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.missing.len(), 3);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Receiver));
}

#[test]
fn unknown_and_mismatch_separated() {
    // 未知项与确定不兼容项分开输出
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn a(&self);
            fn b<T>(x: T) where <T as Iterator>::Item: Clone;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn a(&mut self) {}
            fn b<U>(x: U) where <U as Iterator>::Item: Clone {}
        }
    };
    let report = trait_impl(t, i);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0]
        .differences
        .contains(&Difference::Receiver));
    assert_eq!(report.unknown.len(), 1);
    assert!(report.unknown[0].reason == UnknownReason::ComplexWhere || report.unknown[0].reason == UnknownReason::TypeAlias);
}

#[test]
fn display_report() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn required(&self);
            fn f(&self);
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f(&mut self) {}
        }
    };
    let report = trait_impl(t, i);
    let output = format!("{report}");
    assert!(output.contains("required"), "output: {output}");
    assert!(output.contains("missing"), "output: {output}");
    assert!(output.contains("receiver"), "output: {output}");
}

#[test]
fn display_ok_report() {
    let t: ItemTrait = parse_quote! {
        trait Foo {
            fn f<T>(x: T) -> T;
        }
    };
    let i: ItemImpl = parse_quote! {
        impl Foo for () {
            fn f<U>(x: U) -> U {}
        }
    };
    let report = trait_impl(t, i);
    assert!(report.is_ok());
    let output = format!("{report}");
    assert_eq!(output, "no issues");
}
