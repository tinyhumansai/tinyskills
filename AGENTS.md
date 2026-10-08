# Repository Guidelines

## Purpose and boundaries

`tinyskills` owns portable handling of agentskills.io-style bundles: metadata,
document parsing, filesystem discovery, collision handling, safe resource
reads, and compile-time bundle materialization.

Embedding products own directory layouts, workspace trust, installation
policy, execution, approvals, RPC/controller schemas, event buses, and UI.
Keep product names, product environment variables, and product-specific paths
out of this crate; accept roots and policy inputs at the API boundary instead.

Network I/O is host-supplied through `RegistryTransport`; the crate links no
HTTP client. The `registry` feature owns the request guard (scheme, DNS
pinning, redirect re-validation, size and time limits), caching and search;
the host owns the HTTP stack, TLS, where the catalog store lives, and what to
do with a fetched document.

## Structure

```text
Cargo.toml
crates/tinyskills/
├── Cargo.toml
├── src/
│   ├── lib.rs          # public exports and crate overview
│   ├── model.rs        # metadata, scopes, constants
│   ├── document.rs     # Markdown/frontmatter parsing and inventory
│   ├── flat.rs         # line-based SKILL.md parser and renderer
│   ├── digest.rs       # sha256 of one rendered document
│   ├── discovery.rs    # deterministic scanning and collisions
│   ├── catalog/        # network-free registry catalog parsing, URLs, lookup, search
│   ├── install.rs      # installation URL/host validation, fetched-document validation and atomic write
│   ├── authoring.rs    # slugs, document rendering, budgets, bundle scaffolding
│   ├── remove.rs       # defensive bundle removal
│   ├── trigger.rs      # `triggers:` pattern parsing and matching
│   ├── scan/           # supply-chain scan of untrusted skill text, catalogue sanitizer
│   ├── slug.rs         # host slug rules: length cap, reserved names, punctuation, fallback
│   ├── archive.rs      # `archive` feature: zip/tar upload reader
│   ├── registry/       # `registry` feature: transport contract, fetch guard, sources, index, store, SkillRegistry
│   ├── resource.rs     # safe lookup and resource reads
│   ├── materialize.rs  # rebuild a skill tree from documents and bundle dirs
│   └── bundle.rs       # compile-time bundle materialization
└── tests/
    ├── catalog.rs
    ├── authoring.rs
    ├── collisions.rs
    ├── documents.rs
    ├── edge_cases.rs
    ├── fetched_documents.rs
    ├── flat.rs
    ├── install.rs
    ├── materialize.rs
    ├── public_api.rs
    ├── registry.rs
    ├── registry_loopback.rs
    ├── support/        # registry test doubles and a loopback HTTP server
    ├── fixtures/       # captured upstream data
    ├── remove.rs
    ├── resource_symlinks.rs
    └── trigger.rs
```

Public exports are centralized in `lib.rs`. Prefer focused modules named for
their responsibility; do not introduce generic `utils` or `helpers` modules.

## Build and test

Run all commands from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
cargo test --features registry
```

Tests must be deterministic and avoid network access, wall-clock assumptions,
and shared process state. Integration tests exercise the public API. Add a
regression test for each bug and cover failure paths for security-sensitive
filesystem behavior.

## Rust style

- Use Rust 2024 idioms and standard `rustfmt` output.
- Keep the public surface minimal and document every public item.
- Return typed results from fallible public APIs and document `# Errors`.
- Do not use `unwrap`, `expect`, `panic`, `todo`, or `unimplemented` in library
  paths.
- Do not weaken workspace lints or add broad `allow` attributes.
- Reject symlinks before traversal and canonicalize before containment checks.
- Keep reads size-bounded and preserve strict UTF-8 behavior.

## Dependencies

Prefer the standard library and existing dependencies. Declare shared
dependencies once in the root workspace manifest, enable only required
features, and document any substantial new dependency. Keep `Cargo.lock`
committed.

`cap-std` and `cap-fs-ext` (Bytecode Alliance) back `materialize_tree`, which
takes open directory handles and resolves no path: everything at or below the
handles is opened and created without following a symlink, on Unix and
Windows alike, with no `unsafe` here. How the caller opened those handles,
including symlinked ancestors of the paths they came from, is the caller's.

## Documentation

Update `README.md`, rustdoc, and tests with behavior or public API changes.
Write for an embedding application that has never seen the original host.
Examples must compile and must not assume an OpenHuman directory layout.

## Git workflow

- Never work directly on `main`; use a feature worktree and branch.
- Do not rewrite published history or bypass hooks.
- Keep commits focused and use concise imperative subjects.
- Open pull requests ready for review unless they are genuinely incomplete.
- Report the exact validation commands run and any known failures.

## Agent working agreement

Read surrounding code before editing, stay within the requested scope, and
verify claims with fresh command output. Do not leave placeholders, weaken
guardrails, inspect secrets, or silently skip failing tests. Ask only when an
irreversible decision or genuine product-policy fork blocks progress.

## Tests live in `*_tests.rs` files

- Unit tests are never inline. Do not write a `#[cfg(test)] mod tests { ... }`
  block in a source file. Put the tests in a sibling `<module>_tests.rs`
  (`mod_tests.rs` beside a `mod.rs`, `lib_tests.rs` beside `lib.rs`) and declare
  it at the bottom of the module:

  ```rust
  #[cfg(test)]
  #[path = "foo_tests.rs"]
  mod tests;
  ```

- The test file starts with `use super::*;` and carries no `#[cfg(test)]` of its
  own. It is still a child module, so it reaches private items exactly as an
  inline module did.
- Name test files `<module>_tests.rs`; a second group for the same module is
  `<module>_<topic>_tests.rs`. Never `test.rs`, `tests.rs` or `<module>_test.rs`.
- Integration tests stay in the crate's `tests/` directory.
- OpenHuman's `scripts/externalize-inline-tests.mjs <repo-root> --write` moves
  inline test modules out mechanically; without `--write` it only reports.
