//! Command-line wrapper around the cfg_eval analysis module.
//!
//! Usage:
//!
//!     cargo run -p cfg-eval -- path/to/file.rs
//!
//! Optional overrides repeatable as: --set unix=true or
//! --set 'feature="fast"=unknown'. The built-in environment sets
//! feature="fast" to true, unix to unknown and windows to false.

use std::fs;
use std::process::ExitCode;

use quote::ToTokens;

use cfg_eval::{CfgEval, CfgState, ConfigEnv, DiagnosticKind};

fn default_env() -> ConfigEnv {
    let mut env = ConfigEnv::new();
    env.set(r#"feature="fast""#, CfgState::True);
    env.set("unix", CfgState::Unknown);
    env.set("windows", CfgState::False);
    env
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut env = default_env();
    let mut path = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--set" => {
                index += 1;
                match args.get(index).and_then(|arg| arg.rsplit_once('=')) {
                    Some((key, state)) => {
                        let state = match state {
                            "true" => CfgState::True,
                            "false" => CfgState::False,
                            "unknown" => CfgState::Unknown,
                            _ => {
                                eprintln!("error: state must be true, false or unknown");
                                return ExitCode::FAILURE;
                            }
                        };
                        env.set(key, state);
                    }
                    None => {
                        eprintln!("error: --set expects KEY=true|false|unknown");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if !other.starts_with('-') && path.is_none() => path = Some(other.to_owned()),
            other => {
                eprintln!("error: unexpected argument: {}", other);
                return ExitCode::FAILURE;
            }
        }
        index += 1;
    }

    let Some(path) = path else {
        eprintln!("usage: cfg-eval [--set KEY=STATE ...] FILE.rs");
        return ExitCode::FAILURE;
    };

    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read {}: {}", path, err);
            return ExitCode::FAILURE;
        }
    };
    let mut file = match syn::parse_file(&source) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("error: parse failure: {}", positioned(&err));
            return ExitCode::FAILURE;
        }
    };

    let evaluator = CfgEval::new(env);
    let diagnostics = match evaluator.eval_file(&mut file) {
        Ok(diagnostics) => diagnostics,
        Err(err) => {
            eprintln!("error: cfg evaluation failed: {}", positioned(&err));
            return ExitCode::FAILURE;
        }
    };

    println!("== diagnostics ==");
    if diagnostics.is_empty() {
        println!("(none)");
    }
    for diagnostic in &diagnostics {
        let kind = match diagnostic.kind {
            DiagnosticKind::UnknownCondition => "unknown-condition",
            DiagnosticKind::ExternalModule => "external-module",
        };
        match &diagnostic.condition {
            Some(condition) => println!(
                "{}:{}:{} [{}] {} ({})",
                path,
                diagnostic.line,
                diagnostic.column + 1,
                kind,
                diagnostic.path,
                condition,
            ),
            None => println!(
                "{}:{}:{} [{}] {}",
                path, diagnostic.line, diagnostic.column + 1, kind, diagnostic.path,
            ),
        }
    }

    println!();
    println!("== retained items ==");
    print_retained_items(&file);

    println!();
    println!("== rewritten source ==");
    println!("{}", file.into_token_stream());
    ExitCode::SUCCESS
}

fn positioned(err: &syn::Error) -> String {
    let start = err.span().start();
    format!("{}:{}: {}", start.line, start.column + 1, err)
}

fn print_retained_items(file: &syn::File) {
    for item in &file.items {
        print_item(item, 0);
    }
}

fn print_item(item: &syn::Item, depth: usize) {
    let indent = "  ".repeat(depth);
    match item {
        syn::Item::Mod(module) => {
            println!("{}- mod {}", indent, module.ident);
            if let Some((_, items)) = &module.content {
                for child in items {
                    print_item(child, depth + 1);
                }
            }
        }
        syn::Item::Enum(item) => println!("{}- enum {}", indent, item.ident),
        syn::Item::Struct(item) => println!("{}- struct {}", indent, item.ident),
        syn::Item::Trait(item) => println!("{}- trait {}", indent, item.ident),
        syn::Item::Impl(item) => println!("{}- impl {}", indent, item.self_ty.to_token_stream()),
        syn::Item::Fn(item) => println!("{}- fn {}", indent, item.sig.ident),
        syn::Item::Const(item) => println!("{}- const {}", indent, item.ident),
        syn::Item::Static(item) => println!("{}- static {}", indent, item.ident),
        syn::Item::Type(item) => println!("{}- type {}", indent, item.ident),
        syn::Item::Use(_) => println!("{}- use", indent),
        syn::Item::Union(_) => println!("{}- union", indent),
        syn::Item::ExternCrate(_) => println!("{}- extern crate", indent),
        syn::Item::ForeignMod(_) => println!("{}- extern block", indent),
        syn::Item::Macro(item) => {
            println!("{}- macro! {}", indent, item.mac.path.to_token_stream())
        }
        syn::Item::TraitAlias(item) => println!("{}- trait alias {}", indent, item.ident),
        syn::Item::Verbatim(_) => println!("{}- <verbatim>", indent),
        _ => println!("{}- <item>", indent),
    }
}
