//! Demonstration for the reusable generic-usage analyzer in `lib.rs`.
//!
//! Parses a Rust source file (or a built-in snippet if no path is given) and
//! prints one report per generic owner, with declaration and reference
//! coordinates.
//!
//! Run from this directory:
//!
//!     cargo run
//!     cargo run -- tests/acceptance.rs

use generic_usage::{analyze_file, analyze_item, ItemReport, ParamKind};
use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1) {
        Some(path) => match fs::read_to_string(path) {
            Ok(source) => {
                let file = syn::parse_file(&source).unwrap_or_else(|err| {
                    eprintln!("failed to parse {path}: {err}");
                    process::exit(1);
                });
                for report in analyze_file(&file) {
                    print_report(&report, 0);
                }
            }
            Err(err) => {
                eprintln!("failed to read {path}: {err}");
                process::exit(1);
            }
        },
        None => {
            // Built-in sample: T, U, 'a and const N referenced in a field, a
            // where clause and an array length. The source is parsed with
            // `parse_str` so span line/column coordinates are meaningful.
            let source = "\
struct Container<'a, T, U, const N: usize>
where
    T: AsRef<U>,
{
    first: &'a T,
    table: [U; N],
}
";
            let file = syn::parse_file(source).expect("built-in snippet parses");
            let item = file.items.into_iter().next().expect("one item");
            let report = analyze_item(&item);
            print_report(&report, 0);
        }
    }
}

fn print_report(report: &ItemReport, depth: usize) {
    let indent = "  ".repeat(depth);
    println!("{indent}{}", report.owner);
    for param in &report.params {
        let kind = match param.kind {
            ParamKind::Type => "type",
            ParamKind::Lifetime => "lifetime",
            ParamKind::Const => "const",
        };
        let apostrophe = if param.kind == ParamKind::Lifetime {
            "'"
        } else {
            ""
        };
        println!(
            "{indent}  {kind} {apostrophe}{} declared at {}:{}",
            param.name, param.declared_at.at.0, param.declared_at.at.1
        );
        if param.references.is_empty() {
            if param.unknown_scope {
                println!("{indent}    (no observed use; an opaque scope might use it)");
            } else {
                println!("{indent}    UNUSED");
            }
        }
        for site in &param.references {
            println!(
                "{indent}    used at {}:{}  [{}]",
                site.at.0, site.at.1, site.label
            );
        }
    }
    for nested in &report.nested {
        print_report(nested, depth + 1);
    }
}
