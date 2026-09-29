mod common;

use common::TempDir;
use module_discover::{EdgeKind, ModuleOrigin, discover};

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

#[test]
fn path_attribute_with_trailing_separator_uses_directory_style() {
    const ROOT_SRC: &str = concat!("#[path = \"sys/\"]\n", "mod platform;\n");

    let temp = TempDir::new("md-pathdir").unwrap();
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.write("src/sys/platform.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let platform = find_qualname(&result, "crate::platform");
    match &platform.origin {
        ModuleOrigin::File { path, mod_rs, .. } => {
            assert!(path.ends_with("src/sys/platform.rs"));
            assert!(!mod_rs);
        }
        other => panic!("{other:?}"),
    }

    let temp = TempDir::new("md-pathdir2").unwrap();
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.write("src/sys/mod.rs", "").unwrap();
    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let platform = find_qualname(&result, "crate::platform");
    match &platform.origin {
        ModuleOrigin::File { path, mod_rs, .. } => {
            assert!(path.ends_with("src/sys/mod.rs"));
            assert!(mod_rs);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn directory_path_without_separator_is_still_directory_style() {
    let temp = TempDir::new("md-pathdir3").unwrap();
    const ROOT_SRC: &str = concat!("#[path = \"sys\"]\n", "mod platform;\n");
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.mkdir("src/sys").unwrap();
    temp.write("src/sys/mod.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let platform = find_qualname(&result, "crate::platform");
    assert!(matches!(
        platform.origin,
        ModuleOrigin::File { mod_rs: true, .. }
    ));
}

#[test]
fn mod_rs_style_keeps_children_in_same_directory() {
    let temp = TempDir::new("md-modrs").unwrap();
    temp.write("src/lib.rs", "mod foo;\n").unwrap();
    temp.write("src/foo/mod.rs", "mod bar;\n").unwrap();
    temp.write("src/foo/bar.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let foo = find_qualname(&result, "crate::foo");
    assert!(matches!(
        foo.origin,
        ModuleOrigin::File { mod_rs: true, .. }
    ));
    let bar = find_qualname(&result, "crate::foo::bar");
    match &bar.origin {
        ModuleOrigin::File { path, .. } => {
            assert!(path.ends_with("src/foo/bar.rs"));
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics().is_empty());
}

#[test]
fn inline_path_relocation_redirects_nested_search() {
    let temp = TempDir::new("md-inline-path").unwrap();
    const ROOT_SRC: &str = concat!(
        "#[path = \"relocated/impls\"]\n",
        "mod backend {\n",
        "    mod engine;\n",
        "}\n",
    );
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.mkdir("src/relocated").unwrap();
    temp.write("src/relocated/impls/engine.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let backend = find_qualname(&result, "crate::backend");
    assert!(backend.is_inline());
    let engine = find_qualname(&result, "crate::backend::engine");
    match &engine.origin {
        ModuleOrigin::File { path, .. } => {
            assert!(path.ends_with("src/relocated/impls/engine.rs"));
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics().is_empty());
}

#[test]
fn non_literal_path_attribute_is_reported_unsupported() {
    let temp = TempDir::new("md-badpath").unwrap();
    const ROOT_SRC: &str = concat!(
        "macro_rules! p { () => { \"x.rs\" } }\n",
        "#[path = p!()]\n",
        "mod x;\n",
    );
    temp.write("src/lib.rs", ROOT_SRC).unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let x = find_qualname(&result, "crate::x");
    assert!(matches!(x.origin, ModuleOrigin::UnsupportedPath { .. }));
    let edge = result.edges.iter().find(|edge| edge.child == x.id).unwrap();
    assert_eq!(edge.kind, EdgeKind::Broken);
}
