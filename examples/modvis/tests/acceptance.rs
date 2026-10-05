//! Acceptance tests for the module export view.
//!
//! Run with: cargo test -p modvis

use modvis::{analyze_file, analyze_module, ExportView, Verdict};

/// Three modules: `a` (with nested `b`), `c`, plus an external `ext`.
const THREE_MODULES: &str = "\
mod a {
    pub mod b {
        pub(super) fn only_a() {}
        pub(crate) fn whole_crate() {}
        pub(in crate::a) fn within_a() {}
        pub fn everyone() {}
        fn hidden() {}
    }
    mod ext;
    pub use ext::Thing;
}
mod c {
    pub fn elsewhere() {}
}
";

fn view(src: &str, observer: &[&str]) -> ExportView {
    let file = syn::parse_file(src).expect("fixture parses");
    analyze_file(&file, observer)
}

fn verdict(view: &ExportView, name: &str) -> Verdict {
    view.get(name)
        .unwrap_or_else(|| panic!("no report for item `{name}`"))
        .verdict
        .clone()
}

#[test]
fn pub_super_visible_to_parent_not_to_sibling() {
    // `only_a` is declared in `a::b` with `pub(super)`, so it is visible
    // anywhere inside `a` (including `a::b` itself) but not in `c`.
    let from_a = view(THREE_MODULES, &["a"]);
    assert_eq!(verdict(&from_a, "only_a"), Verdict::Visible);

    let from_ab = view(THREE_MODULES, &["a", "b"]);
    assert_eq!(verdict(&from_ab, "only_a"), Verdict::Visible);

    let from_c = view(THREE_MODULES, &["c"]);
    assert_eq!(verdict(&from_c, "only_a"), Verdict::Hidden);

    // Crucially, `pub(super)` in `a::b` is NOT crate-visible.
    let from_root = view(THREE_MODULES, &[]);
    assert_eq!(verdict(&from_root, "only_a"), Verdict::Hidden);
}

#[test]
fn pub_crate_visible_everywhere_in_crate() {
    for observer in [&["a"][..], &["a", "b"][..], &["c"][..], &[][..]] {
        let v = view(THREE_MODULES, observer);
        assert_eq!(
            verdict(&v, "whole_crate"),
            Verdict::Visible,
            "pub(crate) must be visible from {observer:?}"
        );
    }
}

#[test]
fn pub_in_ancestor_module() {
    // `pub(in crate::a)` in `a::b`: visible within `a` (and `a::b`), not in `c`.
    assert_eq!(verdict(&view(THREE_MODULES, &["a"]), "within_a"), Verdict::Visible);
    assert_eq!(
        verdict(&view(THREE_MODULES, &["a", "b"]), "within_a"),
        Verdict::Visible
    );
    assert_eq!(verdict(&view(THREE_MODULES, &["c"]), "within_a"), Verdict::Hidden);
}

#[test]
fn plain_pub_and_private() {
    let from_c = view(THREE_MODULES, &["c"]);
    assert_eq!(verdict(&from_c, "everyone"), Verdict::Visible);
    assert_eq!(verdict(&from_c, "hidden"), Verdict::Hidden);

    let from_ab = view(THREE_MODULES, &["a", "b"]);
    assert_eq!(verdict(&from_ab, "hidden"), Verdict::Visible);
}

#[test]
fn pub_in_non_ancestor_is_a_diagnostic() {
    let src = "\
mod a {
    pub mod b {
        pub(in crate::c) fn misplaced() {}
    }
}
mod c {}
";
    let v = view(src, &["a"]);
    assert_eq!(verdict(&v, "misplaced"), Verdict::Hidden);
    assert!(
        v.diagnostics
            .iter()
            .any(|d| d.message.contains("is not an ancestor module of `a::b`")),
        "expected an invalid-ancestor diagnostic, got: {:?}",
        v.diagnostics
    );
    // The diagnostic points at the offending path (line 3, `crate::c`).
    let diag = &v.diagnostics[0];
    assert_eq!(diag.span.0, 3);
}

#[test]
fn external_module_declaration_is_unknown_boundary() {
    let v = view(THREE_MODULES, &["a"]);
    let ext = v.get("ext").expect("report for `ext`");
    assert_eq!(ext.kind, "mod");
    match &ext.verdict {
        Verdict::Unknown(reason) => assert!(reason.contains("not read"), "{reason}"),
        other => panic!("external module must be unknown, got {other:?}"),
    }
}

#[test]
fn use_reexport_across_external_boundary_is_unknown() {
    // `pub use ext::Thing;` inside `a`: the re-export itself is `pub`, but
    // its target lives behind the external `mod ext;` boundary.
    let v = view(THREE_MODULES, &["c"]);
    let re = v.get("Thing").expect("report for re-export `Thing`");
    assert_eq!(re.kind, "use");
    assert_eq!(re.visibility, "pub");
    match &re.verdict {
        Verdict::Unknown(reason) => {
            assert!(reason.contains("external module boundary `a::ext`"), "{reason}")
        }
        other => panic!("re-export across boundary must be unknown, got {other:?}"),
    }
}

#[test]
fn use_reexport_of_known_item_stays_visible() {
    let src = "\
mod a {
    pub mod b {
        pub fn f() {}
    }
    pub use b::f;
    pub use self::b::f as g;
}
";
    let v = view(src, &[]);
    assert_eq!(verdict(&v, "f"), Verdict::Visible);
    assert_eq!(verdict(&v, "g"), Verdict::Visible);
}

#[test]
fn report_keeps_declaration_span_and_visibility_source() {
    let v = view(THREE_MODULES, &["a"]);
    let only_a = v.get("only_a").expect("report for `only_a`");
    // Line 3 of THREE_MODULES, 8 spaces of indentation (0-based column).
    assert_eq!(only_a.span, (3, 8));
    assert_eq!(only_a.visibility, "pub(super)");
    assert_eq!(only_a.visibility_span, (3, 8));
    assert_eq!(only_a.module, vec!["a".to_string(), "b".to_string()]);

    let hidden = v.get("hidden").expect("report for `hidden`");
    assert_eq!(hidden.visibility, "private");
}

#[test]
fn inline_module_ast_entry_point() {
    // The input may be an inline `mod` AST node rather than a whole file.
    let module: syn::ItemMod = syn::parse_str(
        "mod root {
            mod inner {
                pub(super) fn up_one() {}
                pub(crate) fn everywhere() {}
            }
        }",
    )
    .expect("inline module parses");

    let from_root = analyze_module(&module, &["root"]);
    assert_eq!(verdict(&from_root, "up_one"), Verdict::Visible);
    assert_eq!(verdict(&from_root, "everywhere"), Verdict::Visible);

    // An observer outside `root` does not exist in this AST: diagnosed, and
    // `pub(super)` stays hidden from it.
    let from_elsewhere = analyze_module(&module, &["outside"]);
    assert_eq!(verdict(&from_elsewhere, "up_one"), Verdict::Hidden);
    assert!(
        from_elsewhere
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not found")),
        "unknown observer path should be diagnosed"
    );
}

#[test]
fn macros_are_not_expanded() {
    let src = "\
mod a {
    macro_rules! m {
        () => { pub fn generated() {} };
    }
    m!();
}
";
    let v = view(src, &[]);
    assert!(v.get("generated").is_none(), "macros must not be expanded");
    assert!(v.diagnostics.is_empty());
}
