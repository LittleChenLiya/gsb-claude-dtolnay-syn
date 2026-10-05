# attr-plan

AST attribute edit plan: select items by explicit path, add/remove outer
attributes, preserve inner attributes and unrelated token order.

## Cargo commands

Run from the repository root:

```sh
# Build the attr-plan crate
cargo build -p attr-plan

# Run all tests (including quote re-parse round-trip tests)
cargo test -p attr-plan

# Run a single test
cargo test -p attr-plan --test plan -- add_inline_only_to_a_f

# Run doc tests
cargo test -p attr-plan --doc
```

## Plan usage

```rust
use attr_plan::{AttrSpec, Plan};
use syn::parse_quote;

let src = r#"
    mod a {
        fn f() {}
    }
    mod b {
        fn f() {}
    }
"#;
let file = syn::parse_file(src).unwrap();

let mut plan = Plan::new();
// Add #[inline] to a::f only; b::f is untouched.
plan.add(parse_quote!(a::f), AttrSpec::parse("inline").unwrap());

let result = plan.apply(&file).unwrap();
// result.file is the new AST; result.mappings records old/new span
// correspondence for every edit.
```

## Guarantees

- **Explicit path selection** — items are selected by a full path like
  `a::f`, not by a bare name.
- **Path ambiguity is rejected** — if a path segment matches more than one
  item in the same scope, the plan fails before any edit.
- **Attribute parameter parsing** — every attribute spec is parsed up front;
  a malformed spec fails the plan before any edit.
- **`cfg_attr` text is never treated as a direct attribute** — the tokens
  inside `#[cfg_attr(...)]` are not parsed as attributes, so removing
  `allow` never touches an `allow` nested inside `cfg_attr`.
- **Atomic failure** — validation runs entirely on the original file; the
  original `File` is never mutated. A new `File` is returned only when every
  step succeeds.
- **No item reordering, no function body rewrite** — only the outer
  attributes of targeted items are changed; inner attributes and token
  order are preserved.
