mod common;

use common::TempDir;
use module_discover::{Diagnostic, ModuleOrigin, discover};

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
fn reports_unparseable_module_file() {
    let temp = TempDir::new("md-parseerr").unwrap();
    temp.write("src/lib.rs", "mod broken;\n").unwrap();
    temp.write("src/broken.rs", "fn oops( {\n").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let broken = find_qualname(&result, "crate::broken");
    assert!(matches!(broken.origin, ModuleOrigin::ParseError { .. }));
    assert!(result.diagnostics().iter().any(|diagnostic| {
        matches!(diagnostic, Diagnostic::ParseError { qualname, .. }
            if qualname == "crate::broken")
    }));
}

#[test]
fn declaration_chain_points_at_every_resolved_file() {
    let temp = TempDir::new("md-chain").unwrap();
    temp.write("src/lib.rs", "mod a;\n").unwrap();
    temp.write("src/a.rs", "mod b;\n").unwrap();
    temp.write("src/a/b.rs", "mod c;\n").unwrap();
    temp.write("src/a/b/c.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let c = find_qualname(&result, "crate::a::b::c");

    assert_eq!(c.chain.len(), 4);
    assert_eq!(c.chain[0].name, "crate");
    assert_eq!(c.chain[1].name, "a");
    assert_eq!(c.chain[2].name, "b");
    assert_eq!(c.chain[3].name, "c");

    let expected = ["lib.rs", "a.rs", "b.rs", "c.rs"];
    for (hop, file_name) in c.chain.iter().zip(expected) {
        match &hop.resolution {
            Some(module_discover::HopResolution::File(path)) => {
                assert!(path.ends_with(file_name), "{path:?}");
            }
            other => panic!("{other:?}"),
        }
        let declared_path = &result.file(hop.declared.file).path;
        let predecessor = match file_name {
            "lib.rs" => "lib.rs",
            "a.rs" => "lib.rs",
            "b.rs" => "a.rs",
            "c.rs" => "b.rs",
            other => panic!("{other}"),
        };
        assert!(declared_path.ends_with(predecessor));
    }
}

#[test]
fn reports_ambiguity_when_both_styles_exist() {
    let temp = TempDir::new("md-ambiguous").unwrap();
    temp.write("src/lib.rs", "mod foo;\n").unwrap();
    temp.write("src/foo.rs", "").unwrap();
    temp.write("src/foo/mod.rs", "").unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let foo = find_qualname(&result, "crate::foo");

    let candidates = match &foo.origin {
        ModuleOrigin::Ambiguous { candidates, .. } => candidates.clone(),
        other => panic!("expected ambiguity, got {other:?}"),
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|path| path.ends_with("foo.rs")));
    assert!(candidates.iter().any(|path| path.ends_with("foo/mod.rs")));

    assert!(foo.children.is_empty());
    let edge = result
        .edges
        .iter()
        .find(|edge| edge.child == foo.id)
        .unwrap();
    assert_eq!(edge.kind, module_discover::EdgeKind::Broken);

    let diagnostic = result
        .diagnostics()
        .into_iter()
        .find(|diagnostic| {
            matches!(diagnostic, Diagnostic::Ambiguous { qualname, .. }
                if qualname == "crate::foo")
        })
        .expect("ambiguous diagnostic");
    if let Diagnostic::Ambiguous { chain, .. } = diagnostic {
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].name, "crate");
        assert_eq!(chain[1].name, "foo");
    }
}

#[test]
fn missing_module_carries_searched_paths_and_chain() {
    let temp = TempDir::new("md-missing").unwrap();
    const ROOT_SRC: &str = concat!("mod outer {\n", "    mod ghost;\n", "}\n");
    temp.write("src/lib.rs", ROOT_SRC).unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let ghost = find_qualname(&result, "crate::outer::ghost");

    let searched = match &ghost.origin {
        ModuleOrigin::Missing { searched, .. } => searched.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(searched.len(), 2);
    assert!(searched[0].ends_with("src/outer/ghost.rs"));
    assert!(searched[1].ends_with("src/outer/ghost/mod.rs"));

    assert_eq!(ghost.chain.len(), 3);
    assert_eq!(ghost.chain[2].name, "ghost");
}

#[test]
fn detects_cycle_through_path_attribute() {
    let temp = TempDir::new("md-cycle").unwrap();
    const ROOT_SRC: &str = concat!("#[path = \"a.rs\"]\n", "mod a;\n");
    const A_SRC: &str = concat!("#[path = \"a.rs\"]\n", "mod again;\n");
    temp.write("src/lib.rs", ROOT_SRC).unwrap();
    temp.write("src/a.rs", A_SRC).unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let again = find_qualname(&result, "crate::a::again");

    match &again.origin {
        ModuleOrigin::Cycle { path, back_to, .. } => {
            assert!(path.ends_with("src/a.rs"));
            assert_eq!(*back_to, 1);
        }
        other => panic!("expected cycle, got {other:?}"),
    }

    let chain_len = again.chain.len();
    let diagnostic = result
        .diagnostics()
        .into_iter()
        .find(|diagnostic| {
            matches!(diagnostic, Diagnostic::Cycle { qualname, .. }
                if qualname == "crate::a::again")
        })
        .expect("cycle diagnostic");
    if let Diagnostic::Cycle { chain, back_to, .. } = diagnostic {
        assert_eq!(chain.len(), chain_len);
        assert_eq!(chain[back_to].name, "a");
        assert_eq!(chain.last().unwrap().name, "again");
    }
}

#[test]
fn detects_mutual_file_inclusion_cycle() {
    let temp = TempDir::new("md-mutual").unwrap();
    temp.write("src/lib.rs", "mod x;\n").unwrap();
    temp.write("src/x.rs", "mod y;\n").unwrap();
    const Y_SRC: &str = concat!("#[path = \"../x.rs\"]\n", "mod back;\n");
    temp.write("src/x/y.rs", Y_SRC).unwrap();

    let result = discover(temp.path().join("src/lib.rs")).unwrap();
    let back = find_qualname(&result, "crate::x::y::back");

    match &back.origin {
        ModuleOrigin::Cycle { back_to, .. } => {
            assert_eq!(*back_to, 1);
        }
        other => panic!("expected cycle, got {other:?}"),
    }
    assert!(result.diagnostics().iter().any(|diagnostic| matches!(
        diagnostic,
        Diagnostic::Cycle { qualname, .. } if qualname == "crate::x::y::back"
    )));
}
