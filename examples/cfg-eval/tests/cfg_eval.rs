use quote::ToTokens;
use syn::File;

use cfg_eval::{CfgEval, CfgState, ConfigEnv, DiagnosticKind};

fn acceptance_env() -> ConfigEnv {
    let mut env = ConfigEnv::new();
    env.set(r#"feature="fast""#, CfgState::True);
    env.set("unix", CfgState::Unknown);
    env.set("windows", CfgState::False);
    env
}

fn analyze(source: &str) -> (File, Vec<cfg_eval::Diagnostic>) {
    analyze_with(source, acceptance_env(), CfgEval::DEFAULT_BUDGET)
}

fn analyze_with(
    source: &str,
    env: ConfigEnv,
    budget: usize,
) -> (File, Vec<cfg_eval::Diagnostic>) {
    let mut file = syn::parse_file(source).expect("input must parse");
    let diagnostics = CfgEval::new(env)
        .with_budget(budget)
        .eval_file(&mut file)
        .expect("analysis must succeed");
    reparses(&file);
    (file, diagnostics)
}

fn reparses(file: &File) {
    let rendered = file.into_token_stream().to_string();
    syn::parse_file(&rendered).expect("rewritten source must parse");
}

fn render(file: &File) -> String {
    file.into_token_stream().to_string()
}

fn function_names(file: &File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(item) => Some(item.sig.ident.to_string()),
            _ => None,
        })
        .collect()
}

fn fn_item<'a>(file: &'a File, name: &str) -> &'a syn::ItemFn {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Fn(item) if item.sig.ident == name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing fn {name}"))
}

#[test]
fn false_removes_true_strips_unknown_keeps() {
    let source = r#"
        #[cfg(windows)]
        fn only_windows() {}

        #[cfg(feature = "fast")]
        fn fast_path() {}

        #[cfg(unix)]
        fn only_unix() {}

        #[cfg(all(windows, unix))]
        fn neither() {}

        #[cfg(any(windows, unix))]
        fn maybe() {}
    "#;
    let (file, diagnostics) = analyze(source);

    assert_eq!(function_names(&file), ["fast_path", "only_unix", "maybe"]);

    assert!(fn_item(&file, "fast_path").attrs.is_empty());

    let only_unix = fn_item(&file, "only_unix");
    assert_eq!(only_unix.attrs.len(), 1);
    assert!(only_unix.attrs[0].path().is_ident("cfg"));

    let unknown: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.kind == DiagnosticKind::UnknownCondition)
        .collect();
    let paths: Vec<_> = unknown.iter().map(|diag| diag.path.clone()).collect();
    assert_eq!(paths, ["only_unix", "maybe"]);

    let maybe = unknown
        .iter()
        .find(|diag| diag.path == "maybe")
        .expect("maybe must be reported");
    assert_eq!(maybe.condition.as_deref(), Some("unix"));
}

#[test]
fn empty_all_is_true_and_empty_any_is_false() {
    let source = r#"
        #[cfg(all())]
        fn kept_all() {}

        #[cfg(any())]
        fn dropped_any() {}
    "#;
    let (file, diagnostics) = analyze(source);
    assert_eq!(function_names(&file), ["kept_all"]);
    assert!(!render(&file).contains("cfg"));
    assert!(diagnostics.is_empty());
}

#[test]
fn not_wrong_arity_and_malformed_expressions_error_with_position() {
    for source in [
        r#"#[cfg(not(unix, windows))] fn bad() {}"#,
        r#"#[cfg(not())] fn bad() {}"#,
        r#"#[cfg(123)] fn bad() {}"#,
        r#"#[cfg] fn bad() {}"#,
    ] {
        let mut file = syn::parse_file(source).unwrap();
        let err = CfgEval::new(acceptance_env())
            .eval_file(&mut file)
            .expect_err("malformed cfg must be rejected");
        assert_eq!(err.span().start().line, 1, "error carries a position: {err}");
        assert!(render(&file).contains("bad"), "no partial rewrite on error");
    }
}

#[test]
fn nested_cfg_attr_is_expanded_recursively() {
    let source = r#"
        #[cfg_attr(feature = "fast", cfg(unix))]
        fn generated_cfg_unknown() {}

        #[cfg_attr(windows, derive(Debug))]
        fn false_cfg_attr_dropped() {}

        #[cfg_attr(feature = "fast", cfg_attr(feature = "fast", cfg(windows)))]
        fn expanded_to_false() {}

        #[cfg_attr(feature = "fast", cfg_attr(unix, allow(dead_code)))]
        fn generated_cfg_attr_unknown() {}
    "#;
    let (file, diagnostics) = analyze(source);

    let names = function_names(&file);
    assert_eq!(
        names,
        [
            "generated_cfg_unknown",
            "false_cfg_attr_dropped",
            "generated_cfg_attr_unknown"
        ]
    );

    let generated_cfg = fn_item(&file, "generated_cfg_unknown");
    assert_eq!(generated_cfg.attrs.len(), 1);
    assert!(generated_cfg.attrs[0].path().is_ident("cfg"));

    let generated_cfg_attr = fn_item(&file, "generated_cfg_attr_unknown");
    assert_eq!(generated_cfg_attr.attrs.len(), 1);
    assert!(generated_cfg_attr.attrs[0].path().is_ident("cfg_attr"));

    let conditions: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.kind == DiagnosticKind::UnknownCondition)
        .map(|diag| (diag.path.as_str(), diag.condition.as_deref().unwrap()))
        .collect();
    assert_eq!(
        conditions,
        [
            ("generated_cfg_unknown", "unix"),
            ("generated_cfg_attr_unknown", "unix"),
        ]
    );
}

#[test]
fn expansion_budget_is_enforced() {
    let source = r#"
        #[cfg_attr(feature = "fast",
            cfg_attr(feature = "fast",
                cfg_attr(feature = "fast",
                    cfg(unix))))]
        fn deep() {}
    "#;
    let mut file = syn::parse_file(source).unwrap();
    let err = CfgEval::new(acceptance_env())
        .with_budget(2)
        .eval_file(&mut file)
        .expect_err("budget exhaustion must be an error");
    assert!(err.to_string().contains("budget"));
}

#[test]
fn tuple_fields_keep_legal_punctuation() {
    let source = r#"
        pub struct S(
            #[cfg(windows)] u8,
            #[cfg(feature = "fast")] u16,
            #[cfg(unix)] u32,
        );
    "#;
    let (file, diagnostics) = analyze(source);
    let struct_item = match &file.items[0] {
        syn::Item::Struct(item) => item,
        _ => panic!("expected struct"),
    };
    let unnamed = match &struct_item.fields {
        syn::Fields::Unnamed(unnamed) => unnamed,
        _ => panic!("expected tuple struct"),
    };
    assert_eq!(unnamed.unnamed.len(), 2);
    let types: Vec<_> = unnamed
        .unnamed
        .iter()
        .map(|field| field.ty.to_token_stream().to_string())
        .collect();
    assert_eq!(types, ["u16", "u32"]);
    let rendered = render(&file);
    assert!(
        rendered.contains("u16 , # [cfg (unix)] u32 ,"),
        "unexpected rendering: {rendered}"
    );

    let paths: Vec<_> = diagnostics.iter().map(|diag| diag.path.clone()).collect();
    assert_eq!(paths, ["S::2"]);
}

#[test]
fn named_fields_and_enum_variants_are_recursed() {
    let source = r#"
        struct Named {
            #[cfg(windows)] gone: u8,
            #[cfg(unix)] maybe: u16,
            kept: u32,
        }

        enum E {
            #[cfg(windows)] Dropped,
            Kept {
                #[cfg(feature = "fast")] fast: u8,
                #[cfg(unix)] maybe: u16,
            },
            #[cfg(unix)] Maybe(u8),
        }
    "#;
    let (file, diagnostics) = analyze(source);
    let rendered = render(&file);
    assert!(!rendered.contains("gone"));
    assert!(!rendered.contains("Dropped"));
    assert!(rendered.contains("maybe"));

    let paths: Vec<_> = diagnostics.iter().map(|diag| diag.path.clone()).collect();
    assert_eq!(paths, ["Named::maybe", "E::Kept::maybe", "E::Maybe"]);
}

#[test]
fn trait_and_impl_items_are_recursed() {
    let source = r#"
        trait T {
            #[cfg(windows)] fn dropped(&self);
            #[cfg(feature = "fast")] fn kept_fast(&self);
            #[cfg(unix)] fn maybe_unix(&self);
        }

        impl T for () {
            #[cfg(windows)] fn dropped(&self) {}
            #[cfg(unix)] fn maybe_unix(&self) {}
        }
    "#;
    let (file, diagnostics) = analyze(source);
    let rendered = render(&file);
    assert!(!rendered.contains("dropped"));

    let paths: Vec<_> = diagnostics.iter().map(|diag| diag.path.clone()).collect();
    assert_eq!(
        paths,
        ["T::maybe_unix", "impl::maybe_unix"]
    );
}

#[test]
fn inline_modules_recurse_and_external_modules_are_reported() {
    let source = r#"
        mod outer {
            mod inner {
                #[cfg(unix)]
                fn deep() {}
            }
        }
        mod external;
    "#;
    let (file, diagnostics) = analyze(source);
    assert!(render(&file).contains("mod external ;"));

    let module = match &file.items[0] {
        syn::Item::Mod(item) => item,
        _ => panic!("expected mod"),
    };
    assert!(module.content.is_some());

    let unknown = diagnostics
        .iter()
        .find(|diag| diag.kind == DiagnosticKind::UnknownCondition)
        .unwrap();
    assert_eq!(unknown.path, "outer::inner::deep");
    assert_eq!(unknown.condition.as_deref(), Some("unix"));

    let external = diagnostics
        .iter()
        .find(|diag| diag.kind == DiagnosticKind::ExternalModule)
        .unwrap();
    assert_eq!(external.path, "external");
}
