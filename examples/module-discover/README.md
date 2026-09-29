# module-discover

Offline Rust module discoverer built with syn (https://github.com/dtolnay/syn).

Given a crate root file (for example src/lib.rs), it walks every mod
declaration without invoking the compiler and reports:

- the module tree, including inline modules;
- the source files that back each file module and the byte ranges of the
  declarations they contain;
- the directed reference edges from a module to its children;
- complete diagnostic chains for foo.rs/foo/mod.rs ambiguity, missing files,
  out-of-root symlinks and recursive inclusion.

Module file location follows the standard Rust rules:

- a file module "mod foo;" resolves to foo.rs or foo/mod.rs;
- children of an inline "mod foo { ... }" are searched under foo/;
- a literal "#[path = \"...\"]" attribute relocates the search; a trailing
  path separator (or an existing directory) selects directory style.

cfg conditions are never evaluated or guessed. Conditional modules are
retained unconditionally with their parsed cfg expressions preserved, so a
caller can decide enablement itself.

Library entry point: module_discover::discover.

CLI:

    cargo run -p module-discover --bin discover -- src/lib.rs
