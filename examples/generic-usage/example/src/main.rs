//! Demo of the `generic-usage` analysis: parses the embedded source file and
//! prints, for every item, where each of its generic parameters is declared
//! and used (line:column, columns are 0-based).

use generic_usage::{analyze_item, GenericUsageReport, ParamKind};
use syn::{File, Item};

const SOURCE: &str = "\
struct Pair<'a, T, U, const N: usize>
where
    T: Clone,
{
    first: &'a T,
    second: U,
    buf: [T; N],
}

impl<'a, T, U, const N: usize> Pair<'a, T, U, N> {
    fn first(&self) -> &'a T {
        self.first
    }
}

struct Callbacks<'a> {
    borrowed: &'a str,
    callback: for<'a> fn(&'a str),
}

struct Spare<T, U = Vec<T>>(u8);

fn convert<T>(input: u8) -> u8 {
    magic!(T);
    input
}
";

fn item_label(item: &Item) -> String {
    let (kind, name) = match item {
        Item::Struct(item) => ("struct", item.ident.to_string()),
        Item::Enum(item) => ("enum", item.ident.to_string()),
        Item::Fn(item) => ("fn", item.sig.ident.to_string()),
        Item::Trait(item) => ("trait", item.ident.to_string()),
        Item::Impl(_) => ("impl", String::new()),
        Item::Union(item) => ("union", item.ident.to_string()),
        Item::Type(item) => ("type", item.ident.to_string()),
        _ => ("item", String::new()),
    };
    if name.is_empty() {
        kind.to_owned()
    } else {
        format!("{} {}", kind, name)
    }
}

fn print_report(label: &str, report: &GenericUsageReport) {
    println!("{}:", label);
    if report.params.is_empty() {
        println!("    (declares no generic parameters)");
    }
    for param in &report.params {
        let name = match param.kind {
            ParamKind::Lifetime => format!("'{}", param.name),
            _ => param.name.clone(),
        };
        println!(
            "    {:8} {:2} declared at {}",
            param.kind.to_string(),
            name,
            param.declared_at
        );
        if param.used_at.is_empty() {
            println!("        never used");
        } else {
            for used_at in &param.used_at {
                println!("        used at {}", used_at);
            }
        }
    }
    if report.has_opaque_regions {
        println!("    contains macro invocations or uninterpreted tokens;");
        println!("    no parameter can be proven unused");
    } else {
        let unused: Vec<String> = report
            .unused_params()
            .iter()
            .map(|param| param.name.clone())
            .collect();
        if unused.is_empty() {
            println!("    unused parameters: (none)");
        } else {
            println!("    unused parameters: {}", unused.join(", "));
        }
    }
    println!();
}

fn main() {
    let file: File = syn::parse_str(SOURCE).unwrap();
    for item in &file.items {
        print_report(&item_label(item), &analyze_item(item));
    }
}
