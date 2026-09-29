//! Consolidate consecutive `use` declarations by common path prefix.
//!
//! The transformation operates on [`syn::File`]:
//! 1. Every [`ItemUse`] in a contiguous run with the same signature is fully
//!    expanded into individual leaf imports. `self`, globs (`*`) and renames
//!    (`as`, including `as _`) all keep their literal meaning.
//! 2. The leaves are inserted into a path trie, one node per path segment.
//! 3. The trie is turned back into a new list of [`ItemUse`]s sharing the
//!    visibility, attributes and leading `::` of the originating run.
//!
//! Only adjacent `use` statements within the same module with identical
//! visibility and attributes are merged; imports never cross intervening
//! non-`use` items.

use proc_macro2::Ident;
use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::token::Brace;
use syn::{File, Item, ItemMod, ItemUse, UseGlob, UseGroup, UseName, UsePath, UseRename, UseTree};

/// Consolidate mergeable consecutive `use` items of a file in place.
pub fn consolidate_file(file: &mut File) {
    consolidate_items(&mut file.items);
}

/// Return a new file in which mergeable consecutive `use` items are
/// consolidated.
pub fn consolidate(file: &File) -> File {
    let mut file = file.clone();
    consolidate_file(&mut file);
    file
}

fn consolidate_items(items: &mut Vec<Item>) {
    let mut output: Vec<Item> = Vec::with_capacity(items.len());
    // A segment is a maximal run of adjacent `use` items (possibly empty
    // between non-use items). Within one segment, uses with the same
    // signature are merged; non-use items terminate the segment, so imports
    // never move across real code.
    let mut segment: Vec<ItemUse> = Vec::new();

    for item in items.drain(..) {
        match item {
            Item::Use(item_use) => {
                segment.push(item_use);
            }
            Item::Mod(item_mod) => {
                flush_segment(&mut output, std::mem::take(&mut segment));
                output.push(Item::Mod(consolidate_mod(item_mod)));
            }
            other => {
                flush_segment(&mut output, std::mem::take(&mut segment));
                output.push(other);
            }
        }
    }
    flush_segment(&mut output, segment);
    *items = output;
}

fn flush_segment(output: &mut Vec<Item>, segment: Vec<ItemUse>) {
    // Preserve the original order of distinct signatures; merge every use
    // sharing a signature into that signature's run, even if other
    // signatures' uses appear in between (they are still adjacent use items,
    // not intervening code).
    let mut order: Vec<Signature> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    for item_use in segment {
        let signature = Signature::new(&item_use);
        match order.iter().position(|existing| existing == &signature) {
            Some(index) => runs[index].uses.push(item_use),
            None => {
                order.push(signature);
                runs.push(Run::new(order.last().unwrap().clone(), item_use));
            }
        }
    }
    for run in runs {
        emit(output, run);
    }
}

fn consolidate_mod(mut item_mod: ItemMod) -> ItemMod {
    if let Some((_, items)) = item_mod.content.as_mut() {
        consolidate_items(items);
    }
    item_mod
}

struct Run {
    signature: Signature,
    uses: Vec<ItemUse>,
}

impl Run {
    fn new(signature: Signature, item_use: ItemUse) -> Self {
        Run {
            signature,
            uses: vec![item_use],
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Signature {
    leading_colon: bool,
    vis: String,
    attrs: String,
}

impl Signature {
    fn new(item_use: &ItemUse) -> Self {
        Signature {
            leading_colon: item_use.leading_colon.is_some(),
            vis: item_use.vis.to_token_stream().to_string(),
            attrs: item_use
                .attrs
                .iter()
                .map(|attr| attr.to_token_stream().to_string())
                .collect::<String>(),
        }
    }
}

fn emit(output: &mut Vec<Item>, run: Run) {
    let representative = run.uses.first().expect("run contains at least one use");
    let attrs = representative.attrs.clone();
    let vis = representative.vis.clone();
    let leading_colon = run.signature.leading_colon;

    let mut root = Node::default();
    for item_use in &run.uses {
        for leaf in expand(&item_use.tree) {
            root.insert(leaf);
        }
    }

    for tree in root.build_root() {
        output.push(Item::Use(ItemUse {
            attrs: attrs.clone(),
            vis: vis.clone(),
            use_token: Default::default(),
            leading_colon: if leading_colon {
                Some(Default::default())
            } else {
                None
            },
            tree,
            semi_token: Default::default(),
        }));
    }
}

/// A fully expanded individual leaf import.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Leaf {
    /// `std::io`, including a trailing `self`.
    Name { path: Vec<String> },
    /// `std::io::Read as R`, including `as _` and `self as M`.
    Rename { path: Vec<String>, alias: String },
    /// `std::io::*`.
    Glob { path: Vec<String> },
}

/// Expand a use tree into individual [`Leaf`]s.
///
/// A leading `self` segment and a trailing `self` name both denote the
/// enclosing module, so they are normalized away; every other segment,
/// rename and glob is kept literally.
pub fn expand(tree: &UseTree) -> Vec<Leaf> {
    let mut leaves = Vec::new();
    flatten(tree, &mut Vec::new(), &mut leaves);
    leaves
}

fn flatten(tree: &UseTree, path: &mut Vec<String>, leaves: &mut Vec<Leaf>) {
    match tree {
        UseTree::Path(use_path) => {
            if use_path.ident != "self" {
                path.push(use_path.ident.to_string());
            }
            flatten(&use_path.tree, path, leaves);
            if use_path.ident != "self" {
                path.pop();
            }
        }
        UseTree::Name(use_name) => {
            if use_name.ident != "self" {
                path.push(use_name.ident.to_string());
            }
            leaves.push(Leaf::Name { path: path.clone() });
            if use_name.ident != "self" {
                path.pop();
            }
        }
        UseTree::Rename(use_rename) => {
            if use_rename.ident != "self" {
                path.push(use_rename.ident.to_string());
            }
            leaves.push(Leaf::Rename {
                path: path.clone(),
                alias: use_rename.rename.to_string(),
            });
            if use_rename.ident != "self" {
                path.pop();
            }
        }
        UseTree::Glob(_) => leaves.push(Leaf::Glob { path: path.clone() }),
        UseTree::Group(group) => {
            for item in group.items.iter() {
                flatten(item, path, leaves);
            }
        }
    }
}

#[derive(Default, Debug)]
struct Node {
    // Markers are deduplicated: importing the same item twice is an error for
    // names and a no-op for globs, so one entry preserves every distinct
    // meaning the original imports had.
    has_name: bool,
    rename_aliases: Vec<String>,
    has_glob: bool,
    children: Vec<(String, Node)>,
}

impl Node {
    fn child_mut(&mut self, segment: &str) -> &mut Node {
        let position = self.children.iter().position(|(name, _)| name == segment);
        match position {
            Some(index) => &mut self.children[index].1,
            None => {
                self.children.push((segment.to_owned(), Node::default()));
                &mut self.children.last_mut().unwrap().1
            }
        }
    }

    fn insert(&mut self, leaf: Leaf) {
        let (path, kind) = match leaf {
            Leaf::Name { path } => (path, LeafKind::Name),
            Leaf::Rename { path, alias } => (path, LeafKind::Rename(alias)),
            Leaf::Glob { path } => (path, LeafKind::Glob),
        };
        let mut node = self;
        for segment in &path {
            node = node.child_mut(segment);
        }
        match kind {
            LeafKind::Name => node.has_name = true,
            LeafKind::Glob => node.has_glob = true,
            LeafKind::Rename(alias) => {
                if !node.rename_aliases.contains(&alias) {
                    node.rename_aliases.push(alias);
                }
            }
        }
    }

    /// Build the use trees rooted at the (virtual) root node.
    ///
    /// Each first-level segment becomes its own `use` item. Unlike deeper
    /// nodes, the root is never wrapped in a braced group, so unrelated
    /// first-level prefixes stay as separate statements. A root-level name
    /// leaf prints as a bare name rather than a trailing `self`.
    fn build_root(&self) -> Vec<UseTree> {
        let mut trees: Vec<UseTree> = Vec::new();
        for alias in &self.rename_aliases {
            trees.push(rename("self", alias));
        }
        if self.has_glob {
            trees.push(glob());
        }
        for (segment, child) in &self.children {
            trees.push(child.render(segment));
        }
        trees
    }

    /// Render the content of a node reached via `segment` into a single tree.
    fn render(&self, segment: &str) -> UseTree {
        let mut entries: Vec<UseTree> = Vec::new();
        if self.has_name {
            entries.push(name("self"));
        }
        for alias in &self.rename_aliases {
            entries.push(rename("self", alias));
        }
        if self.has_glob {
            entries.push(glob());
        }
        for (child_segment, child) in &self.children {
            entries.push(child.render(child_segment));
        }

        if entries.len() == 1 {
            match entries.into_iter().next().unwrap() {
                // A lone own-name entry `segment::self` collapses to the bare
                // name of this node.
                UseTree::Name(use_name) if use_name.ident == "self" => name(segment),
                UseTree::Rename(use_rename) if use_rename.ident == "self" => {
                    rename(segment, &use_rename.rename.to_string())
                }
                other => prefix(segment, other),
            }
        } else if self.has_name || self.has_glob || !self.children.is_empty() {
            prefix(segment, group(entries))
        } else {
            // Every entry renames this same node; express them as
            // `segment as Alias` siblings inside one braced group rather than
            // the wordier `segment::{self as Alias, ...}`.
            let aliases = entries
                .into_iter()
                .map(|entry| match entry {
                    UseTree::Rename(use_rename) => rename(segment, &use_rename.rename.to_string()),
                    other => other,
                })
                .collect();
            group(aliases)
        }
    }
}

enum LeafKind {
    Name,
    Rename(String),
    Glob,
}

fn ident(string: &str) -> Ident {
    Ident::new(string, proc_macro2::Span::call_site())
}

fn name(string: &str) -> UseTree {
    UseTree::Name(UseName {
        ident: ident(string),
    })
}

fn rename(string: &str, alias: &str) -> UseTree {
    UseTree::Rename(UseRename {
        ident: ident(string),
        as_token: Default::default(),
        rename: ident(alias),
    })
}

fn glob() -> UseTree {
    UseTree::Glob(UseGlob {
        star_token: Default::default(),
    })
}

fn prefix(segment: &str, tree: UseTree) -> UseTree {
    UseTree::Path(UsePath {
        ident: ident(segment),
        colon2_token: Default::default(),
        tree: Box::new(tree),
    })
}

fn group(entries: Vec<UseTree>) -> UseTree {
    UseTree::Group(UseGroup {
        brace_token: Brace::default(),
        items: entries
            .into_iter()
            .collect::<Punctuated<UseTree, syn::Token![,]>>(),
    })
}
