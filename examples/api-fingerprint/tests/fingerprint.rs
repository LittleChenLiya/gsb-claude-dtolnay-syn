//! Acceptance tests for the api-fingerprint module.

use api_fingerprint::{
    diff, fingerprint_source, ChangeKind, Kind, Report, Status, UnknownReason,
};

fn fp(src: &str) -> Report {
    fingerprint_source(src).expect("fixture must parse")
}

fn entry<'a>(report: &'a Report, kind: Kind, path: &str) -> &'a api_fingerprint::Entry {
    report
        .find(kind, path)
        .unwrap_or_else(|| panic!("missing {kind} entry {path}"))
}

// ---------------------------------------------------------------------------
// Invariance: bodies, whitespace, comments, docs
// ---------------------------------------------------------------------------

#[test]
fn function_body_change_keeps_digest() {
    let a = fp("pub fn f(x: u8) -> u8 { x + 1 }");
    let b = fp("pub fn f(x: u8) -> u8 { let y = x * 3; y.wrapping_add(7) }");
    assert_eq!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

#[test]
fn whitespace_and_comment_changes_keep_digest() {
    let a = fp("pub fn f(x: u8) -> bool { x > 0 }");
    let b = fp(
        "// leading comment\n\
         pub   fn   f(\n  x :   u8   // trailing\n\
         ) -> bool {\n\t/* block */ x>0\n}\n",
    );
    assert_eq!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

#[test]
fn doc_comment_changes_keep_digest() {
    let a = fp("/// Docs.\npub struct S { /// field docs\n pub x: u8 }");
    let b = fp("/// Totally different docs.\npub struct S { /// other\n pub x: u8 }");
    assert_eq!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&b, Kind::Struct, "S").digest
    );
    assert!(!entry(&a, Kind::Struct, "S").signature.contains("doc"));
}

#[test]
fn trait_default_method_body_change_keeps_digest() {
    let a = fp("pub trait T { fn m(&self) -> u8 { 1 } }");
    let b = fp("pub trait T { fn m(&self) -> u8 { 42 } }");
    assert_eq!(
        entry(&a, Kind::Trait, "T").digest,
        entry(&b, Kind::Trait, "T").digest
    );
}

// ---------------------------------------------------------------------------
// Sensitivity: types, where clauses, repr, discriminants, field order
// ---------------------------------------------------------------------------

#[test]
fn parameter_type_change_changes_digest() {
    let a = fp("pub fn f(x: u8) {}");
    let b = fp("pub fn f(x: u16) {}");
    assert_ne!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

#[test]
fn where_clause_change_changes_digest() {
    let a = fp("pub fn f<T>(x: T) -> T where T: Clone { x }");
    let b = fp("pub fn f<T>(x: T) -> T where T: Clone + Send { x }");
    assert_ne!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

#[test]
fn repr_attribute_change_changes_digest() {
    let a = fp("#[repr(C)] pub struct S { pub x: u8 }");
    let b = fp("#[repr(C, packed)] pub struct S { pub x: u8 }");
    assert_ne!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&b, Kind::Struct, "S").digest
    );
    assert!(entry(&a, Kind::Struct, "S").signature.contains("repr(C)"));
}

#[test]
fn discriminant_change_changes_digest() {
    let a = fp("pub enum E { A = 1, B = 2 }");
    let b = fp("pub enum E { A = 1, B = 3 }");
    assert_ne!(
        entry(&a, Kind::Enum, "E").digest,
        entry(&b, Kind::Enum, "E").digest
    );
}

#[test]
fn field_order_change_changes_digest() {
    let a = fp("pub struct S { pub a: u8, pub b: u8 }");
    let b = fp("pub struct S { pub b: u8, pub a: u8 }");
    assert_ne!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&b, Kind::Struct, "S").digest
    );
}

#[test]
fn abi_change_changes_digest() {
    let a = fp("pub fn f() {}");
    let b = fp("pub extern \"C\" fn f() {}");
    assert_ne!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

// ---------------------------------------------------------------------------
// Visibility / reachability
// ---------------------------------------------------------------------------

#[test]
fn private_items_never_enter_report() {
    let a = fp("fn hidden() -> u8 { 1 } pub fn shown() {}");
    let b = fp("fn hidden() -> u8 { 2 } pub fn shown() {}");
    assert_eq!(a.entries.len(), 1);
    assert_eq!(a.entries[0].digest, b.entries[0].digest);
}

#[test]
fn pub_crate_is_not_exported() {
    let r = fp("pub(crate) fn f() {} pub(self) struct S; pub enum E {}");
    assert!(r.find(Kind::Fn, "f").is_none());
    assert!(r.find(Kind::Struct, "S").is_none());
    assert!(r.find(Kind::Enum, "E").is_some());
}

#[test]
fn private_module_items_are_not_exports() {
    let a = fp("mod m { pub fn f() -> u8 { 1 } }");
    let b = fp("mod m { pub fn f() -> u8 { 2 } }");
    assert!(a.entries.is_empty());
    assert!(b.entries.is_empty());
}

#[test]
fn nested_public_modules_use_qualified_paths() {
    let r = fp("pub mod a { pub mod b { pub fn f() {} } pub fn g() {} }");
    assert!(r.find(Kind::Fn, "a::b::f").is_some());
    assert!(r.find(Kind::Fn, "a::g").is_some());
}

#[test]
fn pub_mod_inside_private_mod_is_not_exported() {
    let r = fp("mod outer { pub mod inner { pub fn f() {} } }");
    assert!(r.entries.is_empty());
}

// ---------------------------------------------------------------------------
// Lifetime alpha-renaming
// ---------------------------------------------------------------------------

#[test]
fn lifetime_rename_keeps_digest() {
    let a = fp("pub fn f<'a>(x: &'a str) -> &'a str { x }");
    let b = fp("pub fn f<'b>(x: &'b str) -> &'b str { x }");
    assert_eq!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
    assert!(entry(&a, Kind::Fn, "f").signature.contains("'l0"));
}

#[test]
fn independent_lifetimes_are_not_merged() {
    // Two independent lifetimes must map to two distinct canonical names.
    let r = fp("pub fn f<'a, 'b>(x: &'a str, y: &'b str) {}");
    let sig = &entry(&r, Kind::Fn, "f").signature;
    assert!(sig.contains("'l0"), "{sig}");
    assert!(sig.contains("'l1"), "{sig}");

    // Collapsing the two lifetimes into one is a different interface.
    let two = fp("pub fn f<'a, 'b>(x: &'a str, y: &'b str) {}");
    let one = fp("pub fn f<'a>(x: &'a str, y: &'a str) {}");
    assert_ne!(
        entry(&two, Kind::Fn, "f").digest,
        entry(&one, Kind::Fn, "f").digest
    );
}

#[test]
fn lifetime_use_swap_changes_digest() {
    let a = fp("pub fn f<'a, 'b>(x: &'a str, y: &'b str) -> &'a str { x }");
    let b = fp("pub fn f<'a, 'b>(x: &'b str, y: &'a str) -> &'a str { y }");
    assert_ne!(
        entry(&a, Kind::Fn, "f").digest,
        entry(&b, Kind::Fn, "f").digest
    );
}

#[test]
fn static_lifetime_is_not_renamed() {
    let r = fp("pub fn f<'a>(x: &'a str, y: &'static str) {}");
    let sig = &entry(&r, Kind::Fn, "f").signature;
    assert!(sig.contains("'static"), "{sig}");
}

#[test]
fn struct_lifetime_rename_keeps_digest() {
    let a = fp("pub struct S<'a> { pub x: &'a u8 }");
    let b = fp("pub struct S<'z> { pub x: &'z u8 }");
    assert_eq!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&b, Kind::Struct, "S").digest
    );
}

// ---------------------------------------------------------------------------
// Raw identifiers and generic defaults
// ---------------------------------------------------------------------------

#[test]
fn raw_identifiers_are_normalized() {
    let r = fp("pub fn r#match(r#type: &str) -> bool { r#type == \"m\" }");
    let e = entry(&r, Kind::Fn, "match");
    assert!(!e.signature.contains("r#"), "{}", e.signature);
    assert!(e.signature.contains("match(type:&str)"), "{}", e.signature);
}

#[test]
fn raw_identifier_body_change_keeps_digest() {
    let a = fp("pub fn r#match() -> u8 { 1 }");
    let b = fp("pub fn r#match() -> u8 { 2 }");
    assert_eq!(
        entry(&a, Kind::Fn, "match").digest,
        entry(&b, Kind::Fn, "match").digest
    );
}

#[test]
fn generic_defaults_are_preserved() {
    let r = fp("pub struct S<T = u32> { pub x: T }");
    assert!(entry(&r, Kind::Struct, "S").signature.contains("T=u32"));

    let a = fp("pub struct S<T = u32> { pub x: T }");
    let b = fp("pub struct S<T = u64> { pub x: T }");
    let c = fp("pub struct S<T> { pub x: T }");
    assert_ne!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&b, Kind::Struct, "S").digest
    );
    assert_ne!(
        entry(&a, Kind::Struct, "S").digest,
        entry(&c, Kind::Struct, "S").digest
    );
}

// ---------------------------------------------------------------------------
// Unknown boundaries
// ---------------------------------------------------------------------------

#[test]
fn item_macro_is_unknown() {
    let r = fp("pub mod m { some_macro! { pub fn generated() {} } }");
    let unknowns: Vec<_> = r.unknown().collect();
    assert_eq!(unknowns.len(), 1);
    assert_eq!(unknowns[0].kind, Kind::Macro);
    assert_eq!(unknowns[0].path, "m::<macro #0>");
    assert_eq!(
        unknowns[0].status,
        Status::Unknown(UnknownReason::ItemMacro)
    );
    // Source location is attached for diff positioning.
    assert_eq!(unknowns[0].line, 1);
    assert!(unknowns[0].column > 1);
}

#[test]
fn macro_inside_private_module_is_ignored() {
    let r = fp("mod m { some_macro!{} }");
    assert!(r.entries.is_empty());
}

#[test]
fn external_pub_use_keeps_textual_path_and_unknown_status() {
    let r = fp("pub use serde::Serialize;");
    let e = entry(&r, Kind::Use, "Serialize");
    assert_eq!(e.status, Status::Unknown(UnknownReason::ExternalUse));
    // The textual path is preserved in the normalized signature and digest.
    assert!(e.signature.contains("serde::Serialize"), "{}", e.signature);

    // A textual change to the path changes the digest.
    let r2 = fp("pub use serde::Serializer;");
    assert_ne!(e.digest, entry(&r2, Kind::Use, "Serializer").digest);
}

#[test]
fn internal_pub_use_is_known() {
    let r = fp("pub mod m { pub struct S; } pub use self::m::S as Alias;");
    let e = entry(&r, Kind::Use, "Alias");
    assert_eq!(e.status, Status::Known);
    assert!(e.signature.contains("self::m::S as Alias"), "{}", e.signature);
}

#[test]
fn glob_pub_use_is_unknown() {
    let r = fp("pub mod m {} pub use self::m::*;");
    let e = entry(&r, Kind::Use, "*");
    assert_eq!(e.status, Status::Unknown(UnknownReason::GlobUse));
}

#[test]
fn grouped_pub_use_exports_each_name() {
    let r = fp("pub use std::fmt::{Debug, Display as Show};");
    assert_eq!(
        entry(&r, Kind::Use, "Debug").status,
        Status::Unknown(UnknownReason::ExternalUse)
    );
    assert_eq!(
        entry(&r, Kind::Use, "Show").status,
        Status::Unknown(UnknownReason::ExternalUse)
    );
}

#[test]
fn unloaded_pub_module_is_unknown() {
    let r = fp("pub mod other_file;");
    let e = entry(&r, Kind::Mod, "other_file");
    assert_eq!(e.status, Status::Unknown(UnknownReason::UnloadedModule));
}

#[test]
fn unknowns_prevent_clean_diff() {
    let a = fp("some_macro!();");
    let b = fp("some_macro!();");
    let d = diff(&a, &b);
    assert!(d.changes.is_empty());
    assert!(!d.is_clean(), "unknown boundaries must taint the diff");
    assert!(!d.unknowns.is_empty());
}

// ---------------------------------------------------------------------------
// Methods, traits, diff
// ---------------------------------------------------------------------------

#[test]
fn inherent_methods_are_recorded() {
    let a = fp("pub struct S; impl S { pub fn m(&self) -> u8 { 1 } fn hidden() {} }");
    let e = entry(&a, Kind::Method, "S::m");
    assert!(e.signature.contains("fn m(&self)-> u8"), "{}", e.signature);
    assert!(a.find(Kind::Method, "S::hidden").is_none());

    let b = fp("pub struct S; impl S { pub fn m(&self) -> u8 { 2 } }");
    assert_eq!(e.digest, entry(&b, Kind::Method, "S::m").digest);

    let c = fp("pub struct S; impl S { pub fn m(&self) -> u16 { 2 } }");
    assert_ne!(e.digest, entry(&c, Kind::Method, "S::m").digest);
}

#[test]
fn trait_signature_covers_items_and_bounds() {
    let a = fp("pub trait T: Clone { fn m(&self); type A; const C: u8; }");
    let sig = &entry(&a, Kind::Trait, "T").signature;
    assert!(sig.contains("Clone"), "{sig}");
    assert!(sig.contains("fn m(&self)"), "{sig}");
    assert!(sig.contains("type A"), "{sig}");
    assert!(sig.contains("const C:u8"), "{sig}");

    let b = fp("pub trait T: Clone { fn m(&self, x: u8); type A; const C: u8; }");
    assert_ne!(
        entry(&a, Kind::Trait, "T").digest,
        entry(&b, Kind::Trait, "T").digest
    );
}

#[test]
fn diff_reports_added_removed_changed() {
    let old = fp("pub fn keep() {} pub fn gone() {} pub fn changed(x: u8) {}");
    let new = fp("pub fn keep() {} pub fn changed(x: u16) {} pub fn fresh() {}");
    let d = diff(&old, &new);
    assert!(!d.is_clean());
    let kinds: Vec<(ChangeKind, &str)> = d
        .changes
        .iter()
        .map(|c| (c.kind, c.entry().path.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (ChangeKind::Changed, "changed"),
            (ChangeKind::Added, "fresh"),
            (ChangeKind::Removed, "gone"),
        ]
    );
}

#[test]
fn identical_sources_have_clean_diff() {
    let a = fp("pub fn f() {} pub struct S; pub enum E {} pub trait T {}");
    let b = fp("pub fn f() {} pub struct S; pub enum E {} pub trait T {}");
    assert!(diff(&a, &b).is_clean());
}

// ---------------------------------------------------------------------------
// Fixture files: two versions of one interface
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/").to_owned() + name;
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn fixture_diff_matches_expectations() {
    let old = fp(&fixture("v1.rs"));
    let new = fp(&fixture("v2.rs"));

    // Body-only / lifetime-rename / whitespace changes: digests unchanged.
    for path in ["nested::inner::greet", "match", "two_lifetimes"] {
        assert_eq!(
            entry(&old, Kind::Fn, path).digest,
            entry(&new, Kind::Fn, path).digest,
            "{path} should be unchanged"
        );
    }
    // Trait default body change only: unchanged.
    assert_eq!(
        entry(&old, Kind::Trait, "nested::inner::Draw").digest,
        entry(&new, Kind::Trait, "nested::inner::Draw").digest
    );
    // Method body change only: unchanged.
    assert_eq!(
        entry(&old, Kind::Method, "nested::inner::Point::origin").digest,
        entry(&new, Kind::Method, "nested::inner::Point::origin").digest
    );

    // Real interface changes: digests differ.
    for (kind, path) in [
        (Kind::Struct, "nested::inner::Point"),  // field type
        (Kind::Struct, "nested::inner::Header"), // repr
        (Kind::Enum, "nested::inner::Color"),    // discriminant
        (Kind::Fn, "generic_default"),           // generic default
        (Kind::Fn, "constrained"),               // where clause
    ] {
        assert_ne!(
            entry(&old, kind, path).digest,
            entry(&new, kind, path).digest,
            "{path} should have changed"
        );
    }

    // Private items never enter the report.
    assert!(old.find(Kind::Fn, "nested::private_helper").is_none());
    assert!(old.find(Kind::Fn, "private_mod::hidden").is_none());
    assert!(new.find(Kind::Fn, "nested::private_helper").is_none());

    // Unknown boundaries are present and flagged, never "known".
    let unknowns: Vec<_> = old.unknown().collect();
    assert!(unknowns
        .iter()
        .any(|e| e.status == Status::Unknown(UnknownReason::ItemMacro)));
    assert!(unknowns
        .iter()
        .any(|e| e.status == Status::Unknown(UnknownReason::ExternalUse)));
    assert!(unknowns
        .iter()
        .any(|e| e.status == Status::Unknown(UnknownReason::GlobUse)));
    assert!(unknowns
        .iter()
        .any(|e| e.status == Status::Unknown(UnknownReason::UnloadedModule)));

    // The external re-export keeps its textual path.
    let e = entry(&old, Kind::Use, "Debug");
    assert!(e.signature.contains("std::fmt::Debug"));
    // The internal re-export alias is recorded and known.
    assert_eq!(entry(&old, Kind::Use, "Point2D").status, Status::Known);

    // Diff is tainted by unknowns and reports exactly the changed set.
    let d = diff(&old, &new);
    assert!(!d.is_clean());
    let changed: Vec<&str> = d
        .changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Changed)
        .map(|c| c.entry().path.as_str())
        .collect();
    assert_eq!(
        changed,
        vec![
            "constrained",
            "generic_default",
            "nested::inner::Color",
            "nested::inner::Header",
            "nested::inner::Point",
        ]
    );
}
