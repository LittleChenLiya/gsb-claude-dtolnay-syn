//! Merge adjacent 'use' items that share a visibility by common path
//! prefixes, producing a new syntax tree suitable for code generators.
//!
//! The algorithm first expands every UseTree into flat leaves (one leaf
//! per imported name, glob, or rename), then merges leaves that belong to
//! the same group back into a prefix trie, and finally emits a single
//! ItemUse per group. The meaning of 'self', globs, renames (including
//! 'as _') and absolute paths ('use ::std::...') is preserved.
//!
//! Only 'use' items that form one contiguous run within the same module
//! are candidates for merging; imports are never moved across ordinary
//! code. Items that differ in visibility, attributes (such as #[cfg]),
//! or leading '::' are kept in separate groups. If merging a group would
//! introduce a binding-name collision (for example an explicit name
//! clashing with a rename), the original items are emitted unchanged.
//!
//! # Example
//!
//! ```
//! let mut file = syn::parse_file("use std::io::{self, Read};\nuse std::io::Write;").unwrap();
//! merge_uses::merge_uses(&mut file);
//! assert_eq!(file.items.len(), 1);
//! ```

use proc_macro2::Span;
use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::{Attribute, Ident, Item, ItemUse, UseTree};

/// Merge eligible 'use' items in a parsed file, recursing into inline
/// modules.
pub fn merge_uses(file: &mut syn::File) {
    merge_uses_in_items(&mut file.items);
}

/// Merge eligible 'use' items within one list of module items.
///
/// Contiguous runs of Item::Use are merged within the run only; items
/// before or after non-use code are left in place. Inline modules are
/// processed recursively.
pub fn merge_uses_in_items(items: &mut Vec<Item>) {
    for item in items.iter_mut() {
        if let Item::Mod(item_mod) = item {
            if let Some((_, content)) = &mut item_mod.content {
                merge_uses_in_items(content);
            }
        }
    }

    let mut merged = Vec::with_capacity(items.len());
    let mut run: Vec<ItemUse> = Vec::new();
    for item in items.drain(..) {
        match item {
            Item::Use(item_use) => run.push(item_use),
            other => {
                flush_run(&mut run, &mut merged);
                merged.push(other);
            }
        }
    }
    flush_run(&mut run, &mut merged);
    *items = merged;
}

fn flush_run(run: &mut Vec<ItemUse>, out: &mut Vec<Item>) {
    if run.is_empty() {
        return;
    }

    // Group by (visibility, attributes, leading '::'), preserving the
    // order in which each group first appears.
    let mut groups: Vec<(GroupKey, Vec<ItemUse>)> = Vec::new();
    for item_use in run.drain(..) {
        let key = GroupKey::of(&item_use);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, items)) => items.push(item_use),
            None => groups.push((key, vec![item_use])),
        }
    }

    for (_key, items) in groups {
        if items.len() == 1 {
            out.push(Item::Use(items.into_iter().next().unwrap()));
            continue;
        }
        match merge_group(&items) {
            Some(tree) => {
                let first = &items[0];
                out.push(Item::Use(ItemUse {
                    attrs: first.attrs.clone(),
                    vis: first.vis.clone(),
                    use_token: first.use_token,
                    leading_colon: first.leading_colon,
                    tree,
                    semi_token: first.semi_token,
                }));
            }
            None => out.extend(items.into_iter().map(Item::Use)),
        }
    }
}

#[derive(PartialEq, Eq)]
struct GroupKey {
    vis: String,
    attrs: String,
    absolute: bool,
}

impl GroupKey {
    fn of(item_use: &ItemUse) -> Self {
        GroupKey {
            vis: item_use.vis.to_token_stream().to_string(),
            attrs: attrs_key(&item_use.attrs),
            absolute: item_use.leading_colon.is_some(),
        }
    }
}

fn attrs_key(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .map(|attr| attr.to_token_stream().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One flattened import: the full path from the use root plus what is
/// bound at its end.
#[derive(Clone, PartialEq, Eq)]
struct Leaf {
    segments: Vec<String>,
    kind: LeafKind,
}

#[derive(Clone, PartialEq, Eq)]
enum LeafKind {
    /// 'use a::b;' — binds the last segment as a name.
    Name,
    /// 'use a::b::{self};' — binds the module at the path itself.
    Self_,
    /// 'use a::b as c;' — binds the alias; 'as _' binds nothing.
    Rename(String),
    /// 'use a::b::*;'
    Glob,
}

fn expand(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<Leaf>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            expand(&path.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(name) => {
            if name.ident == "self" {
                out.push(Leaf {
                    segments: prefix.clone(),
                    kind: LeafKind::Self_,
                });
            } else {
                let mut segments = prefix.clone();
                segments.push(name.ident.to_string());
                out.push(Leaf {
                    segments,
                    kind: LeafKind::Name,
                });
            }
        }
        UseTree::Rename(rename) => {
            let mut segments = prefix.clone();
            segments.push(rename.ident.to_string());
            out.push(Leaf {
                segments,
                kind: LeafKind::Rename(rename.rename.to_string()),
            });
        }
        UseTree::Glob(_) => out.push(Leaf {
            segments: prefix.clone(),
            kind: LeafKind::Glob,
        }),
        UseTree::Group(group) => {
            for tree in &group.items {
                expand(tree, prefix, out);
            }
        }
    }
}

/// The name a leaf binds in the importing module, if any. Globs and
/// 'as _' renames bind no name.
fn binding_name(leaf: &Leaf) -> Option<&str> {
    match &leaf.kind {
        LeafKind::Name => leaf.segments.last().map(String::as_str),
        LeafKind::Self_ => leaf.segments.last().map(String::as_str),
        LeafKind::Rename(alias) if alias != "_" => Some(alias),
        LeafKind::Rename(_) | LeafKind::Glob => None,
    }
}

/// Prefix trie used to rebuild a merged UseTree from expanded leaves.
#[derive(Default)]
struct Node {
    /// The module at this node is imported itself ('self' or plain name).
    self_leaf: bool,
    glob: bool,
    /// (original, alias) renames of direct children, in first-seen order.
    renames: Vec<(String, String)>,
    /// Child segments in first-seen order.
    children: Vec<(String, Node)>,
}

impl Node {
    fn child(&mut self, segment: &str) -> &mut Node {
        match self.children.iter().position(|(name, _)| name == segment) {
            Some(i) => &mut self.children[i].1,
            None => {
                self.children.push((segment.to_owned(), Node::default()));
                &mut self.children.last_mut().unwrap().1
            }
        }
    }

    fn insert(&mut self, leaf: &Leaf) {
        match &leaf.kind {
            LeafKind::Name | LeafKind::Self_ => {
                let mut node = self;
                for segment in &leaf.segments {
                    node = node.child(segment);
                }
                node.self_leaf = true;
            }
            LeafKind::Glob => {
                let mut node = self;
                for segment in &leaf.segments {
                    node = node.child(segment);
                }
                node.glob = true;
            }
            LeafKind::Rename(alias) => {
                let (parent, name) = leaf.segments.split_at(leaf.segments.len() - 1);
                let mut node = self;
                for segment in parent {
                    node = node.child(segment);
                }
                let entry = (name[0].clone(), alias.clone());
                if !node.renames.contains(&entry) {
                    node.renames.push(entry);
                }
            }
        }
    }

    /// Emit the contents of this node as use-tree entries.
    fn entries(&self) -> Vec<UseTree> {
        let mut entries = Vec::new();
        if self.self_leaf {
            entries.push(UseTree::Name(syn::UseName {
                ident: Ident::new("self", Span::call_site()),
            }));
        }
        for (name, child) in &self.children {
            let ident = Ident::new(name, Span::call_site());
            let child_entries = child.entries();
            // A child that only imports itself is a plain name, not
            // 'name::self'.
            let only_self = child.self_leaf && child_entries.len() == 1;
            if child_entries.is_empty() || only_self {
                entries.push(UseTree::Name(syn::UseName { ident }));
            } else {
                entries.push(UseTree::Path(syn::UsePath {
                    ident,
                    colon2_token: Default::default(),
                    tree: Box::new(group_or_single(child_entries)),
                }));
            }
        }
        if self.glob {
            entries.push(UseTree::Glob(syn::UseGlob {
                star_token: Default::default(),
            }));
        }
        for (original, alias) in &self.renames {
            entries.push(UseTree::Rename(syn::UseRename {
                ident: Ident::new(original, Span::call_site()),
                as_token: Default::default(),
                rename: Ident::new(alias, Span::call_site()),
            }));
        }
        entries
    }
}

/// 'a::{b}' collapses to 'a::b'; anything else stays braced.
fn group_or_single(mut entries: Vec<UseTree>) -> UseTree {
    if entries.len() == 1 {
        entries.pop().unwrap()
    } else {
        UseTree::Group(syn::UseGroup {
            brace_token: Default::default(),
            items: entries.into_iter().collect::<Punctuated<_, _>>(),
        })
    }
}

/// Try to merge one group of same-key 'use' items into a single tree.
/// Returns None if merging would change the meaning of the imports.
fn merge_group(items: &[ItemUse]) -> Option<UseTree> {
    let mut leaves: Vec<Leaf> = Vec::new();
    for item_use in items {
        expand(&item_use.tree, &mut Vec::new(), &mut leaves);
    }

    // Refuse to merge if two distinct explicit leaves would bind the same
    // name; globs and 'as _' bind nothing or yield to explicit names.
    let mut bindings: Vec<(&str, &Leaf)> = Vec::new();
    for leaf in &leaves {
        if let Some(name) = binding_name(leaf) {
            if bindings
                .iter()
                .any(|(bound, other)| *bound == name && *other != leaf)
            {
                return None;
            }
            bindings.push((name, leaf));
        }
    }

    let mut root = Node::default();
    for leaf in &leaves {
        root.insert(leaf);
    }

    let entries = root.entries();
    if entries.is_empty() {
        return None;
    }
    Some(group_or_single(entries))
}

/// Expand every 'use' item in a file (recursing into inline modules) into
/// a sorted list of human-readable leaf descriptions. Two files whose
/// expansions are equal import the same set of names; used by tests to
/// verify that merging preserves meaning.
pub fn expand_file_imports(file: &syn::File) -> Vec<String> {
    let mut out = Vec::new();
    expand_items(&file.items, &mut out);
    out.sort();
    out.dedup();
    out
}

fn expand_items(items: &[Item], out: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Use(item_use) => {
                let mut leaves = Vec::new();
                expand(&item_use.tree, &mut Vec::new(), &mut leaves);
                for leaf in leaves {
                    let mut desc = String::new();
                    if item_use.leading_colon.is_some() {
                        desc.push_str("::");
                    }
                    desc.push_str(&leaf.segments.join("::"));
                    match &leaf.kind {
                        LeafKind::Name => {}
                        // 'use a::b::{self};' and 'use a::b;' bind the
                        // same name, so both normalize to the path.
                        LeafKind::Self_ => {}
                        LeafKind::Rename(alias) => {
                            desc.push_str(" as ");
                            desc.push_str(alias);
                        }
                        LeafKind::Glob => desc.push_str("::*"),
                    }
                    out.push(desc);
                }
            }
            Item::Mod(item_mod) => {
                if let Some((_, content)) = &item_mod.content {
                    expand_items(content, out);
                }
            }
            _ => {}
        }
    }
}
