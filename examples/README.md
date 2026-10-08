### [`dump-syntax`](dump-syntax)

Little utility to parse a Rust source file into a `syn::File` and print out a
debug representation of the syntax tree.

### [`generic-usage`](generic-usage)

Reusable analyzer that, given an item, reports where each declared type,
lifetime and const generic parameter is used: field types, function
signatures, `where` bounds, defaults, array lengths and inline const blocks.
Demonstrates scope stacks with nested-item shadowing, higher-ranked lifetime
binders, and conservative handling of opaque macro/attribute scopes so that
unused parameters never include parameters a macro expansion might reference.

### [`heapsize`](heapsize)

An example implementation of a derive macro that generates trait impls.

### [`lazy-static`](lazy-static)

An example of parsing a custom syntax within a `functionlike!(...)` procedural
macro. Demonstrates how to trigger custom warnings and error messages on
individual tokens of the input.

### [`trace-var`](trace-var)

An attribute procedural macro that uses a syntax tree traversal to transform
certain syntax tree nodes in a function body.
