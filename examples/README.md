### [`dump-syntax`](dump-syntax)

Little utility to parse a Rust source file into a `syn::File` and print out a
debug representation of the syntax tree.

### [`cfg-eval`](cfg-eval)

Reusable three-valued evaluation of `cfg` / `cfg_attr` conditionals on a
parsed `syn::File`, reporting which items a configuration would keep without
compiling the project.

### [`heapsize`](heapsize)

An example implementation of a derive macro that generates trait impls.

### [`lazy-static`](lazy-static)

An example of parsing a custom syntax within a `functionlike!(...)` procedural
macro. Demonstrates how to trigger custom warnings and error messages on
individual tokens of the input.

### [`trace-var`](trace-var)

An attribute procedural macro that uses a syntax tree traversal to transform
certain syntax tree nodes in a function body.
