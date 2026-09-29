mod common;

use common::TempDir;
use module_discover::{CfgExpr, CfgPredicate, Diagnostic, EdgeKind, ModuleOrigin, discover};
use std::path::Path;

fn find_qualname<'a>(
    result: &'a module_discover::DiscoveredCrate,
    qualname: &str,
) -> &'a module_discover::Module {
    result
        .modules
        .iter()
        .find(|module| module.qualname == qualname)
        .unwrap_or_else(|| panic!("module {qualname} not found"))
}

fn origin_is_file(module: &module_discover::Module) -> bool {
    matches!(module.origin, ModuleOrigin::File { .. })
}

fn file_path<'a>(
    result: &'a module_discover::DiscoveredCrate,
    module: &'a module_discover::Module,
) -> &'a Path {
    let _ = result;
    match &module.origin {
        ModuleOrigin::File { path, .. } => path,
        other => panic!("expected file module, got {other:?}"),
    }
}

#[test]
fn discovers_root_file_inline_and_path_relocated_modules() {
    let temp = TempDir::new("md-tree").unwrap();
    const ROOT_SRC: &str = concat!(
        "mod plain;\n",
        "mod inline {\n",
        "    mod child;\n",
        "    #[path = \"inline/relocated/deep.rs\"]\n",
        "    mod deep;\n",
        "}\n",
        "#[path = \"renamed_module.rs\"]\n",
        "mod renamed;\n",
    );
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.write("src/plain.rs", "pub struct Plain;").unwrap();
    temp.write("src/inline/child.rs", "").unwrap();
    temp.mkdir("src/inline/relocated").unwrap();
    temp.write("src/inline/relocated/deep.rs", "").unwrap();
    temp.write("src/renamed_module.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();

    let plain = find_qualname(&result, "crate::plain");
    assert!(origin_is_file(plain));
    assert!(file_path(&result, plain).ends_with("src/plain.rs"));

    let inline = find_qualname(&result, "crate::inline");
    assert!(inline.is_inline());

    let child = find_qualname(&result, "crate::inline::child");
    assert!(origin_is_file(child));
    assert!(file_path(&result, child).ends_with("src/inline/child.rs"));

    let deep = find_qualname(&result, "crate::inline::deep");
    assert!(origin_is_file(deep));
    assert!(file_path(&result, deep).ends_with("src/inline/relocated/deep.rs"));

    let renamed = find_qualname(&result, "crate::renamed");
    assert!(origin_is_file(renamed));
    assert!(file_path(&result, renamed).ends_with("src/renamed_module.rs"));

    assert_eq!(result.module(result.root).children.len(), 3);
    assert_eq!(inline.children.len(), 2);

    let inline_edges = result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Inline)
        .count();
    let file_edges = result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::File)
        .count();
    assert_eq!(inline_edges, 1);
    assert_eq!(file_edges, 4);
    assert!(
        result
            .edges
            .iter()
            .all(|edge| edge.kind != EdgeKind::Broken)
    );

    assert!(result.diagnostics().is_empty());
}

#[test]
fn byte_ranges_account_for_multibyte_characters() {
    let temp = TempDir::new("md-multibyte").unwrap();
    let src = "// 中文注释很长\nmod a;\nmod b;\n";
    temp.write("src/lib.rs", src).unwrap();
    temp.write("src/a.rs", "").unwrap();
    temp.write("src/b.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let a = find_qualname(&result, "crate::a");
    let edge = result.edges.iter().find(|edge| edge.child == a.id).unwrap();
    let a_start = src.find("mod a;").unwrap();
    assert_eq!(edge.site.range.start, a_start);
    assert_eq!(&src[edge.site.range.start..edge.site.range.end], "mod a;");

    let b = find_qualname(&result, "crate::b");
    let edge = result.edges.iter().find(|edge| edge.child == b.id).unwrap();
    assert_eq!(&src[edge.site.range.start..edge.site.range.end], "mod b;");
}

#[test]
fn reports_source_ranges_and_file_lengths() {
    let temp = TempDir::new("md-range").unwrap();
    const SRC: &str = concat!("#[cfg(unknown_predicate)]\n", "mod alpha;\n", "mod beta;\n",);
    temp.write("src/lib.rs", SRC).unwrap();
    temp.write("src/alpha.rs", "").unwrap();
    temp.write("src/beta.rs", "// beta\n").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();

    let root_file_id = result.files[0].id;
    let root_file = result.file(root_file_id);
    assert_eq!(root_file.byte_len, SRC.len());
    assert_eq!(root_file.declarations.len(), 2);

    let alpha = find_qualname(&result, "crate::alpha");
    let edge = result
        .edges
        .iter()
        .find(|edge| edge.child == alpha.id)
        .unwrap();
    let range = edge.site.range;
    assert_eq!(
        &SRC[range.start..range.end],
        "#[cfg(unknown_predicate)]\nmod alpha;"
    );

    let beta = find_qualname(&result, "crate::beta");
    let edge = result
        .edges
        .iter()
        .find(|edge| edge.child == beta.id)
        .unwrap();
    let range = edge.site.range;
    assert_eq!(&SRC[range.start..range.end], "mod beta;");

    let beta_file_id = match &beta.origin {
        ModuleOrigin::File { file, .. } => *file,
        other => panic!("{other:?}"),
    };
    assert_eq!(result.file(beta_file_id).byte_len, "// beta\n".len());
}

#[test]
fn retains_unknown_cfg_predicates_without_guessing() {
    let temp = TempDir::new("md-cfg").unwrap();
    const ROOT_SRC: &str = concat!(
        "#[cfg(any(target_os = \"linux\", feature = \"frob\"))]\n",
        "mod maybe;\n",
        "mod outer {\n",
        "    #[cfg(not(unix))]\n",
        "    mod never_checked;\n",
        "}\n",
    );
    temp.write("src/lib.rs", ROOT_SRC).unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();

    let maybe = find_qualname(&result, "crate::maybe");
    assert_eq!(maybe.cfg.len(), 1);
    match &maybe.cfg[0] {
        CfgExpr::Any(predicates) => {
            assert_eq!(predicates.len(), 2);
            match predicates.as_slice() {
                [
                    CfgExpr::Predicate(CfgPredicate::KeyValue { key, value }),
                    CfgExpr::Predicate(CfgPredicate::KeyValue {
                        key: key2,
                        value: value2,
                    }),
                ] => {
                    assert_eq!(key, "target_os");
                    assert_eq!(value, "linux");
                    assert_eq!(key2, "feature");
                    assert_eq!(value2, "frob");
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(maybe.origin, ModuleOrigin::Missing { .. }));

    let never = find_qualname(&result, "crate::outer::never_checked");
    assert_eq!(never.cfg.len(), 1);
    assert!(matches!(never.cfg[0], CfgExpr::Not(_)));
    assert!(matches!(never.origin, ModuleOrigin::Missing { .. }));

    let outer = find_qualname(&result, "crate::outer");
    assert!(outer.cfg.is_empty());
    assert!(outer.is_inline());

    let missing = result
        .diagnostics()
        .into_iter()
        .filter(|diagnostic| {
            matches!(diagnostic, Diagnostic::Missing { qualname, .. }
                if qualname == "crate::maybe"
                    || qualname == "crate::outer::never_checked")
        })
        .count();
    assert_eq!(missing, 2);
}

#[test]
fn handles_chinese_module_names() {
    let temp = TempDir::new("md-unicode").unwrap();
    const ROOT_SRC: &str = concat!("mod 模块;\n", "mod 内联 {\n", "    mod 子模块;\n", "}\n",);
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.write("src/模块.rs", "pub struct 东西;\n").unwrap();
    temp.write("src/内联/子模块.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();

    let module_ = find_qualname(&result, "crate::模块");
    assert!(file_path(&result, module_).ends_with("src/模块.rs"));

    let child = find_qualname(&result, "crate::内联::子模块");
    assert!(file_path(&result, child).ends_with("src/内联/子模块.rs"));

    assert!(result.diagnostics().is_empty());
}
