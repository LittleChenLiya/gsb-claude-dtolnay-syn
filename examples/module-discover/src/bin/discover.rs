//! Command line front end for the offline module discoverer.
//!
//! Usage: `module-discover [path/to/lib.rs]` (defaults to `src/lib.rs`).

use std::path::Path;
use std::process::ExitCode;

use module_discover::{Diagnostic, EdgeKind, HopResolution, ModuleOrigin, SourceRef, discover};

fn main() -> ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "src/lib.rs".to_owned());

    let result = match discover(Path::new(&path)) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("error: unable to open {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!("crate root: {}", result.crate_root.display());
    println!();
    println!("module tree:");
    print_tree(&result, result.root, 0);

    println!();
    println!("source files:");
    for file in &result.files {
        println!(
            "  [{}] {} ({} bytes, {} declarations)",
            file.id.0,
            file.path.display(),
            file.byte_len,
            file.declarations.len(),
        );
        for declaration in &file.declarations {
            println!(
                "        {}..{}",
                declaration.range.start, declaration.range.end
            );
        }
    }

    println!();
    println!("reference edges:");
    for edge in &result.edges {
        let kind = match edge.kind {
            EdgeKind::Inline => "inline",
            EdgeKind::File => "file",
            EdgeKind::Broken => "broken",
        };
        println!(
            "  {} -> {} ({kind}, site {})",
            edge.parent.0,
            edge.child.0,
            display_ref(&result, edge.site),
        );
    }

    let diagnostics = result.diagnostics();
    if !diagnostics.is_empty() {
        println!();
        println!("diagnostics:");
        for diagnostic in &diagnostics {
            print_diagnostic(diagnostic);
        }
    }

    ExitCode::SUCCESS
}

fn print_tree(
    result: &module_discover::DiscoveredCrate,
    id: module_discover::ModuleId,
    depth: usize,
) {
    let module = result.module(id);
    let indent = "  ".repeat(depth);
    let suffix = origin_suffix(&module.origin);
    let cfg = if module.cfg.is_empty() {
        String::new()
    } else {
        format!(" cfg={}", module.cfg.len())
    };
    println!("{indent}{} [{}]{suffix}{cfg}", module.name, module.id.0);
    for child in &module.children {
        print_tree(result, *child, depth + 1);
    }
}

fn origin_suffix(origin: &ModuleOrigin) -> String {
    match origin {
        ModuleOrigin::Root => " (root)".to_owned(),
        ModuleOrigin::Inline { .. } => " (inline)".to_owned(),
        ModuleOrigin::File { path, mod_rs, .. } => {
            let style = if *mod_rs { "mod.rs" } else { "file" };
            format!(" ({style}: {})", path.display())
        }
        ModuleOrigin::Ambiguous { candidates, .. } => format!(
            " (ambiguous: {})",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        ModuleOrigin::Missing { searched, .. } => format!(
            " (missing; tried {})",
            searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ModuleOrigin::OutsideRoot { path, resolved, .. } => {
            format!(
                " (symlink escapes root: {} -> {})",
                path.display(),
                resolved.display()
            )
        }
        ModuleOrigin::Cycle { path, back_to, .. } => {
            format!(" (cycle: {} revisits hop {back_to})", path.display())
        }
        ModuleOrigin::Unreadable { path, error, .. } => {
            format!(" (unreadable {}: {error})", path.display())
        }
        ModuleOrigin::NotUtf8 { path, .. } => {
            format!(" (not utf-8: {})", path.display())
        }
        ModuleOrigin::ParseError { path, error, .. } => {
            format!(" (parse error {}: {error})", path.display())
        }
        ModuleOrigin::UnsupportedPath { .. } => " (unsupported #[path])".to_owned(),
    }
}

fn display_ref(result: &module_discover::DiscoveredCrate, reference: SourceRef) -> String {
    let file = result.file(reference.file);
    format!(
        "{}:{}..{}",
        file.path.display(),
        reference.range.start,
        reference.range.end
    )
}

fn print_diagnostic(diagnostic: &Diagnostic) {
    let (kind, chain) = match diagnostic {
        Diagnostic::Ambiguous {
            qualname,
            chain,
            candidates,
            ..
        } => {
            println!("ambiguous module {qualname}");
            for candidate in candidates {
                println!("  candidate: {}", candidate.display());
            }
            ("ambiguous", chain)
        }
        Diagnostic::Missing {
            qualname,
            chain,
            searched,
            ..
        } => {
            println!("missing module {qualname}");
            for path in searched {
                println!("  searched: {}", path.display());
            }
            ("missing", chain)
        }
        Diagnostic::OutsideRoot {
            qualname,
            chain,
            path,
            resolved,
            ..
        } => {
            println!(
                "out-of-root symlink in {qualname}: {} -> {}",
                path.display(),
                resolved.display()
            );
            ("outside-root", chain)
        }
        Diagnostic::Cycle {
            qualname,
            chain,
            back_to,
            path,
            ..
        } => {
            println!(
                "recursive inclusion in {qualname}: {} revisits chain hop {back_to}",
                path.display()
            );
            ("cycle", chain)
        }
        Diagnostic::Unreadable {
            qualname,
            chain,
            path,
            error,
            ..
        } => {
            println!("unreadable {qualname} at {}: {error}", path.display());
            ("unreadable", chain)
        }
        Diagnostic::NotUtf8 {
            qualname,
            chain,
            path,
            ..
        } => {
            println!("not utf-8 {qualname}: {}", path.display());
            ("not-utf8", chain)
        }
        Diagnostic::ParseError {
            qualname,
            chain,
            path,
            error,
            ..
        } => {
            println!("parse error in {qualname} at {}: {error}", path.display());
            ("parse-error", chain)
        }
        Diagnostic::UnsupportedPath {
            qualname, chain, ..
        } => {
            println!("unsupported #[path] attribute on {qualname}");
            ("unsupported-path", chain)
        }
    };
    let _ = kind;
    println!("  chain:");
    for (index, hop) in chain.iter().enumerate() {
        let resolution = hop
            .resolution
            .as_ref()
            .map(resolution_text)
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "    [{index}] {} declared at file {} bytes {}..{} -> {resolution}",
            hop.name, hop.declared.file.0, hop.declared.range.start, hop.declared.range.end,
        );
    }
}

fn resolution_text(resolution: &HopResolution) -> String {
    match resolution {
        HopResolution::Inline => "inline".to_owned(),
        HopResolution::File(path) => path.display().to_string(),
        HopResolution::Ambiguous(candidates) => format!(
            "ambiguous({})",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        HopResolution::Missing => "missing".to_owned(),
        HopResolution::OutsideRoot { path, resolved } => {
            format!("outside-root({} -> {})", path.display(), resolved.display())
        }
        HopResolution::Cycle(path) => format!("cycle({})", path.display()),
        HopResolution::Unreadable(path) => format!("unreadable({})", path.display()),
        HopResolution::NotUtf8(path) => format!("not-utf8({})", path.display()),
        HopResolution::ParseError(path) => format!("parse-error({})", path.display()),
        HopResolution::UnsupportedPath => "unsupported-path".to_owned(),
    }
}
