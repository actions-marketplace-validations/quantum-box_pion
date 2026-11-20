# pion

Select the tests a change can affect.

pion reads the Cargo dependency graph of a workspace and the files that
changed since a revision, and prints which crates' tests to run and why.

```console
$ cargo pion plan --base origin/main
2 changed file(s), 3 crate(s) to test

api
  depends on: auth

auth
  changed: packages/auth/src/usecase/sign_in.rs

auth_domain
  changed: packages/auth/domain/src/token.rs
```

## Status

Early. The first level of resolution (crates) works. Narrowing inside a crate
and selecting API scenarios are designed in [docs/design.md](docs/design.md)
and not built yet.

## Install

```bash
cargo install --git https://github.com/quantum-box/pion cargo-pion
```

## Use

```bash
# Changes in the working tree since a revision, untracked files included
cargo pion plan --base origin/main

# Changes between two revisions
cargo pion plan --base v1.2.0 --head v1.3.0

# Run only the affected crates with nextest
cargo nextest run -E "$(cargo pion plan --base origin/main --format nextest)"

# The whole plan, with the reason for every selected crate
cargo pion plan --base origin/main --format json
```

`--deny-uncovered` exits with status 2 when a changed file is covered by no
rule, for callers that gate a deployment on the plan.

## How a change spreads

| Changed file | Tests selected |
| --- | --- |
| A file inside a crate (`.rs`, `Cargo.toml`, `build.rs`, SQL, fixtures, ...) | That crate, and every workspace crate that depends on it |
| `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `.cargo/` | Every crate |
| A file no workspace crate owns | None. The file is listed as not covered |

A crate depends on another when its tests are built from it: through normal
and build dependencies transitively, and through its own dev-dependencies. A
dev-dependency does not pass on to the crate's dependents.

## Dependencies

pion runs in CI next to deploy credentials, so its dependencies are kept few
and well known: `guppy` for the dependency graph, `clap`, `serde`,
`serde_json`, `camino` and `thiserror`. State the reason when adding one.

## License

MIT
