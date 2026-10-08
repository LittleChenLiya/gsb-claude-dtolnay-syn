# generic-usage

A reusable example of analyzing generic parameter usage with [`syn`].

Given a parsed [`syn::Item`], the analyzer in [`src/lib.rs`] lists the type,
lifetime and const generic parameters that the item declares and records every
place where each parameter is referenced:

- struct/enum/union **field types** (including tuple-field indices),
- **function signatures** (receiver, arguments, return type),
- **`where` clauses** and parameter bounds,
- **default values** of type and const generic parameters,
- **array lengths** and **inline `const { ... }` blocks**,
- associated types, associated consts and their defaults.

Every record stores the parameter kind, its declaration site and each reference
site as a `line:column` coordinate with a label describing the position, so
reports can point straight back at the relevant source tokens.

## Scope and shadowing

References are resolved against a stack of scopes:

- each generic owner (struct, enum, union, trait, impl, function, method,
  associated item, extern item, ...) pushes its generics;
- nested items push their own generics, so a nested parameter with the same
  name as an outer one correctly **shadows** it;
- higher-ranked binders (`for<'a>` on trait objects, function pointers and
  `where` predicates) introduce lifetime-only scopes;
- qualified self paths (`<T as Trait>::Assoc`) resolve only the qself type,
  never the associated-item segments.

## Unknown scopes (macros and attributes)

`syn` cannot see inside token streams, so they are treated as opaque:

- a macro invocation (`println!(...)`, a function-like macro, an item-position
  invocation) may expand to code that names any in-scope parameter;
- a non-builtin attribute (including `#[derive(...)]` and attribute macros)
  receives the whole item and may emit such code as well;
- `macro_rules!` definitions are textual and cannot capture surrounding
  generics, so they are *not* considered opaque.

A parameter is reported as unused only when every relevant scope was
inspected. Parameters with no observed use but an opaque scope are reported
as **possibly used** via [`ParamReport::possibly_used`] and are excluded from
[`ItemReport::unused_params`].

## Running

```console
# built-in demonstration
cargo run -p generic-usage

# analyze a source file and print the reports
cargo run -p generic-usage -- path/to/file.rs

# acceptance tests: structs with T/U/'a/const N, HRTB, nested shadowing,
# associated types, default generics, inline consts and macros
cargo test -p generic-usage
```

## API sketch

```rust
use generic_usage::analyze_file;

let file = syn::parse_file(&source)?;
for report in analyze_file(&file) {
    for param in &report.params {
        println!("{} {} declared {}", param.kind, param.name, param.declared_at);
        for site in &param.references {
            println!("  used at {site}");
        }
        if param.is_unused() {
            println!("  (unused)");
        } else if param.possibly_used() {
            println!("  (a macro/attribute might use it)");
        }
    }
}
```

[`syn`]: https://github.com/dtolnay/syn
[`src/lib.rs`]: src/lib.rs
