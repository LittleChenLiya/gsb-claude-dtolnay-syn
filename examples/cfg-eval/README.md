# cfg-eval

Example-level, reusable analysis of cfg / cfg_attr conditionals on a
parsed syn::File, for tools that do not compile the Rust project but still
need to know which syntax items a configuration would keep.

## Usage

    cargo run -p cfg-eval -- path/to/file.rs

The command parses the file, evaluates it against the built-in environment
(feature="fast" is true, unix is unknown, windows is false; extra
--set KEY=true|false|unknown flags override entries), and prints the
diagnostics, the retained items, and the rewritten source.

Library entry point: cfg_eval::CfgEval::new(env).eval_file(&mut file)
returns the diagnostics and rewrites the file in place.

## Semantics

- false condition: the item is removed.
- true condition: the cfg / cfg_attr attribute is stripped.
- unknown condition: the original attribute is kept untouched and a
  diagnostic records the simplified condition plus the module path of the
  annotated node. Unknown conditions are never treated as false.
- cfg_attr with a true condition expands in place; generated cfg,
  cfg_attr, or other attributes are processed recursively. Expansion is
  bounded by a budget (CfgEval::DEFAULT_BUDGET, overridable with
  with_budget) so adversarial nesting cannot run away.
- Malformed cfg expressions abort the whole analysis with a positioned
  syn::Error; the caller's File is left unmodified (no partial rewrite).

## Supported scope

- Predicates: bare names (unix), name/value pairs (feature = "fast"),
  and the combinators all(...), any(...), not(...). Empty all() is
  true, empty any() is false. not requires exactly one argument.
- Recursion: crate-level items, inline mod blocks, impl blocks, trait
  items, extern blocks, enum variants, and named or tuple struct/enum
  fields. Removing tuple fields keeps the punctuation legal; tokens and
  spans of unaffected syntax are preserved.
- External "mod foo;" declarations are reported as ExternalModule
  diagnostics; the module never reads other files and never expands macros.
- Not evaluated: cfg inside function bodies, macro invocations, and
  derive-generated code.
