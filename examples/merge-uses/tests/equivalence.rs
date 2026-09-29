use quote::ToTokens;

/// Parse source, merge uses, print the new AST, re-parse it, and return
/// both the re-parsed file and its compact token-string form.
fn merge_and_reparse(src: &str) -> (syn::File, String) {
    let mut file = syn::parse_file(src).unwrap();
    merge_uses::merge_uses(&mut file);
    let printed = file.to_token_stream().to_string();
    let reparsed = syn::parse_file(&printed).unwrap();
    (reparsed, printed)
}

/// Every import of the original source must still be present, with the
/// same meaning, after merging and re-parsing.
fn assert_equivalent(src: &str) -> (syn::File, String) {
    let original = syn::parse_file(src).unwrap();
    let (reparsed, printed) = merge_and_reparse(src);
    assert_eq!(
        merge_uses::expand_file_imports(&original),
        merge_uses::expand_file_imports(&reparsed),
        "merged output lost or changed an import:\\n{printed}"
    );
    (reparsed, printed)
}

fn compact(tokens: &str) -> String {
    tokens.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn use_item_count(file: &syn::File) -> usize {
    file.items
        .iter()
        .filter(|item| matches!(item, syn::Item::Use(_)))
        .count()
}

#[test]
fn merges_self_and_sibling() {
    let src = "use std::io::{self, Read};\nuse std::io::Write;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    assert_eq!(compact(&printed), "usestd::io::{self,Read,Write};");
}

#[test]
fn pub_and_private_stay_separate() {
    let src = "use a::b::C;\npub use a::b::D;\npub(crate) use a::b::E;\npub use a::b::F;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 3);
    let compact = compact(&printed);
    assert!(compact.contains("usea::b::C;"));
    assert!(compact.contains("pubusea::b::{D,F};"));
    assert!(compact.contains("pub(crate)usea::b::E;"));
}

#[test]
fn cfg_attributes_partition_groups() {
    let src = "#[cfg(unix)] use a::b;\n#[cfg(unix)] use a::c;\n#[cfg(windows)] use a::d;\nuse a::e;\nuse a::f;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 3);
    let compact = compact(&printed);
    assert!(compact.contains("#[cfg(unix)]usea::{b,c};"));
    assert!(compact.contains("#[cfg(windows)]usea::d;"));
    assert!(compact.contains("usea::{e,f};"));
}

#[test]
fn rename_to_underscore_is_preserved() {
    let src = "use a::b as _;\nuse a::c;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    assert_eq!(compact(&printed), "usea::{c,bas_};");
}

#[test]
fn absolute_paths_merge_with_leading_colon() {
    let src = "use ::std::io::Read;\nuse ::std::io::Write;\nuse std::fmt::Debug;\nuse std::fmt::Display;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 2);
    let compact = compact(&printed);
    assert!(compact.contains("use::std::io::{Read,Write};"));
    assert!(compact.contains("usestd::fmt::{Debug,Display};"));
}

#[test]
fn glob_merges_with_explicit_names() {
    let src = "use a::b::*;\nuse a::b::c;\nuse a::b::d as e;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    assert_eq!(compact(&printed), "usea::b::{c,*,dase};");
}

#[test]
fn rename_collision_is_not_merged() {
    // 'b as c' and 'c' would bind the same name; merging them into one
    // statement must not happen, and the originals are kept verbatim.
    let src = "use a::b as c;\nuse a::c;\n";
    let (file, _) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 2);
}

#[test]
fn glob_and_rename_collision_stays_unmerged() {
    // Distinct paths binding the same name through a rename: kept apart.
    let src = "use a::b;\nuse c::d as b;\n";
    let (file, _) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 2);
}

#[test]
fn does_not_move_imports_across_code() {
    let src = "use a::b;\nfn f() {}\nuse a::c;\n";
    let (file, _) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 2);
    assert!(matches!(file.items[0], syn::Item::Use(_)));
    assert!(matches!(file.items[1], syn::Item::Fn(_)));
    assert!(matches!(file.items[2], syn::Item::Use(_)));
}

#[test]
fn merges_inside_inline_modules_independently() {
    let src = "mod m {\nuse a::b;\nuse a::c;\n}\nuse a::d;\nuse a::e;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    let compact = compact(&printed);
    assert!(compact.contains("modm{usea::{b,c};}"));
    assert!(compact.contains("usea::{d,e};"));
}

#[test]
fn nested_prefixes_and_self_collapse() {
    let src = "use a::b;\nuse a::b::c;\nuse a::b::c::d;\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    assert_eq!(compact(&printed), "usea::b::{self,c::{self,d}};");
}

#[test]
fn duplicate_imports_dedup() {
    let src = "use a::b;\nuse a::{b, c};\n";
    let (file, printed) = assert_equivalent(src);
    assert_eq!(use_item_count(&file), 1);
    assert_eq!(compact(&printed), "usea::{b,c};");
}
