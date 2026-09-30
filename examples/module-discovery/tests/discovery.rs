use std::fs;
use std::path::{Path, PathBuf};

use module_discovery::{discover, Diagnostic, ModuleKind, ModuleTree};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = format!(
            "module-discovery-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

fn child_names(tree: &ModuleTree, id: usize) -> Vec<String> {
    let mut names: Vec<String> = tree
        .children(id)
        .map(|child| tree.module(child).name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn root_inline_and_path_modules() {
    let tmp = TempDir::new("basic");
    let lib_source = concat!(
        "mod foo;\n",
        "mod wrapper {\n",
        "    pub mod inner;\n",
        "}\n",
        "#[path = \"renamed/real.rs\"]\n",
        "mod relocated;\n",
    );
    let lib = tmp.write("lib.rs", lib_source);
    let foo_source = "mod nested;\n";
    tmp.write("foo.rs", foo_source);
    tmp.write("foo/nested.rs", "");
    tmp.write("wrapper/inner.rs", "");
    tmp.write("renamed/real.rs", "mod deep;\n");
    tmp.write("renamed/real/deep.rs", "");

    let tree = discover(&lib);
    assert!(tree.diagnostics.is_empty(), "{:#?}", tree.diagnostics);

    let dir = canonical(&lib).parent().unwrap().to_path_buf();

    // Every module resolves to its expected file.
    let foo = tree.module_by_name("foo").unwrap();
    assert_eq!(foo.kind, ModuleKind::File);
    assert_eq!(foo.file.as_deref(), Some(dir.join("foo.rs").as_path()));
    assert_eq!(foo.file_range, Some(0..foo_source.len()));

    let nested = tree.module_by_name("nested").unwrap();
    assert_eq!(nested.file.as_deref(), Some(dir.join("foo/nested.rs").as_path()));

    let wrapper = tree.module_by_name("wrapper").unwrap();
    assert_eq!(wrapper.kind, ModuleKind::Inline);
    assert_eq!(wrapper.file.as_deref(), Some(canonical(&lib).as_path()));

    let inner = tree.module_by_name("inner").unwrap();
    assert_eq!(inner.file.as_deref(), Some(dir.join("wrapper/inner.rs").as_path()));

    let relocated = tree.module_by_name("relocated").unwrap();
    assert_eq!(relocated.file.as_deref(), Some(dir.join("renamed/real.rs").as_path()));

    let deep = tree.module_by_name("deep").unwrap();
    assert_eq!(deep.file.as_deref(), Some(dir.join("renamed/real/deep.rs").as_path()));

    // Declaration edges shape the module tree.
    assert_eq!(child_names(&tree, 0), ["foo", "relocated", "wrapper"]);
    assert_eq!(child_names(&tree, foo.id), ["nested"]);
    assert_eq!(child_names(&tree, wrapper.id), ["inner"]);
    assert_eq!(child_names(&tree, relocated.id), ["deep"]);

    // The item range points at the `mod` item in the parent source.
    let range = foo.item_range.clone().unwrap();
    assert_eq!(&lib_source[range], "mod foo;");
}

#[test]
fn ambiguous_foo_rs_and_mod_rs() {
    let tmp = TempDir::new("ambiguous");
    let lib = tmp.write("lib.rs", "mod foo;\n");
    tmp.write("foo.rs", "");
    tmp.write("foo/mod.rs", "");

    let tree = discover(&lib);
    let foo = tree.module_by_name("foo").unwrap();
    assert!(foo.file.is_none());

    let dir = canonical(&lib).parent().unwrap().to_path_buf();
    let ambiguities: Vec<_> = tree
        .diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::Ambiguous {
                module,
                chain,
                candidates,
            } => Some((*module, chain.clone(), candidates.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ambiguities.len(), 1, "{:#?}", tree.diagnostics);
    let (module, chain, candidates) = &ambiguities[0];
    assert_eq!(*module, foo.id);
    assert_eq!(*chain, vec![canonical(&lib)]);
    assert_eq!(candidates[0], dir.join("foo.rs"));
    assert_eq!(candidates[1], dir.join("foo/mod.rs"));
}

#[test]
fn missing_module_reports_full_chain() {
    let tmp = TempDir::new("missing");
    let lib = tmp.write("lib.rs", "mod a;\n");
    tmp.write("a.rs", "mod b;\n");
    tmp.write("a/b.rs", "mod c;\n");

    let tree = discover(&lib);
    let missing: Vec<_> = tree
        .diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::Missing {
                module,
                chain,
                searched,
            } => Some((*module, chain.clone(), searched.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(missing.len(), 1, "{:#?}", tree.diagnostics);
    let (module, chain, searched) = &missing[0];
    assert_eq!(tree.module(*module).name, "c");

    // The chain walks every source file from the crate root down to the
    // file containing the dangling declaration.
    let dir = canonical(&lib).parent().unwrap().to_path_buf();
    assert_eq!(
        *chain,
        vec![dir.join("lib.rs"), dir.join("a.rs"), dir.join("a/b.rs")],
    );
    for link in chain {
        assert!(link.is_file(), "chain link missing: {}", link.display());
    }
    assert_eq!(
        *searched,
        vec![dir.join("a/b/c.rs"), dir.join("a/b/c/mod.rs")],
    );
}

#[test]
fn cyclic_path_inclusion_reports_chain() {
    let tmp = TempDir::new("cycle");
    let lib = tmp.write("lib.rs", "mod a;\n");
    tmp.write("a.rs", "#[path = \"b.rs\"]\nmod b;\n");
    tmp.write("a/b.rs", "#[path = \"../../a.rs\"]\nmod again;\n");

    let tree = discover(&lib);
    let cycles: Vec<_> = tree
        .diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::Cycle { module, chain } => Some((*module, chain.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(cycles.len(), 1, "{:#?}", tree.diagnostics);
    let (module, chain) = &cycles[0];
    assert_eq!(tree.module(*module).name, "again");

    let dir = canonical(&lib).parent().unwrap().to_path_buf();
    assert_eq!(
        *chain,
        vec![
            dir.join("lib.rs"),
            dir.join("a.rs"),
            dir.join("a/b.rs"),
            dir.join("a.rs"),
        ],
    );
}

#[test]
fn chinese_file_names() {
    let tmp = TempDir::new("chinese");
    let lib = tmp.write(
        "lib.rs",
        "mod \u{6a21}\u{5757};\n#[path = \"\u{6570}\u{636e}.rs\"]\nmod data;\n",
    );
    tmp.write("\u{6a21}\u{5757}.rs", "");
    tmp.write("\u{6570}\u{636e}.rs", "");

    let tree = discover(&lib);
    assert!(tree.diagnostics.is_empty(), "{:#?}", tree.diagnostics);

    let dir = canonical(&lib).parent().unwrap().to_path_buf();
    let module = tree.module_by_name("\u{6a21}\u{5757}").unwrap();
    assert_eq!(
        module.file.as_deref(),
        Some(dir.join("\u{6a21}\u{5757}.rs").as_path()),
    );
    let data = tree.module_by_name("data").unwrap();
    assert_eq!(data.file.as_deref(), Some(dir.join("\u{6570}\u{636e}.rs").as_path()));
}

#[test]
fn conditional_modules_are_kept_unevaluated() {
    let tmp = TempDir::new("cfg");
    let lib = tmp.write(
        "lib.rs",
        concat!(
            "#[cfg(feature = \"extra\")]\n",
            "mod conditional;\n",
            "#[cfg(all())]\n",
            "mod never_enabled;\n",
            "#[cfg(any(unix, windows))]\n",
            "mod inline_cond {\n",
            "    pub mod child;\n",
            "}\n",
        ),
    );
    tmp.write("conditional.rs", "");
    tmp.write("never_enabled.rs", "");
    tmp.write("inline_cond/child.rs", "");

    let tree = discover(&lib);
    assert!(tree.diagnostics.is_empty(), "{:#?}", tree.diagnostics);

    // Conditions are recorded verbatim, never evaluated.
    let conditional = tree.module_by_name("conditional").unwrap();
    assert_eq!(conditional.cfg.len(), 1);
    assert_eq!(conditional.cfg[0].replace(' ', ""), "feature=\"extra\"");
    assert!(conditional.file.is_some());

    // Even a condition that is certainly false keeps its module node.
    let never_enabled = tree.module_by_name("never_enabled").unwrap();
    assert_eq!(never_enabled.cfg[0].replace(' ', ""), "all()");
    assert!(never_enabled.file.is_some());

    // Inline conditional modules are discovered through as well.
    let inline_cond = tree.module_by_name("inline_cond").unwrap();
    assert_eq!(inline_cond.kind, ModuleKind::Inline);
    assert_eq!(child_names(&tree, inline_cond.id), ["child"]);
}

#[test]
fn out_of_bounds_symlink_is_reported() {
    let tmp = TempDir::new("symlink");
    fs::create_dir_all(tmp.0.join("outside")).unwrap();
    tmp.write("outside/secret.rs", "mod inner;\n");
    fs::create_dir_all(tmp.0.join("crate")).unwrap();
    let lib = tmp.write("crate/lib.rs", "#[path = \"link.rs\"]\nmod escaped;\n");

    let target = tmp.0.join("outside/secret.rs");
    let link = tmp.0.join("crate/link.rs");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&target, &link);
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&target, &link);
    if let Err(error) = linked {
        eprintln!("symlink creation not permitted, skipping: {error}");
        return;
    }

    let tree = discover(&lib);
    let escaped = tree.module_by_name("escaped").unwrap();
    let out_of_bounds: Vec<_> = tree
        .diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::OutOfBounds { module, chain, path } => {
                Some((*module, chain.clone(), path.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(out_of_bounds.len(), 1, "{:#?}", tree.diagnostics);
    let (module, chain, path) = &out_of_bounds[0];
    assert_eq!(*module, escaped.id);
    assert_eq!(*chain, vec![canonical(&lib)]);
    assert!(path.ends_with("link.rs"));

    // The escaped file is not followed, so its contents stay undiscovered.
    assert!(tree.module_by_name("inner").is_none());
}
