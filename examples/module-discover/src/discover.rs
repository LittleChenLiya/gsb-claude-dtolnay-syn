//! Offline discovery of a crate's module tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use syn::{File as SynFile, Item, ItemMod};

use crate::cfg::parse_cfg_attrs;
use crate::items::{PathAttr, item_range, path_attr, resolution_label};
use crate::model::*;
use crate::resolve::{CandidateResolution, inline_children_dir, resolve_candidate};
use crate::spanmap::LineMap;

/// Module tree, source files and reference edges for one crate root.
#[derive(Clone, Debug)]
pub struct DiscoveredCrate {
    pub root: ModuleId,
    pub modules: Vec<Module>,
    pub files: Vec<SourceFile>,
    pub edges: Vec<Edge>,
    /// Canonical path of the crate root's directory.
    pub crate_root: PathBuf,
}

impl DiscoveredCrate {
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.0]
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0]
    }

    /// Every resolution problem, each carrying its full declaration chain.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for module in &self.modules {
            let diagnostic = match &module.origin {
                ModuleOrigin::Ambiguous { candidates, .. } => Some(Diagnostic::Ambiguous {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    candidates: candidates.clone(),
                }),
                ModuleOrigin::Missing { searched, .. } => Some(Diagnostic::Missing {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    searched: searched.clone(),
                }),
                ModuleOrigin::OutsideRoot { path, resolved, .. } => Some(Diagnostic::OutsideRoot {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    path: path.clone(),
                    resolved: resolved.clone(),
                }),
                ModuleOrigin::Cycle { path, back_to, .. } => Some(Diagnostic::Cycle {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    back_to: *back_to,
                    path: path.clone(),
                }),
                ModuleOrigin::Unreadable { path, error, .. } => Some(Diagnostic::Unreadable {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    path: path.clone(),
                    error: error.clone(),
                }),
                ModuleOrigin::NotUtf8 { path, .. } => Some(Diagnostic::NotUtf8 {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    path: path.clone(),
                }),
                ModuleOrigin::ParseError { path, error, .. } => Some(Diagnostic::ParseError {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                    path: path.clone(),
                    error: error.clone(),
                }),
                ModuleOrigin::UnsupportedPath { .. } => Some(Diagnostic::UnsupportedPath {
                    module: module.id,
                    qualname: module.qualname.clone(),
                    chain: module.chain.clone(),
                }),
                ModuleOrigin::Root | ModuleOrigin::Inline { .. } | ModuleOrigin::File { .. } => {
                    None
                }
            };
            if let Some(diagnostic) = diagnostic {
                out.push(diagnostic);
            }
        }
        out
    }
}

/// A resolution problem with the complete declaration chain leading to it.
#[derive(Clone, Debug)]
pub enum Diagnostic {
    Ambiguous {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        candidates: Vec<PathBuf>,
    },
    Missing {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        searched: Vec<PathBuf>,
    },
    OutsideRoot {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        path: PathBuf,
        resolved: PathBuf,
    },
    Cycle {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        /// Index of the ancestor hop that the last hop revisits.
        back_to: usize,
        path: PathBuf,
    },
    Unreadable {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        path: PathBuf,
        error: String,
    },
    NotUtf8 {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        path: PathBuf,
    },
    ParseError {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
        path: PathBuf,
        error: String,
    },
    UnsupportedPath {
        module: ModuleId,
        qualname: String,
        chain: Vec<ChainHop>,
    },
}

pub(crate) struct LoadedFile {
    pub(crate) id: FileId,
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
    pub(crate) ast: SynFile,
    pub(crate) line_map: LineMap,
    pub(crate) parse_error: Option<LoadProblem>,
}

#[derive(Clone)]
pub(crate) enum LoadProblem {
    NotUtf8,
    Parse(String),
}

struct Discoverer {
    modules: Vec<Module>,
    files: Vec<SourceFile>,
    edges: Vec<Edge>,
    texts: HashMap<FileId, String>,
    file_ids: HashMap<PathBuf, FileId>,
    /// Canonical file paths on the current inclusion stack.
    stack: Vec<PathBuf>,
    crate_root: PathBuf,
}

fn empty_file() -> SynFile {
    SynFile {
        shebang: None,
        frontmatter: None,
        attrs: Vec::new(),
        items: Vec::new(),
    }
}

/// Discover all modules reachable from a crate root file (e.g. `src/lib.rs`).
pub fn discover(root: impl AsRef<Path>) -> io::Result<DiscoveredCrate> {
    let canonical_root = fs::canonicalize(root.as_ref())?;
    let crate_root = canonical_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let mut state = Discoverer {
        modules: Vec::new(),
        files: Vec::new(),
        edges: Vec::new(),
        texts: HashMap::new(),
        file_ids: HashMap::new(),
        stack: Vec::new(),
        crate_root,
    };

    let loaded = state.load_file(&canonical_root)?;
    let root_id = state.push_module(
        "crate".to_owned(),
        "crate".to_owned(),
        None,
        Vec::new(),
        ModuleOrigin::Root,
        vec![ChainHop {
            name: "crate".to_owned(),
            declared: SourceRef {
                file: loaded.id,
                range: SpanRange {
                    start: 0,
                    end: loaded.contents.len(),
                },
            },
            resolution: Some(HopResolution::File(canonical_root.clone())),
        }],
    );

    state.stack.push(canonical_root);
    let root_dir = state.crate_root.clone();
    state.scan_items(root_id, loaded.id, &root_dir, &loaded.ast.items, &loaded);
    state.stack.pop();

    Ok(DiscoveredCrate {
        root: root_id,
        modules: state.modules,
        files: state.files,
        edges: state.edges,
        crate_root: state.crate_root,
    })
}

impl Discoverer {
    /// Read, register and parse a canonical file path.
    fn load_file(&mut self, canonical: &Path) -> io::Result<LoadedFile> {
        if let Some(&id) = self.file_ids.get(canonical) {
            let contents = self.texts[&id].clone();
            let ast = syn::parse_file(&contents).unwrap_or_else(|_| empty_file());
            return Ok(LoadedFile {
                id,
                path: canonical.to_path_buf(),
                line_map: LineMap::new(&contents),
                contents,
                ast,
                parse_error: None,
            });
        }

        let raw = fs::read(canonical)?;
        let id = FileId(self.files.len());
        match String::from_utf8(raw) {
            Ok(contents) => {
                let (ast, parse_error) = match syn::parse_file(&contents) {
                    Ok(ast) => (ast, None),
                    Err(err) => (empty_file(), Some(LoadProblem::Parse(err.to_string()))),
                };
                self.files.push(SourceFile {
                    id,
                    path: canonical.to_path_buf(),
                    byte_len: contents.len(),
                    declarations: Vec::new(),
                });
                self.file_ids.insert(canonical.to_path_buf(), id);
                self.texts.insert(id, contents.clone());
                let line_map = LineMap::new(&contents);
                Ok(LoadedFile {
                    id,
                    path: canonical.to_path_buf(),
                    contents,
                    ast,
                    line_map,
                    parse_error,
                })
            }
            Err(_) => {
                self.files.push(SourceFile {
                    id,
                    path: canonical.to_path_buf(),
                    byte_len: 0,
                    declarations: Vec::new(),
                });
                self.file_ids.insert(canonical.to_path_buf(), id);
                self.texts.insert(id, String::new());
                Ok(LoadedFile {
                    id,
                    path: canonical.to_path_buf(),
                    contents: String::new(),
                    ast: empty_file(),
                    line_map: LineMap::new(""),
                    parse_error: Some(LoadProblem::NotUtf8),
                })
            }
        }
    }

    fn push_module(
        &mut self,
        name: String,
        qualname: String,
        parent: Option<ModuleId>,
        cfg: Vec<CfgExpr>,
        origin: ModuleOrigin,
        chain: Vec<ChainHop>,
    ) -> ModuleId {
        let id = ModuleId(self.modules.len());
        self.modules.push(Module {
            id,
            name,
            qualname,
            parent,
            children: Vec::new(),
            origin,
            cfg,
            chain,
        });
        if let Some(parent) = parent {
            self.modules[parent.0].children.push(id);
        }
        id
    }

    fn qualname(&self, parent: ModuleId, name: &str) -> String {
        format!("{}::{}", self.modules[parent.0].qualname, name)
    }

    fn child_chain(
        &self,
        parent: ModuleId,
        name: &str,
        site: SourceRef,
        resolution: Option<HopResolution>,
    ) -> Vec<ChainHop> {
        let mut chain = self.modules[parent.0].chain.clone();
        chain.push(ChainHop {
            name: name.to_owned(),
            declared: site,
            resolution,
        });
        chain
    }

    fn scan_items(
        &mut self,
        parent: ModuleId,
        file_id: FileId,
        search_dir: &Path,
        items: &[Item],
        loaded: &LoadedFile,
    ) {
        for item in items {
            if let Item::Mod(item_mod) = item {
                self.scan_mod(parent, file_id, search_dir, item_mod, loaded);
            }
        }
    }

    fn scan_mod(
        &mut self,
        parent: ModuleId,
        file_id: FileId,
        search_dir: &Path,
        item_mod: &ItemMod,
        loaded: &LoadedFile,
    ) {
        let name = item_mod.ident.to_string();
        let mut cfg = self.modules[parent.0].cfg.clone();
        cfg.extend(parse_cfg_attrs(&item_mod.attrs));
        let range = item_range(loaded, item_mod);
        let site = SourceRef {
            file: file_id,
            range,
        };
        self.files[file_id.0].declarations.push(site);

        let path_attr = path_attr(&item_mod.attrs);

        if let Some((_, items)) = &item_mod.content {
            let chain = self.child_chain(parent, &name, site, Some(HopResolution::Inline));
            let qualname = self.qualname(parent, &name);
            let file_dir = self.files[file_id.0]
                .path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf();
            let default_child_dir = inline_children_dir(&self.files[file_id.0].path, &name);
            let child_dir = match &path_attr {
                PathAttr::Literal(value) => file_dir.join(value),
                PathAttr::None | PathAttr::Unsupported => default_child_dir,
            };
            let id = self.push_module(
                name,
                qualname,
                Some(parent),
                cfg,
                ModuleOrigin::Inline {
                    declared_in: file_id,
                },
                chain,
            );
            self.edges.push(Edge {
                parent,
                child: id,
                kind: EdgeKind::Inline,
                site,
            });
            self.scan_items(id, file_id, &child_dir, items, loaded);
            return;
        }

        // File module.
        let origin = match path_attr {
            PathAttr::Unsupported => ModuleOrigin::UnsupportedPath {
                declared_in: file_id,
            },
            PathAttr::None | PathAttr::Literal(_) => {
                let explicit = match &path_attr {
                    PathAttr::Literal(value) => Some(value.clone()),
                    _ => None,
                };
                let resolution = resolve_candidate(
                    &self.crate_root,
                    self.files[file_id.0]
                        .path
                        .parent()
                        .unwrap_or_else(|| Path::new("")),
                    search_dir,
                    &name,
                    explicit.as_deref(),
                );
                self.finalize_file_module(file_id, resolution)
            }
        };

        let resolution_label = resolution_label(&origin);
        let chain = self.child_chain(parent, &name, site, resolution_label);
        let qualname = self.qualname(parent, &name);
        let id = self.push_module(name, qualname, Some(parent), cfg, origin, chain);
        let kind = match self.modules[id.0].origin {
            ModuleOrigin::File { .. } => EdgeKind::File,
            _ => EdgeKind::Broken,
        };
        self.edges.push(Edge {
            parent,
            child: id,
            kind,
            site,
        });

        // Descend only into a uniquely resolved file.
        if let ModuleOrigin::File { file, mod_rs, .. } = self.modules[id.0].origin.clone() {
            let canonical = self.files[file.0].path.clone();
            self.stack.push(canonical.clone());
            let child_loaded = LoadedFile {
                id: file,
                path: canonical,
                contents: self.texts[&file].clone(),
                ast: syn::parse_file(&self.texts[&file]).unwrap_or_else(|_| empty_file()),
                line_map: LineMap::new(&self.texts[&file]),
                parse_error: None,
            };
            let child_dir = if mod_rs {
                child_loaded
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
            } else {
                inline_children_dir(&child_loaded.path, &self.modules[id.0].name)
            };
            self.scan_items(id, file, &child_dir, &child_loaded.ast.items, &child_loaded);
            self.stack.pop();
        }
    }

    fn finalize_file_module(
        &mut self,
        declared_in: FileId,
        resolution: CandidateResolution,
    ) -> ModuleOrigin {
        match resolution {
            CandidateResolution::Ambiguous { candidates } => ModuleOrigin::Ambiguous {
                declared_in,
                candidates,
            },
            CandidateResolution::Missing { searched } => ModuleOrigin::Missing {
                declared_in,
                searched,
            },
            CandidateResolution::OutsideRoot { path, resolved } => ModuleOrigin::OutsideRoot {
                declared_in,
                path,
                resolved,
            },
            CandidateResolution::Exists {
                path,
                canonical,
                mod_rs,
            } => {
                // Cycle detection against the inclusion stack.
                if let Some(position) = self.stack.iter().position(|p| p == &canonical) {
                    return ModuleOrigin::Cycle {
                        declared_in,
                        path,
                        back_to: position,
                    };
                }

                let loaded = match self.load_file(&canonical) {
                    Ok(loaded) => loaded,
                    Err(err) => {
                        return ModuleOrigin::Unreadable {
                            declared_in,
                            path,
                            error: err.to_string(),
                        };
                    }
                };

                if let Some(error) = loaded.parse_error {
                    match error {
                        LoadProblem::NotUtf8 => ModuleOrigin::NotUtf8 { declared_in, path },
                        LoadProblem::Parse(error) => ModuleOrigin::ParseError {
                            declared_in,
                            file: loaded.id,
                            path,
                            error,
                        },
                    }
                } else {
                    ModuleOrigin::File {
                        declared_in,
                        file: loaded.id,
                        path,
                        mod_rs,
                    }
                }
            }
        }
    }
}
