# Generic parameter usage analysis

This example builds a reusable analysis on top of `syn`'s syntax tree
visitor: given any `syn::Item`, it reports every type, lifetime and const
generic parameter the item declares, where each of them is declared, and
every position that references it — looking through field types, function
signatures, `where` clauses, generic parameter defaults, associated types and
inline const expressions.

Scoping follows Rust: parameters of nested items, methods, GATs and
higher-ranked (`for<'a>`) binders shadow same-named outer parameters, nested
items cannot capture outer generics, and macro invocations are treated as
opaque regions that prevent any parameter from being proven unused.

- [`generic-usage`](generic-usage/src/lib.rs) — the analysis library, with a
  test suite covering higher-ranked lifetimes, nested-item shadowing,
  associated types, default generics, inline const expressions and macros.
- [`example`](example/src/main.rs) — a small binary that runs the analysis
  over a sample source file and prints a report.

Try it out:

```console
$ cargo run --manifest-path examples/generic-usage/example/Cargo.toml
$ cargo test --manifest-path examples/generic-usage/generic-usage/Cargo.toml
```
