//! Demo: print the module export view of a three-module crate (`a` with a
//! nested `b`, plus `c`) from the point of view of several observers.
//!
//! Run with: cargo run -p modvis

use modvis::{analyze_file, ExportView};

/// Three modules: `a` (with nested `b`), `c`, and an external `ext` boundary.
const THREE_MODULES: &str = r#"
mod a {
    pub mod b {
        pub(super) fn only_a() {}
        pub(crate) fn whole_crate() {}
        pub(in crate::a) fn within_a() {}
        pub fn everyone() {}
        fn hidden() {}
        pub(in crate::c) fn misplaced() {}
    }
    mod ext;
    pub use ext::Thing;
}

mod c {
    pub fn elsewhere() {}
}
"#;

fn print_view(observer: &str, view: &ExportView) {
    println!("=== observer: {observer} ===");
    println!(
        "{:<10} {:<14} {:<6} {:<8} {:<18} {}",
        "module", "item", "kind", "span", "visibility", "verdict"
    );
    for item in &view.items {
        let module = if item.module.is_empty() {
            "crate".to_string()
        } else {
            item.module.join("::")
        };
        println!(
            "{:<10} {:<14} {:<6} {:<8} {:<18} {}",
            module,
            item.name,
            item.kind,
            format!("{}:{}", item.span.0, item.span.1),
            item.visibility,
            item.verdict,
        );
    }
    for diag in &view.diagnostics {
        println!("diagnostic {}:{}: {}", diag.span.0, diag.span.1, diag.message);
    }
    println!();
}

fn main() {
    let file = syn::parse_file(THREE_MODULES).expect("the three-module fixture parses");
    for observer in ["a", "a::b", "c"] {
        let path: Vec<&str> = observer.split("::").collect();
        print_view(observer, &analyze_file(&file, &path));
    }
}
