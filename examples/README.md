### [`dump-syntax`](dump-syntax)

Little utility to parse a Rust source file into a `syn::File` and print out a
debug representation of the syntax tree.

### [`heapsize`](heapsize)

An example implementation of a derive macro that generates trait impls.

### [`lazy-static`](lazy-static)

An example of parsing a custom syntax within a `functionlike!(...)` procedural
macro. Demonstrates how to trigger custom warnings and error messages on
individual tokens of the input.

### [`trace-var`](trace-var)

An attribute procedural macro that uses a syntax tree traversal to transform
certain syntax tree nodes in a function body.

### [`module-discovery`](module-discovery)

An offline module discoverer that walks `mod` declarations from a crate root
file, resolves `foo.rs` / `foo/mod.rs` / inline module files and literal
`#[path]` overrides, and reports a module tree with source ranges, reference
edges and diagnostics (ambiguity, missing files, inclusion cycles,
out-of-bounds files) while keeping `#[cfg]` conditions unevaluated.
