# consolidate-use

An example use-declaration organizer built on `syn`'s AST. It expands every
`use` tree into its individual imports, then merges consecutive `use`
statements that share visibility, attributes and leading `::` by their common
path prefix, producing a new `syn::File` suitable for feeding into a code generator.

Run the tests:

```sh
cargo test -p consolidate-use
```
