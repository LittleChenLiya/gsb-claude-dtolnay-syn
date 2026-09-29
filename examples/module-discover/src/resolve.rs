//! Candidate path resolution following the Rust module file rules.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Outcome of locating the file backing a `mod foo;` declaration.
pub enum CandidateResolution {
    /// Exactly one existing file was chosen.
    Exists {
        path: PathBuf,
        canonical: PathBuf,
        /// `foo/mod.rs` style rather than `foo.rs` style.
        mod_rs: bool,
    },
    /// `foo.rs` and `foo/mod.rs` both exist.
    Ambiguous { candidates: Vec<PathBuf> },
    /// No candidate existed.
    Missing { searched: Vec<PathBuf> },
    /// The path leaves the crate root through a symlink.
    OutsideRoot { path: PathBuf, resolved: PathBuf },
}

/// Resolve a file-module declaration.
///
/// `crate_root` is the canonical directory of the crate root file.
/// `file_dir` is the directory of the file that contains the declaration;
/// `#[path]` values are interpreted relative to it. `default_dir` is the
/// directory used by the name-based `foo.rs`/`foo/mod.rs` search, which
/// additionally carries the names of enclosing inline modules.
pub fn resolve_candidate(
    crate_root: &Path,
    file_dir: &Path,
    default_dir: &Path,
    name: &str,
    explicit_path: Option<&str>,
) -> CandidateResolution {
    let candidates = if let Some(explicit) = explicit_path {
        // A #[path] is relative to the declaring file's directory. A trailing
        // separator (or an existing directory target) selects directory style;
        // otherwise the value names the file directly.
        let joined = file_dir.join(explicit);
        let trailing_separator = explicit
            .chars()
            .next_back()
            .is_some_and(std::path::is_separator);
        let directory_style = trailing_separator || joined.is_dir();
        if directory_style {
            vec![joined.join(format!("{name}.rs")), joined.join("mod.rs")]
        } else {
            vec![joined]
        }
    } else {
        vec![
            default_dir.join(format!("{name}.rs")),
            default_dir.join(name).join("mod.rs"),
        ]
    };

    // Ambiguity only applies to the name-based search: both `foo.rs` and
    // `foo/mod.rs` present at once.
    if explicit_path.is_none()
        && candidates.len() == 2
        && candidates[0].is_file()
        && candidates[1].is_file()
    {
        return CandidateResolution::Ambiguous { candidates };
    }

    let mut searched = Vec::new();
    for candidate in candidates {
        match candidate.metadata() {
            Ok(metadata) if metadata.is_file() => {
                if let Some((path, resolved)) = escapes_through_symlink(crate_root, &candidate) {
                    return CandidateResolution::OutsideRoot { path, resolved };
                }
                match fs::canonicalize(&candidate) {
                    Ok(canonical) => {
                        return CandidateResolution::Exists {
                            mod_rs: candidate
                                .file_name()
                                .is_some_and(|file_name| file_name == "mod.rs"),
                            path: candidate,
                            canonical,
                        };
                    }
                    Err(_) => searched.push(candidate),
                }
            }
            _ => searched.push(candidate),
        }
    }

    CandidateResolution::Missing { searched }
}

/// Directory used to resolve children of an inline module.
///
/// An inline `mod foo { ... }` keeps its file children under a `foo/`
/// directory next to the file that declares it.
pub fn inline_children_dir(current_file: &Path, name: &str) -> PathBuf {
    let parent_dir = current_file.parent().unwrap_or_else(|| Path::new(""));
    parent_dir.join(name)
}

/// Walk `path` from `crate_root` one component at a time, canonicalizing
/// each symlink. If a symlink points outside `crate_root`, returns the
/// symlink path and its resolved target.
fn escapes_through_symlink(crate_root: &Path, path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current = crate_root.to_path_buf();
    let mut components = path.components().peekable();

    // Skip the leading components shared with the crate root.
    for root_component in crate_root.components() {
        match components.peek() {
            Some(component) if component.as_os_str() == root_component.as_os_str() => {
                components.next();
            }
            _ => break,
        }
    }

    for component in components {
        match component {
            Component::Normal(part) => {
                current.push(part);
                if current.is_symlink() {
                    if let Ok(target) = fs::canonicalize(&current) {
                        if !target.starts_with(crate_root) {
                            return Some((current.clone(), target));
                        }
                    } else {
                        // Dangling or looping symlink; full canonicalization
                        // surfaces this as a Missing resolution.
                        return None;
                    }
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::RootDir | Component::Prefix(_) => {
                current = PathBuf::from(component.as_os_str());
            }
        }
    }
    None
}
