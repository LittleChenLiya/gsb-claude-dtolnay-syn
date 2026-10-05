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

### [`modvis`](modvis)

Module export view: given an inline module AST and an observer module path,
decide which items are visible to the observer. Resolves `pub`, `pub(crate)`,
`pub(super)` and `pub(in path)` along the real module ancestor chain, treats
external `mod name;` declarations as an unknown boundary without reading
files, and keeps declaration spans and visibility sources in the report.
