use api_fingerprint::{diff, fingerprint_source, Fingerprint, ItemKind, UnknownReason};

fn fp(src: &str) -> Fingerprint {
    fingerprint_source(src).expect("fixture must parse")
}

fn only_entry(fp: &Fingerprint) -> &api_fingerprint::Entry {
    assert_eq!(fp.entries.len(), 1, "expected exactly one entry: {fp:#?}");
    &fp.entries[0]
}

// --- Acceptance: bodies and whitespace do not move the digest -------------

#[test]
fn digest_ignores_body_whitespace_and_comments() {
    let a = fp("pub fn f(x: u32) -> u32 { x + 1 }");
    let b = fp("pub  fn   f( x : u32 ) -> u32 {\n    // a comment\n    x * 2\n}");
    assert!(a.is_complete());
    assert_eq!(a.digest, b.digest);
    // Signatures are identical; only source locations (not in the digest) differ.
    assert_eq!(a.entries[0].signature, b.entries[0].signature);
    assert_ne!(a.entries[0].location, b.entries[0].location);
}

#[test]
fn digest_ignores_doc_comments() {
    let a = fp("/// docs\npub fn f() {}");
    let b = fp("/// totally different docs\n#[doc = \"more\"]\npub fn f() {}");
    assert_eq!(a.digest, b.digest);
}

#[test]
fn digest_ignores_trait_default_bodies() {
    let a = fp("pub trait T { fn f(&self) -> u32 { 1 } }");
    let b = fp("pub trait T { fn f(&self) -> u32 { 999 } }");
    assert_eq!(a.digest, b.digest);
    assert!(only_entry(&a).signature.contains("fn f (& self) -> u32 ;"));
}

// --- Acceptance: signature-relevant changes move the digest ---------------

#[test]
fn digest_changes_with_parameter_type() {
    let a = fp("pub fn f(x: u32) {}");
    let b = fp("pub fn f(x: u64) {}");
    assert_ne!(a.digest, b.digest);
}

#[test]
fn digest_changes_with_where_clause() {
    let a = fp("pub fn f<T>(x: T) where T: Clone {}");
    let b = fp("pub fn f<T>(x: T) where T: Copy {}");
    let c = fp("pub fn f<T>(x: T) {}");
    assert_ne!(a.digest, b.digest);
    assert_ne!(a.digest, c.digest);
}

#[test]
fn digest_changes_with_repr() {
    let a = fp("#[repr(C)] pub struct S { pub a: u8 }");
    let b = fp("#[repr(packed)] pub struct S { pub a: u8 }");
    let c = fp("pub struct S { pub a: u8 }");
    assert_ne!(a.digest, b.digest);
    assert_ne!(a.digest, c.digest);
}

#[test]
fn digest_changes_with_abi() {
    let a = fp("pub extern \"C\" fn f() {}");
    let b = fp("pub extern \"system\" fn f() {}");
    let c = fp("pub fn f() {}");
    assert_ne!(a.digest, b.digest);
    assert_ne!(a.digest, c.digest);
    assert!(only_entry(&a).signature.contains("extern \"C\""));
}

#[test]
fn digest_changes_with_discriminant_and_field_order() {
    let a = fp("pub enum E { A = 1, B = 2 }");
    let b = fp("pub enum E { A = 7, B = 2 }");
    assert_ne!(a.digest, b.digest);

    let s1 = fp("pub struct S { pub a: u8, pub b: u8 }");
    let s2 = fp("pub struct S { pub b: u8, pub a: u8 }");
    assert_ne!(s1.digest, s2.digest);
}

// --- Acceptance: private items never enter the report ---------------------

#[test]
fn private_functions_do_not_enter_report() {
    let a = fp("fn hidden() -> u32 { 1 }\npub fn shown() {}");
    let b = fp("fn hidden() -> u64 { 2 }\npub fn shown() {}");
    assert_eq!(a.digest, b.digest);
    assert_eq!(a.entries.len(), 1);
    assert_eq!(a.entries[0].path, "shown");
}

#[test]
fn restricted_visibility_is_not_exported() {
    let f = fp("pub(crate) fn a() {} pub(self) fn b() {} pub(super) fn c() {} pub fn d() {}");
    assert_eq!(f.entries.len(), 1);
    assert_eq!(f.entries[0].path, "d");
}

#[test]
fn private_module_items_do_not_enter_report() {
    let a = fp("mod m { pub fn h() -> u32 { 1 } }");
    let b = fp("mod m { pub fn h() -> u64 { 2 } }");
    assert!(a.entries.is_empty());
    assert_eq!(a.digest, b.digest);
}

// --- Unknown boundaries ----------------------------------------------------

#[test]
fn external_reexport_is_unknown_with_textual_path() {
    let f = fp("pub use serde::Serialize;");
    assert!(!f.is_complete());
    assert_eq!(f.unknowns.len(), 1);
    assert_eq!(f.unknowns[0].reason, UnknownReason::ExternalReexport);
    assert_eq!(f.unknowns[0].detail, "pub use serde::Serialize;");
    assert_eq!(f.unknowns[0].location.line, 1);

    // The textual path feeds the digest.
    let g = fp("pub use serde::Deserialize;");
    assert_ne!(f.digest, g.digest);
}

#[test]
fn glob_reexport_is_unknown() {
    let f = fp("pub use prelude::*;");
    assert_eq!(f.unknowns.len(), 1);
    assert_eq!(f.unknowns[0].reason, UnknownReason::GlobReexport);
}

#[test]
fn internal_reexport_is_recorded_as_alias() {
    let f = fp("mod m { pub fn h() {} }\npub use m::h;\npub use self::m::h as h2;\npub use crate::m::h as h3;");
    assert!(f.is_complete(), "unexpected unknowns: {:#?}", f.unknowns);
    let uses: Vec<_> = f.entries.iter().filter(|e| e.kind == ItemKind::Use).collect();
    assert_eq!(uses.len(), 3);
    assert!(uses.iter().any(|e| e.path == "h" && e.signature == "pub use m::h;"));
    assert!(uses.iter().any(|e| e.path == "h2" && e.signature == "pub use self::m::h as h2;"));
}

#[test]
fn macro_invocation_is_unknown() {
    let f = fp("macro_rules! m { () => {} }\nm!();");
    assert!(!f.is_complete());
    assert!(f.unknowns.iter().all(|u| u.reason == UnknownReason::MacroGenerated));
}

#[test]
fn derive_is_unknown_but_kept_in_signature() {
    let f = fp("#[derive(Clone)] pub struct S { pub a: u8 }");
    assert!(!f.is_complete());
    assert!(f.unknowns.iter().any(|u|u.reason == UnknownReason::MacroGenerated));
    assert!(only_entry(&f).signature.contains("derive (Clone)"));

    // Changing the derive list changes the digest.
    let g = fp("#[derive(Debug)] pub struct S { pub a: u8 }");
    assert_ne!(f.digest, g.digest);
}

#[test]
fn out_of_line_module_is_unknown() {
    let f = fp("pub mod ext;");
    assert_eq!(f.unknowns.len(), 1);
    assert_eq!(f.unknowns[0].reason, UnknownReason::OutOfLineModule);
}

#[test]
fn unknowns_carry_source_locations() {
    let f = fp("\n\npub use serde::Serialize;");
    assert_eq!(f.unknowns[0].location.line, 3);
}

// --- Lifetime normalization -------------------------------------------------

#[test]
fn bound_lifetime_rename_is_normalized() {
    let a = fp("pub fn f<'a>(x: &'a str) -> &'a str { x }");
    let b = fp("pub fn f<'b>(x: &'b str) -> &'b str { x }");
    assert_eq!(a.digest, b.digest);
    let sig = &only_entry(&a).signature;
    assert!(sig.contains("'lt0"), "sig: {sig}");
}

#[test]
fn independent_lifetimes_are_not_merged() {
    let a = fp("pub fn f<'a, 'b>(x: &'a str, y: &'b str) {}");
    let sig = &only_entry(&a).signature;
    assert!(sig.contains("'lt0"), "sig: {sig}");
    assert!(sig.contains("'lt1"), "sig: {sig}");

    // Swapping which lifetime applies to which parameter must be detected.
    let b = fp("pub fn f<'a, 'b>(x: &'b str, y: &'a str) {}");
    assert_ne!(a.digest, b.digest);
}

#[test]
fn elided_and_named_lifetimes_coexist() {
    let a = fp("pub fn f<'a>(x: &'a str, y: &str) -> &'a str { x }");
    let b = fp("pub fn f<'src>(x: &'src str, y: &str) -> &'src str { x }");
    assert_eq!(a.digest, b.digest);
}

#[test]
fn trait_level_lifetime_rename_is_normalized() {
    let a = fp("pub trait T<'a> { fn get(&'a self) -> &'a str; }");
    let b = fp("pub trait T<'x> { fn get(&'x self) -> &'x str; }");
    assert_eq!(a.digest, b.digest);
}

// --- Raw identifiers ---------------------------------------------------------

#[test]
fn raw_identifiers_are_normalized() {
    let a = fp("pub fn r#match(r#type: u32) -> u32 { r#type }");
    let sig = &only_entry(&a).signature;
    assert!(!sig.contains("r#"), "sig: {sig}");
    assert_eq!(a.entries[0].path, "match");

    // Body/whitespace-only change on raw identifiers: same digest.
    let b = fp("pub fn r#match( r#type : u32 ) -> u32 { 0 }");
    assert_eq!(a.digest, b.digest);

    // Renaming the parameter is an interface change.
    let c = fp("pub fn r#match(r#self_: u32) -> u32 { 0 }");
    assert_ne!(a.digest, c.digest);
}

// --- Generic defaults ---------------------------------------------------------

#[test]
fn generic_defaults_are_preserved() {
    let a = fp("pub struct S<T = u32> { pub v: T }");
    assert!(only_entry(&a).signature.contains("= u32"));
    let no_default = fp("pub struct S<T> { pub v: T }");
    let other_default = fp("pub struct S<T = u64> { pub v: T }");
    assert_ne!(a.digest, no_default.digest);
    assert_ne!(a.digest, other_default.digest);
}

// --- Nested modules -------------------------------------------------------------

#[test]
fn nested_modules() {
    let src = "pub mod a { pub mod b { pub fn f() {} } mod c { pub fn g() {} } }";
    let f = fp(src);
    assert!(f.entries.iter().any(|e| e.path == "a::b::f" && e.kind == ItemKind::Fn));
    assert!(!f.entries.iter().any(|e| e.path.contains('g')));
}

// --- Inherent methods -------------------------------------------------------------

#[test]
fn inherent_methods_are_entries() {
    let f = fp("pub struct S;\nimpl S { pub fn m(&self) {} fn hidden(&self) {} }");
    assert!(f.entries.iter().any(|e| e.path == "S::m"));
    assert!(!f.entries.iter().any(|e| e.path.contains("hidden")));

    let g = fp("pub struct S;\nimpl S { pub fn m(&self, x: u8) {} fn hidden(&self) {} }");
    assert_ne!(f.digest, g.digest);
}

// --- Diff -----------------------------------------------------------------------------

#[test]
fn diff_reports_added_removed_changed() {
    let old = fp("pub fn a(x: u32) {}\npub fn b() {}\npub fn gone() {}");
    let new = fp("pub fn a(x: u64) {}\npub fn b() {}\npub fn fresh() {}");
    let d = diff(&old, &new);
    assert_eq!(d.added.len(), 1);
    assert_eq!(d.added[0].path, "fresh");
    assert_eq!(d.removed.len(), 1);
    assert_eq!(d.removed[0].path, "gone");
    assert_eq!(d.changed.len(), 1);
    assert_eq!(d.changed[0].path, "a");
    assert!(!d.is_identical());
    assert!(!d.is_definitely_compatible());
}

#[test]
fn identical_complete_interfaces_are_definitely_compatible() {
    let a = fp("pub fn f(x: u32) -> u32 { x }");
    let b = fp("pub fn f(x: u32) -> u32 { x + 0 }");
    let d = diff(&a, &b);
    assert!(d.is_identical());
    assert!(d.is_definitely_compatible());
}

#[test]
fn unknown_boundary_prevents_definite_compatibility() {
    let a = fp("pub use serde::Serialize;\npub fn f() {}");
    let b = fp("pub use serde::Serialize;\npub fn f() {}");
    let d = diff(&a, &b);
    assert!(d.is_identical());
    assert!(!d.is_definitely_compatible());
}

// --- Fixtures -----------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/");
    std::fs::read_to_string(format!("{dir}{name}")).expect("fixture must exist")
}

#[test]
fn fixture_v1_and_v1_1_have_identical_digests() {
    let v1 = fp(&fixture("v1.rs"));
    let v1_1 = fp(&fixture("v1_1.rs"));
    assert_eq!(v1.digest, v1_1.digest);
    assert!(diff(&v1, &v1_1).is_identical());
}

#[test]
fn fixture_v1_and_v2_differ_with_expected_changes() {
    let v1 = fp(&fixture("v1.rs"));
    let v2 = fp(&fixture("v2.rs"));
    assert_ne!(v1.digest, v2.digest);
    let d = diff(&v1, &v2);
    let changed: Vec<&str> = d.changed.iter().map(|c| c.path.as_str()).collect();
    for expected in ["compute", "Point", "Kind", "Defaulted", "nested::helper"] {
        assert!(changed.contains(&expected), "missing {expected}: {changed:?}");
    }
    assert_eq!(d.added.len(), 1);
    assert_eq!(d.added[0].path, "new_api");
    assert_eq!(d.removed.len(), 1);
    assert_eq!(d.removed[0].path, "match");
    // Unknown boundaries are reported, not treated as compatible.
    assert!(!d.unknowns.is_empty());
    assert!(!d.is_definitely_compatible());
}

#[test]
fn fixture_v1_report_matches_golden() {
    let v1 = fp(&fixture("v1.rs"));
    let golden = fixture("v1.expected.txt");
    assert_eq!(v1.report(), golden);
}
