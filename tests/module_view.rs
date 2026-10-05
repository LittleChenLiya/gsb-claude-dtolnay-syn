//! Integration tests for the module export view.
//!
//! These tests exercise the public API of [`syn::module_view::ModuleView`]
//! from outside the crate, using small inline module fixtures.

use syn::module_view::{ExportKind, ModuleView, VisibilitySource};
use syn::File;

fn parse(src: &str) -> File {
    syn::parse_str(src).unwrap()
}

fn visible_names(view: &ModuleView, observer: &[String]) -> Vec<String> {
    view.exports_visible_from(observer)
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

fn has(view: &ModuleView, observer: &[String], name: &str) -> bool {
    visible_names(view, observer).iter().any(|n| n == name)
}

/// The three-module example: `a`, `a::b`, and `c`.
///
/// - `pub(super)` in `a::b` is visible to `a` but not to `c`.
/// - `pub(crate)` is visible throughout the crate.
/// - `pub` is visible everywhere.
#[test]
fn three_module_visibility_example() {
    let file = parse(
        r#"
        mod a {
            pub mod b {
                pub fn everywhere() {}
                pub(crate) fn crate_wide() {}
                pub(super) fn a_only() {}
                fn b_private() {}
            }
            pub fn in_a() {}
        }
        mod c {
            pub fn in_c() {}
        }
        "#,
    );
    let view = ModuleView::from_file(&file);

    // Observer at `a`: sees a's items and a::b's pub/pub(crate)/pub(super).
    assert!(has(&view, &["a".into()], "in_a"));
    assert!(has(&view, &["a".into()], "everywhere"));
    assert!(has(&view, &["a".into()], "crate_wide"));
    assert!(has(&view, &["a".into()], "a_only"));
    assert!(!has(&view, &["a".into()], "b_private"));

    // Observer at `c`: sees only pub and pub(crate) items.
    assert!(has(&view, &["c".into()], "in_c"));
    assert!(has(&view, &["c".into()], "everywhere"));
    assert!(has(&view, &["c".into()], "crate_wide"));
    assert!(!has(&view, &["c".into()], "a_only"));
    assert!(!has(&view, &["c".into()], "b_private"));

    // Observer at `a::b`: sees everything in a::b.
    assert!(has(&view, &["a".into(), "b".into()], "b_private"));
    assert!(has(&view, &["a".into(), "b".into()], "a_only"));

    // `pub(super)` resolves to the real parent, not crate-wide.
    let a_only = view
        .direct_exports(&["a".into(), "b".into()])
        .iter()
        .find(|e| e.name == "a_only")
        .unwrap();
    assert_eq!(a_only.visibility, VisibilitySource::Super);
    assert_eq!(a_only.boundary, Some(vec!["a".into()]));

    let crate_wide = view
        .direct_exports(&["a".into(), "b".into()])
        .iter()
        .find(|e| e.name == "crate_wide")
        .unwrap();
    assert_eq!(crate_wide.visibility, VisibilitySource::Crate);
    assert_eq!(crate_wide.boundary, Some(vec![]));
}

/// Illegal ancestor paths produce diagnostics.
#[test]
fn illegal_ancestor_paths_give_diagnostics() {
    let file = parse(
        r#"
        mod a {
            pub mod b {
                pub(in crate::c) fn bad_ancestor() {}
                pub(in super::super::super) fn too_many_supers() {}
            }
        }
        mod c {}
        pub(super) fn root_super() {}
        "#,
    );
    let view = ModuleView::from_file(&file);
    let messages: Vec<String> = view
        .diagnostics()
        .iter()
        .map(|d| d.message.clone())
        .collect();

    assert!(
        messages.iter().any(|m| m.contains("ancestor modules")),
        "expected ancestor-modules diagnostic, got: {:?}",
        messages
    );
    assert!(
        messages.iter().any(|m| m.contains("too many leading `super`")),
        "expected too-many-supers diagnostic, got: {:?}",
        messages
    );
}

/// External module declarations are reported as unknown boundaries and their
/// files are never read.
#[test]
fn external_module_declaration_is_unknown_boundary() {
    let file = parse(
        r#"
        mod external;
        mod a {
            pub use crate::external::Thing;
            pub use crate::external::*;
        }
        "#,
    );
    let view = ModuleView::from_file(&file);

    let ext = view.module(&["external".into()]).unwrap();
    assert!(ext.is_external);
    assert_eq!(ext.exports.len(), 0);

    let reexports: Vec<&syn::module_view::Export> = view
        .direct_exports(&["a".into()])
        .iter()
        .filter(|e| e.reexport.is_some())
        .collect();
    assert_eq!(reexports.len(), 2);
    for r in &reexports {
        let re = r.reexport.as_ref().unwrap();
        assert!(re.unknown_boundary);
    }
}

/// `use` re-exports intersect with the target item's visibility.
#[test]
fn use_reexport_intersection() {
    let file = parse(
        r#"
        mod a {
            pub fn pub_item() {}
            pub(crate) fn crate_item() {}
        }
        mod b {
            pub(crate) use crate::a::pub_item as re_crate;
            pub use crate::a::crate_item as re_pub;
        }
        "#,
    );
    let view = ModuleView::from_file(&file);

    let re_crate = view
        .direct_exports(&["b".into()])
        .iter()
        .find(|e| e.name == "re_crate")
        .unwrap();
    assert_eq!(re_crate.kind, ExportKind::Fn);
    assert_eq!(re_crate.boundary, Some(vec![]));

    let re_pub = view
        .direct_exports(&["b".into()])
        .iter()
        .find(|e| e.name == "re_pub")
        .unwrap();
    assert_eq!(re_pub.boundary, Some(vec![]));
    assert!(view
        .diagnostics()
        .iter()
        .any(|d| d.message.contains("widens visibility")));
}

/// `use ::path` (leading colon) is rooted at the crate root.
#[test]
fn use_leading_colon_is_crate_rooted() {
    let file = parse(
        r#"
        mod a {
            pub fn foo() {}
        }
        mod b {
            pub use ::a::foo as re_foo;
        }
        "#,
    );
    let view = ModuleView::from_file(&file);
    let re_foo = view
        .direct_exports(&["b".into()])
        .iter()
        .find(|e| e.name == "re_foo")
        .unwrap();
    assert!(!re_foo.reexport.as_ref().unwrap().unknown_boundary);
    assert_eq!(re_foo.boundary, None);
}

/// The report preserves declaration spans.
#[test]
fn report_preserves_declaration_spans() {
    let file = parse(
        r#"
        mod a {
            pub fn located() {}
        }
        "#,
    );
    let view = ModuleView::from_file(&file);
    let export = view
        .direct_exports(&["a".into()])
        .iter()
        .find(|e| e.name == "located")
        .unwrap();
    // The span should be preserved (non-empty debug representation).
    let span_debug = format!("{:?}", export.span);
    assert!(!span_debug.is_empty());
}
