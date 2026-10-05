//! Integration tests for the AST attribute edit plan.

use attr_plan::{AttrEditKind, AttrSpec, Plan};
use quote::quote;
use syn::parse_quote;

/// Helper: parse a source string into a syn::File.
fn parse(src: &str) -> syn::File {
    syn::parse_file(src).expect("test source should parse")
}

/// Helper: quote a File and re-parse it, returning the token string.
///
/// This verifies that the edited AST round-trips through quote! and can be
/// re-parsed as valid Rust.
fn quote_reparse(file: &syn::File) -> String {
    let tokens = quote!(#file);
    let reparsed = syn::parse2::<syn::File>(tokens.clone())
        .expect("quoted file should re-parse as valid Rust");
    // The re-parsed file should be identical to the original edited file.
    assert_eq!(
        quote!(#reparsed).to_string(),
        tokens.to_string(),
        "re-parsed file should round-trip identically",
    );
    tokens.to_string()
}

// ---------- acceptance test 1 ----------

#[test]
fn add_inline_only_to_a_f() {
    let src = r#"
        mod a {
            fn f() {}
        }
        mod b {
            fn f() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let result = plan.apply(&file).expect("plan should apply");

    // a::f got #[inline], b::f did not.
    let out = quote_reparse(&result.file);
    assert!(out.contains("mod a { # [inline] fn f () { } }"), "a::f should have inline: {}", out);
    assert!(out.contains("mod b { fn f () { } }"), "b::f should be unchanged: {}", out);

    // Span mapping recorded the add.
    assert_eq!(result.mappings.len(), 1);
    assert_eq!(result.mappings[0].target, "a::f");
    assert_eq!(result.mappings[0].kind, AttrEditKind::Add);
    assert!(result.mappings[0].old_span.is_none());
    assert!(result.mappings[0].new_span.is_some());

    // Original file untouched.
    assert!(!quote!(#file).to_string().contains("inline"));
}

// ---------- acceptance test 2 ----------

#[test]
fn remove_allow_keeps_cfg_attr_allow() {
    let src = r#"
        #[allow(dead_code)]
        #[cfg_attr(feature = "x", allow(dead_code))]
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.remove(parse_quote!(f), AttrSpec::parse("allow").unwrap());

    let result = plan.apply(&file).expect("plan should apply");

    let out = quote_reparse(&result.file);
    // The direct #[allow(dead_code)] is gone.
    assert!(!out.contains("# [allow (dead_code)]"), "direct allow should be removed: {}", out);
    // The cfg_attr-generated allow is untouched.
    assert!(
        out.contains("# [cfg_attr (feature = \"x\" , allow (dead_code))]"),
        "cfg_attr allow should be kept: {}",
        out,
    );

    // Span mapping recorded the remove.
    assert_eq!(result.mappings.len(), 1);
    assert_eq!(result.mappings[0].target, "f");
    assert_eq!(result.mappings[0].kind, AttrEditKind::Remove);
    assert!(result.mappings[0].old_span.is_some());
    assert!(result.mappings[0].new_span.is_none());
}

// ---------- acceptance test 3 ----------

#[test]
fn duplicate_target_rejected() {
    let src = r#"
        mod a {
            fn f() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("duplicate target should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("duplicate target"), "error should mention duplicate target: {}", msg);

    // Original file untouched.
    assert!(!quote!(#file).to_string().contains("inline"));
}

// ---------- path ambiguity ----------

#[test]
fn ambiguous_path_rejected() {
    let src = r#"
        mod a {
            fn f() {}
            fn f() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("ambiguous path should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("ambiguous"), "error should mention ambiguity: {}", msg);
}

#[test]
fn ambiguous_module_rejected() {
    let src = r#"
        mod a {
            fn f() {}
        }
        mod a {
            fn g() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("ambiguous module should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("ambiguous"), "error should mention ambiguity: {}", msg);
}

// ---------- path not found ----------

#[test]
fn missing_item_rejected() {
    let src = r#"
        mod a {
            fn f() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::g), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("missing item should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("no item named"), "error should mention missing item: {}", msg);
}

#[test]
fn path_to_non_module_rejected() {
    let src = r#"
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(f::g), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("path through non-module should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("not a module"), "error should mention not a module: {}", msg);
}

// ---------- add/remove conflicts ----------

#[test]
fn add_existing_attr_rejected() {
    let src = r#"
        #[inline]
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(f), AttrSpec::parse("inline").unwrap());

    let err = plan.apply(&file).expect_err("adding existing attr should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("already exists"), "error should mention already exists: {}", msg);
}

#[test]
fn remove_missing_attr_rejected() {
    let src = r#"
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.remove(parse_quote!(f), AttrSpec::parse("allow").unwrap());

    let err = plan.apply(&file).expect_err("removing missing attr should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("no direct outer attribute"), "error should mention missing attr: {}", msg);
}

#[test]
fn add_remove_same_attr_rejected() {
    let src = r#"
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.entry(
        parse_quote!(f),
        vec![
            AttrEditKind::Add.into_edit("inline"),
            AttrEditKind::Remove.into_edit("inline"),
        ],
    );

    let err = plan.apply(&file).expect_err("add+remove same attr should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("conflicting"), "error should mention conflict: {}", msg);
}

// ---------- malformed spec ----------

#[test]
fn malformed_spec_rejected() {
    let result = AttrSpec::parse("inline (");
    assert!(result.is_err(), "malformed spec should fail to parse");
}

// ---------- inner attributes preserved ----------

#[test]
fn inner_attributes_preserved() {
    let src = r#"
        mod a {
            #![allow(dead_code)]
            fn f() {}
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let result = plan.apply(&file).expect("plan should apply");
    let out = quote_reparse(&result.file);
    assert!(
        out.contains("# ! [allow (dead_code)]"),
        "inner attribute should be preserved: {}",
        out,
    );
    assert!(out.contains("# [inline]"), "inline should be added: {}", out);
}

// ---------- function body preserved ----------

#[test]
fn function_body_preserved() {
    let src = r#"
        mod a {
            fn f(x: i32) -> i32 {
                let y = x + 1;
                y * 2
            }
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let result = plan.apply(&file).expect("plan should apply");
    let out = quote_reparse(&result.file);
    assert!(
        out.contains("let y = x + 1 ; y * 2"),
        "function body should be preserved: {}",
        out,
    );
}

// ---------- multiple edits on one target ----------

#[test]
fn multiple_edits_on_one_target() {
    let src = r#"
        #[allow(dead_code)]
        fn f() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.entry(
        parse_quote!(f),
        vec![
            AttrEditKind::Remove.into_edit("allow"),
            AttrEditKind::Add.into_edit("inline"),
        ],
    );

    let result = plan.apply(&file).expect("plan should apply");
    let out = quote_reparse(&result.file);
    assert!(!out.contains("# [allow (dead_code)]"), "allow should be removed: {}", out);
    assert!(out.contains("# [inline]"), "inline should be added: {}", out);
    assert_eq!(result.mappings.len(), 2);
}

// ---------- deeply nested path ----------

#[test]
fn deeply_nested_path() {
    let src = r#"
        mod a {
            mod b {
                mod c {
                    fn f() {}
                }
            }
        }
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::b::c::f), AttrSpec::parse("inline").unwrap());

    let result = plan.apply(&file).expect("plan should apply");
    let out = quote_reparse(&result.file);
    assert!(
        out.contains("mod c { # [inline] fn f () { } }"),
        "deeply nested f should get inline: {}",
        out,
    );
}

// ---------- no item reordering ----------

#[test]
fn no_item_reordering() {
    let src = r#"
        fn z() {}
        mod a {
            fn f() {}
        }
        fn y() {}
    "#;
    let file = parse(src);

    let mut plan = Plan::new();
    plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

    let result = plan.apply(&file).expect("plan should apply");
    let out = quote_reparse(&result.file);
    // Order preserved: z, a (with f), y.
    let z_pos = out.find("fn z ()").unwrap();
    let a_pos = out.find("mod a").unwrap();
    let y_pos = out.find("fn y ()").unwrap();
    assert!(z_pos < a_pos && a_pos < y_pos, "item order should be preserved: {}", out);
}

// ---------- helper trait for tests ----------

trait IntoEdit {
    fn into_edit(self, spec: &str) -> attr_plan::AttrEdit;
}

impl IntoEdit for AttrEditKind {
    fn into_edit(self, spec: &str) -> attr_plan::AttrEdit {
        let s = AttrSpec::parse(spec).unwrap();
        match self {
            AttrEditKind::Add => attr_plan::AttrEdit::Add(s),
            AttrEditKind::Remove => attr_plan::AttrEdit::Remove(s),
        }
    }
}
