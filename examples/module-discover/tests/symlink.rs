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
fn flags_symlink_pointing_outside_crate_root() {
    let outside = match TempDir::new("md-outside2") {
        Ok(dir) => dir,
        Err(_) => return,
    };
    outside.write("evasion.rs", "mod deeper;\n").unwrap();
    let crate_dir = match TempDir::new("md-symlink-crate2") {
        Ok(dir) => dir,
        Err(_) => return,
    };
    crate_dir.write("src/lib.rs", "mod linked;\n").unwrap();
    if crate_dir
        .symlink_file(&outside.path().join("evasion.rs"), "src/linked.rs")
        .is_err()
    {
        eprintln!("skipping symlink test: unable to create symlink");
        return;
    }

    let result = discover(crate_dir.path().join("src/lib.rs")).unwrap();
    let linked = find_qualname(&result, "crate::linked");

    match &linked.origin {
        ModuleOrigin::OutsideRoot { path, resolved, .. } => {
            assert!(path.ends_with("src/linked.rs"));
            assert!(resolved.ends_with("evasion.rs"));
        }
        other => panic!("expected OutsideRoot, got {other:?}"),
    }

    let diagnostic = result
        .diagnostics()
        .into_iter()
        .find(|diagnostic| {
            matches!(diagnostic, Diagnostic::OutsideRoot { qualname, .. }
                if qualname == "crate::linked")
        })
        .expect("outside-root diagnostic");
    if let Diagnostic::OutsideRoot { chain, .. } = diagnostic {
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].name, "linked");
    }
}

#[test]
fn allows_symlink_staying_inside_crate_root() {
    let crate_dir = match TempDir::new("md-symlink-inside") {
        Ok(dir) => dir,
        Err(_) => return,
    };
    crate_dir.write("src/lib.rs", "mod linked;\n").unwrap();
    crate_dir
        .write("src/actual.rs", "pub fn inside() {}\n")
        .unwrap();
    if crate_dir
        .symlink_file(&crate_dir.path().join("src/actual.rs"), "src/linked.rs")
        .is_err()
    {
        eprintln!("skipping symlink test: unable to create symlink");
        return;
    }

    let result = discover(crate_dir.path().join("src/lib.rs")).unwrap();
    let linked = find_qualname(&result, "crate::linked");
    assert!(
        matches!(linked.origin, ModuleOrigin::File { .. }),
        "got {:#?}",
        linked.origin
    );
    assert!(
        !result
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, Diagnostic::OutsideRoot { .. }))
    );
}
