use consolidate_use::{consolidate, expand, Leaf};
use quote::ToTokens;
use std::collections::BTreeSet;
use syn::{File, Item, ItemUse, UseTree, Visibility};

/// Parse a source file, consolidate it, print it back, and reparse.
fn consolidate_twice(source: &str) -> File {
    let file: File = syn::parse_str(source).unwrap();
    let consolidated = consolidate(&file);
    let printed = consolidated
        .items
        .iter()
        .map(|item| item.to_token_stream().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    syn::parse_str(&printed).unwrap_or_else(|err| {
        panic!("consolidated output failed to parse:\n{printed}\nerror: {err}")
    })
}

/// Canonical representation of one expanded import.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    absolute: bool,
    vis: String,
    attrs: String,
    path: Vec<String>,
    kind: String,
}

fn entries_of(file: &File) -> BTreeSet<Entry> {
    let mut entries = BTreeSet::new();
    for item in &file.items {
        if let Item::Use(item_use) = item {
            push_entries(item_use, &mut entries);
        }
    }
    entries
}

fn push_entries(item_use: &ItemUse, entries: &mut BTreeSet<Entry>) {
    let vis = item_use
        .vis
        .to_token_stream()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let attrs = item_use
        .attrs
        .iter()
        .map(|attr| attr.to_token_stream().to_string())
        .collect::<String>();
    let absolute = item_use.leading_colon.is_some();
    for leaf in expand(&item_use.tree) {
        let entry = match leaf {
            Leaf::Name { path } => Entry {
                absolute,
                vis: vis.clone(),
                attrs: attrs.clone(),
                path,
                kind: "name".to_owned(),
            },
            Leaf::Rename { path, alias } => Entry {
                absolute,
                vis: vis.clone(),
                attrs: attrs.clone(),
                path,
                kind: format!("rename:{alias}"),
            },
            Leaf::Glob { path } => Entry {
                absolute,
                vis: vis.clone(),
                attrs: attrs.clone(),
                path,
                kind: "glob".to_owned(),
            },
        };
        entries.insert(entry);
    }
}

fn assert_equivalent(before: &str) -> File {
    let before_file: File = syn::parse_str(before).unwrap();
    let expected = entries_of(&before_file);
    let after_file = consolidate_twice(before);
    let actual = entries_of(&after_file);
    assert_eq!(
        expected,
        actual,
        "expanded imports differ after consolidation\
        \n--- before ---\
        {before}\n--- after ---\
        {after_text}",
        after_text = printed(&after_file),
    );
    after_file
}

fn use_trees(file: &File) -> Vec<UseTree> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use.tree.clone()),
            _ => None,
        })
        .collect()
}

fn printed(file: &File) -> String {
    file.items
        .iter()
        .map(|item| item.to_token_stream().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn merges_std_io_with_self() {
    // The headline case from the task: `self` must survive the merge.
    let after = assert_equivalent("use std::io::{self, Read};\nuse std::io::Write;");

    assert_eq!(after.items.len(), 1);
    let tree = match &after.items[0] {
        Item::Use(item_use) => &item_use.tree,
        other => panic!("expected use item, got {}", other.to_token_stream()),
    };
    let text = tree.to_token_stream().to_string();
    assert_eq!(text, "std :: io :: { self , Read , Write }");

    // Every original leaf is still present after re-expansion.
    let leaves = expand(tree);
    assert!(leaves.contains(&Leaf::Name {
        path: vec!["std".into(), "io".into()]
    }));
    assert!(leaves.contains(&Leaf::Name {
        path: vec!["std".into(), "io".into(), "Read".into()]
    }));
    assert!(leaves.contains(&Leaf::Name {
        path: vec!["std".into(), "io".into(), "Write".into()]
    }));
}

#[test]
fn keeps_pub_and_private_separate() {
    let after = assert_equivalent(
        "use std::io::Read;\
         pub use std::io::Write;\
         use std::io::Error;",
    );
    let trees = use_trees(&after);
    assert_eq!(trees.len(), 2);
    let private = after
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item_use) if matches!(item_use.vis, Visibility::Inherited) => Some(item_use),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(private.len(), 1);
    assert_eq!(
        private[0].tree.to_token_stream().to_string(),
        "std :: io :: { Read , Error }"
    );
}

#[test]
fn keeps_cfg_attributes_together_but_separate_runs() {
    let after = assert_equivalent(
        "#[cfg(feature = \"a\")] use std::io::Read;\
         #[cfg(feature = \"a\")] use std::io::Write;\
         #[cfg(feature = \"b\")] use std::io::Error;",
    );
    let trees = use_trees(&after);
    assert_eq!(trees.len(), 2);
    let uses = after
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(uses[0].attrs.len(), 1);
    assert_eq!(
        uses[0].tree.to_token_stream().to_string(),
        "std :: io :: { Read , Write }"
    );
    assert_eq!(
        uses[1].tree.to_token_stream().to_string(),
        "std :: io :: Error"
    );
}

#[test]
fn does_not_cross_non_use_items() {
    let source = "use std::io::Read;\
                 fn marker() {}\n
                 use std::io::Write;";
    let after = assert_equivalent(source);
    assert_eq!(after.items.len(), 3);
    assert!(matches!(after.items[1], Item::Fn(_)));
    assert!(matches!(after.items[0], Item::Use(_)));
    assert!(matches!(after.items[2], Item::Use(_)));
}

#[test]
fn as_underscore_and_renames() {
    let after = assert_equivalent(
        "use std::io::Read as _;\
         use std::io::Write as W;\
         use std::io::Error as E;",
    );
    let text = printed(&after);
    assert_eq!(
        text,
        "use std :: io :: { Read as _ , Write as W , Error as E } ;"
    );
}

#[test]
fn absolute_paths_keep_leading_colon_and_do_not_merge_with_relative() {
    let after = assert_equivalent(
        "use ::std::io::Read;\
         use ::std::io::Write;\
         use std::io::Error;",
    );
    let trees = use_trees(&after);
    assert_eq!(trees.len(), 2);
    let uses = after
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(uses[0].leading_colon.is_some());
    assert_eq!(
        uses[0].tree.to_token_stream().to_string(),
        "std :: io :: { Read , Write }"
    );
    assert!(uses[1].leading_colon.is_none());
    assert_eq!(
        uses[1].tree.to_token_stream().to_string(),
        "std :: io :: Error"
    );
}

#[test]
fn glob_and_rename_collisions_keep_distinct_meanings() {
    // A glob, `self`, a rename of the module itself, and `as _` collide on
    // the same prefix but keep distinct meanings.
    let after = assert_equivalent(
        "use std::io::*;\
         use std::io::{self, self as Io, Read as _, Error as E};",
    );
    let text = printed(&after);
    assert_eq!(
        text,
        "use std :: io :: { self , self as Io , * , Read as _ , Error as E } ;"
    );
    let tree = &use_trees(&after)[0];
    let leaves = expand(tree);
    assert!(leaves.contains(&Leaf::Glob {
        path: vec!["std".into(), "io".into()]
    }));
    assert!(leaves.contains(&Leaf::Name {
        path: vec!["std".into(), "io".into()]
    }));
    assert!(leaves.contains(&Leaf::Rename {
        path: vec!["std".into(), "io".into()],
        alias: "Io".into()
    }));
    assert!(leaves.contains(&Leaf::Rename {
        path: vec!["std".into(), "io".into(), "Error".into()],
        alias: "E".into()
    }));
    assert!(leaves.contains(&Leaf::Rename {
        path: vec!["std".into(), "io".into(), "Read".into()],
        alias: "_".into()
    }));
}

#[test]
fn distinct_top_level_prefixes_stay_separate() {
    let after = assert_equivalent(
        "use std::io::*;\
         use std::io::Read;\
         use core::fmt::Display;",
    );
    let trees = use_trees(&after);
    assert_eq!(trees.len(), 2);
    let text = printed(&after);
    assert_eq!(
        text,
        "use std :: io :: { * , Read } ;\nuse core :: fmt :: Display ;"
    );
}

#[test]
fn self_in_middle_and_end_is_canonical() {
    // Both forms denote the module itself; the merged output must still
    // contain exactly one name leaf for `a::b`.
    let after = assert_equivalent(
        "use a::b::self;\
         use a::b::{self, c};",
    );
    let text = printed(&after);
    assert_eq!(text, "use a :: b :: { self , c } ;");
}

#[test]
fn nested_modules_are_consolidated_independently() {
    let source = "mod m { use std::io::Read; use std::io::Write; }\nuse std::fmt::Debug;";
    let after = assert_equivalent(source);
    let mod_item = match &after.items[0] {
        Item::Mod(item_mod) => item_mod,
        other => panic!("expected mod, got {}", other.to_token_stream()),
    };
    let inner = &mod_item.content.as_ref().unwrap().1;
    assert_eq!(inner.len(), 1);
    let inner_tree = match &inner[0] {
        Item::Use(item_use) => &item_use.tree,
        other => panic!("expected use, got {}", other.to_token_stream()),
    };
    assert_eq!(
        inner_tree.to_token_stream().to_string(),
        "std :: io :: { Read , Write }"
    );
}

#[test]
fn already_merged_use_is_stable() {
    let source = "use std::io::{self, Read, Write};";
    let after = assert_equivalent(source);
    assert_eq!(
        printed(&after),
        "use std :: io :: { self , Read , Write } ;"
    );
}

#[test]
fn multiple_aliases_of_same_item_all_survive() {
    let after = assert_equivalent(
        "use std::io::Error as E;\
         use std::io::Error as Err;",
    );
    let text = printed(&after);
    assert_eq!(text, "use std :: io :: { Error as E , Error as Err } ;");
}

#[test]
fn duplicate_imports_are_canonicalized_not_duplicated() {
    // Repeating the same exact import is E0252 for names; consolidation
    // emits it once, which has the same expanded-import meaning.
    let after = assert_equivalent(
        "use std::io::Read;\
         use std::io::Read;\
         use std::io::*;\
         use std::io::*;",
    );
    let text = printed(&after);
    assert_eq!(text, "use std :: io :: { * , Read } ;");
}

#[test]
fn root_self_rename_is_printed_literally() {
    let after = assert_equivalent("use self as root;");
    assert_eq!(printed(&after), "use self as root ;");
}

#[test]
fn multiple_attributes_are_part_of_signature() {
    let after = assert_equivalent(
        "#[cfg(unix)] #[allow(unused)] use std::io::Read;\
         #[cfg(unix)] #[allow(unused)] use std::io::Write;\
         #[cfg(unix)] use std::io::Error;",
    );
    assert_eq!(use_trees(&after).len(), 2);
    let uses = after
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(uses[0].attrs.len(), 2);
    assert_eq!(uses[1].attrs.len(), 1);
}
