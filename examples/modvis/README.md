# modvis — module export view

Given an **inline module AST** (a `syn::File` or an inline `syn::ItemMod`)
and an **observer module path**, decide for every declared item whether the
observer can see it.

- `pub`, `pub(crate)`, `pub(super)` and `pub(in path)` are resolved along
  the **real module ancestor chain** of the inline AST. In particular
  `pub(super)` inside `a::b` means "visible within `a`" — it is *not*
  treated as crate-visible, so it stays hidden from `c`.
- `pub(in path)` must name an ancestor module of the declaring module;
  anything else produces a diagnostic pointing at the offending path.
- External module declarations (`mod ext;`) are an **unknown boundary**:
  they are reported as `Unknown` and their files are never read. A
  `pub use ext::Thing;` re-export whose target crosses that boundary is
  likewise reported `Unknown`.
- Every report row keeps the declaration span (line, column) and the
  visibility source as written (`pub`, `pub(crate)`, `pub(super)`,
  `pub(in crate::a)`, `private`).
- Macros are never expanded.

## Run

Three-module demo (`a` with nested `b`, plus `c` and an external `ext`):

```console
cargo run -p modvis
```

Acceptance tests:

```console
cargo test -p modvis
```
