# trait-impl-contract

Static contract checking between a `trait` definition and its `impl` block,
built purely on `syn` syntax trees. It never invokes `rustc` and never mutates
the input AST.

The checker reports three disjoint categories:

- **missing** — associated items (methods, types, consts) that the trait
  requires but the impl does not provide. Items with a default (default method
  body, defaulted associated type or const) are *not* required.
- **mismatches** — signature differences that are statically decidable from
  syntax alone: receiver shape (`&self` vs `&mut self` vs `self` vs
  `self: Box<Self>`), `const` / `async` / `safe` / `unsafe` qualifiers, ABI
  strings, generic parameter arity, argument count, and concrete type
  differences in which no type alias could be involved.
- **unknowns** — pairs whose compatibility cannot be decided without name
  resolution or trait solving: differences that may be explained by a type
  alias, syntactically different `where` clauses or bounds, array length
  expressions, associated-type bounds satisfaction, and so on. Unknown is
  reported separately from proven incompatibility.

Generic parameter renames are handled by positional mapping, so a trait
method `f<T>(T)` implemented as `f<U>(U)` is recognized as isomorphic.

See the crate-level documentation in `src/lib.rs` for the exact statically
decidable scope and worked examples.
