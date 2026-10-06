# api-fingerprint

Compile-free public interface fingerprinting for Rust source files, built on
[`syn`](https://github.com/dtolnay/syn). The input is a parsed `syn::File`
AST (or a source string parsed with `syn::parse_file`); no compiler, no name
resolution and no macro expansion are involved. The output is, for every
exported item, a **normalized signature** plus its **SHA-256 digest**, with
source locations attached so a diff can point back at the code.

## What is reported

Public functions (including `pub fn` in inherent impls), structs, enums,
traits and `pub use` re-exports that are reachable through fully `pub`
module chains from the crate root:

- items inside private or `pub(crate)` modules are **not** exports and never
  enter the report;
- `pub use` aliases are recorded with their normalized textual path, but the
  external crate is never consulted;
- anything the tool cannot resolve statically is recorded with
  `Status::Unknown` and a reason, so a diff can never report it as
  *definitely compatible*:
  - `ItemMacro` — a macro invocation in item position of a public module,
  - `UnloadedModule` — `pub mod foo;` whose file is not part of the input,
  - `ExternalUse` — `pub use` of a path not rooted at `self`/`super`/`crate`,
  - `GlobUse` — `pub use ...::*` whose exported set cannot be enumerated.

## Normalization rules

Ignored (digest-invariant): whitespace, comments, `#[doc]` attributes, and
function bodies (including default bodies of trait methods and default
values of trait associated consts).

Preserved (digest-sensitive): generic parameters and their bounds, `where`
clauses, visibility, field order, enum discriminants, ABI (`extern "C"`),
and all non-doc attributes such as `#[repr(...)]`.

Additional canonicalization:

- **Lifetime alpha-renaming** — lifetimes explicitly bound by the item's own
  generics are renamed to `'l0`, `'l1`, ... in declaration order, so a pure
  rename (`'a` → `'b`) is invisible while two independent lifetimes keep
  distinct names and are never merged. Unbound lifetimes (`'static`,
  `for<'x>` binders inside types) are left untouched.
- **Raw identifiers** — `r#type` normalizes to `type`.
- **Generic defaults** — `<T = u32>` is preserved as written.

Each entry's digest is `SHA-256(kind \0 path \0 normalized_signature)`,
implemented from scratch in `src/sha256.rs` (no external dependencies).

## Usage

Print the interface report of one file:

```console
$ cargo run -p api-fingerprint -- examples/api-fingerprint/fixtures/v1.rs
```

Show the interface difference between two versions of a file:

```console
$ cargo run -p api-fingerprint -- diff \
    examples/api-fingerprint/fixtures/v1.rs \
    examples/api-fingerprint/fixtures/v2.rs
```

Exit codes: `0` = no known changes and no unknown boundaries, `1` = known
interface changes detected, `2` = unknown boundaries present (compatibility
cannot be certified).

Library API: `fingerprint_source` / `fingerprint_file` produce a `Report`;
`diff(&old, &new)` produces a `Diff` with `Added`/`Removed`/`Changed`
changes plus the list of unknown entries from both sides.

## Fixtures and tests

`fixtures/v1.rs` and `fixtures/v2.rs` are two versions of the same small
interface, exercising nested modules, lifetimes, raw identifiers, generic
defaults, `repr`, discriminants, re-exports, macros and private items.

```console
$ cargo test -p api-fingerprint
```
