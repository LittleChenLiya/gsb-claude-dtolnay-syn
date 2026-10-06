# api-fingerprint

Public-interface fingerprinting for Rust source files, built on
[`syn`](https://github.com/dtolnay/syn). **No compilation of the analyzed
crate is required** — everything is derived from the parsed `syn::File`
AST of a single crate-root source file.

Given a source file, the tool emits:

* the **normalized signatures** of all exported items — public functions
  (including inherent methods), structs, enums, traits and `pub use`
  re-exports;
* a list of **unknown** export boundaries (macro-generated items,
  unresolvable `pub use`, out-of-line modules, unparseable items), each
  with a source location;
* a **SHA-256 digest** over the canonical report.

## Usage

```console
# print the interface report of one file
cargo run -p api-fingerprint -- fingerprint/fixtures/v1.rs

# show the interface diff between two versions (runs in well under 90 s)
cargo run -p api-fingerprint -- diff fingerprint/fixtures/v1.rs fingerprint/fixtures/v2.rs
```

Exit codes for `diff`: `0` when the known surface is identical *and* no
unknown boundaries exist; `1` when the interface changed or unknown
boundaries prevent a definite compatibility claim; `2` on usage errors.

Library API:

```rust
let fp = api_fingerprint::fingerprint_source(&source)?;
println!("digest: {}", fp.digest);
print!("{}", fp.report());
let d = api_fingerprint::diff(&old, &new);
assert!(d.is_definitely_compatible());
```

## What the digest covers

The digest changes exactly when the *known* public interface changes:

| Change | Digest |
| --- | --- |
| function body, whitespace, comments, doc comments | **unchanged** |
| private fn / private module contents | **unchanged** (never reported) |
| renaming a bound lifetime (`'a` → `'x`) | **unchanged** |
| raw vs non-raw spelling of the same ident (`r#type` / `type`) | **unchanged** |
| parameter/return types, generics, `where` clauses | **changed** |
| `#[repr(...)]`, `#[non_exhaustive]`, `#[cfg(...)]`, `#[derive(...)]` | **changed** |
| field order, enum discriminants, ABI (`extern "C"`) | **changed** |
| generic parameter defaults (`<T = u32>`) | **changed** |
| `pub use` alias or its textual target path | **changed** |

## Normalization rules

* **Lifetimes**: only lifetimes *explicitly bound* on the item's own
  generic parameter list (or a `for<>` binder) are alpha-renamed to
  `'lt0`, `'lt1`, … in declaration order. Two independent lifetimes are
  never merged into one. Elided lifetimes and `'_` / `'static` are left
  as-is.
* **Raw identifiers**: `r#type` normalizes to `type`.
* **Modules**: only `pub mod` contents are traversed; entry paths carry
  the module prefix (`nested::deep::Leaf`). Items in private modules are
  not exports.
* **Visibility**: only fully `pub` items are exported; `pub(crate)`,
  `pub(self)`, `pub(super)`, `pub(in …)` are not.

## Unknown boundaries

Some export boundaries cannot be decided from one file's AST. They are
recorded as `unknown` entries with a source location (`line:column`) for
diff localization, and the fingerprint is then *incomplete* — it must
not be reported as definitely compatible:

* **macro invocations** in exported scope (`my_macro! { … }`,
  `#[derive(...)]`) — the macro may generate public items;
* **`pub use` of external paths** — recorded with the textual path and
  `unknown` status, never resolved into an external crate;
* **glob re-exports** (`pub use foo::*`) — the re-exported set is unknown;
* **out-of-line modules** (`pub mod ext;`) — the module body lives in a
  file that was not provided;
* **unparseable items** (`syn::Item::Verbatim`).

Unknown entries are listed separately in the report and their textual
detail feeds the digest, so a change in an external `pub use` path or a
macro invocation is still detected.

## Fixtures

`fixtures/` contains the AST fixtures used by the test suite and the
demo above:

* `v1.rs` — a crate root exercising every feature (generics, lifetimes,
  raw identifiers, nested modules, `pub use`, macros, out-of-line mod);
* `v1_1.rs` — same public interface as `v1.rs` (only bodies, whitespace,
  comments, lifetime names and private items changed): identical digest;
* `v2.rs` — real interface changes (parameter type, `where` clause,
  `repr`, discriminant, generic default, added/removed items);
* `v1.expected.txt` — golden report for `v1.rs`.

## Tests

```console
cargo test -p api-fingerprint
```

## Limitations

* Input is a single file; `mod foo;` declarations are reported as
  `unknown` (out-of-line-module) rather than followed.
* `cfg` attributes are kept verbatim, not evaluated.
* `pub const` / `pub static` / `pub type` are currently not fingerprinted.
