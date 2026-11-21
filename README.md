# pion

Select the tests a change can affect.

pion reads what changed since a revision and prints which tests to run and
why, at three levels of resolution:

1. **Crates**, from the Cargo dependency graph.
2. **Tests inside a crate**, from a reference graph of the code: only the
   tests that reach a changed function, type or constant.
3. **API scenarios**, through the handlers their requests reach.

```console
$ cargo pion plan --base origin/main --scip target/pion/index.scip \
    --scenarios apps/api/tests/scenarios
1 changed file(s), 2 crate(s) and 1 scenario(s) to test

auth (1 test(s))
  test: tests::rejects_expired_token
  reaches: core::token/impl#[Token]is_expired().

core (1 test(s))
  test: token::tests::token_roundtrip
  reaches: core::token/impl#[Token]is_expired().

apps/api/tests/scenarios/sign_in.yaml
  api::sign_in_handler(). reaches core::token/impl#[Token]is_expired().
```

## Status

Early. All three levels work and are tested against a real rust-analyzer
index, but the selection rules have not yet been checked against a long
history of full test runs. Keep running every test somewhere (see
[Checking the selection](#checking-the-selection)) and do not gate a release
on pion alone.

Measured on a workspace of 172 crates and about 16,000 tests, by asking what
a change to each of 300 random lines of code would select:

| | rust-analyzer index | compiler records |
| --- | --- | --- |
| Changes that could be narrowed | 295 of 300 | 288 of 300 |
| Tests selected for a change in non-test code: median | 5 | 9 |
| ... 75th percentile | 65 | 70 |
| ... 90th percentile | 328 | 487 |
| ... worst case | 4,779 | 3,292 |
| Changes in non-test code that reach no test | 86 of 222 | 61 of 220 |
| API scenarios selected, median of 178 | 46 | 35 |
| Crates the crate-level plan would run in full, median | 6 | 6 |

The compiler records select somewhat more tests and leave fewer changes with
no test at all: they follow calls the index cannot see. Neither has been
checked against real failures yet.

## Install

### Release binaries

Each release has prebuilt binaries for Linux (x86_64 and aarch64) and macOS
(Apple silicon), with a `SHA256SUMS` file. The install script verifies every
download against it:

```bash
curl -fsSL https://raw.githubusercontent.com/quantum-box/pion/v0.1.0/scripts/install.sh \
  | PION_VERSION=v0.1.0 sh
```

| Variable | Meaning |
| --- | --- |
| `PION_VERSION` | Release to install, such as `v0.1.0`. Default: `latest` |
| `PION_INSTALL_DIR` | Where the binaries go. Default: `$CARGO_HOME/bin` |
| `PION_RUSTC_NIGHTLY` | Also install `pion-rustc` built for this nightly, such as `nightly-2026-06-04` |

`cargo-pion` is a static binary. `pion-rustc` loads the compiler's own
library, so it exists per nightly toolchain: the ones listed in
[driver/toolchains.txt](driver/toolchains.txt). For another nightly, add it
there and release, or build the wrapper from source (see
[References from the compiler](#references-from-the-compiler-instead-of-an-index)).

### In GitHub Actions

```yaml
- uses: quantum-box/pion@<commit of the release>  # v0.1.0
  id: pion
  with:
    version: v0.1.0
    rustc-nightly: nightly-2026-06-04   # omit to install cargo-pion only

# Record references during the check the job runs anyway
- run: cargo check --workspace --all-targets
  env:
    RUSTC_WORKSPACE_WRAPPER: ${{ steps.pion.outputs.pion-rustc }}
    PION_REFS_DIR: ${{ runner.temp }}/pion-refs

- run: cargo pion plan --base "$BASE_SHA" --refs "$RUNNER_TEMP/pion-refs" --format github-matrix
```

Pin the action to a commit and `version` to a tag, so that a new release
cannot change what a workflow runs without a change in the workflow.

### From source

```bash
cargo install --git https://github.com/quantum-box/pion cargo-pion
```

Building an index with `cargo pion index` needs `rust-analyzer` on the `PATH`
(`rustup component add rust-analyzer`), and `cargo pion run` needs
[cargo-nextest](https://nexte.st).

## Use

```bash
# Crate level: changes in the working tree since a revision
cargo pion plan --base origin/main

# Changes between two revisions
cargo pion plan --base v1.2.0 --head v1.3.0

# Inside crates: build the index of the working tree, then plan with it
cargo pion index
cargo pion plan --base origin/main --scip target/pion/index.scip

# API scenarios as well
cargo pion plan --base origin/main --scip target/pion/index.scip \
  --scenarios apps/api/tests/scenarios

# Run the selected tests with nextest
cargo pion run --base origin/main --scip target/pion/index.scip -- --no-fail-fast
```

### Which revision to index

Building the index is the slow part: rust-analyzer analyses the whole
workspace, and there is no incremental mode. On a workspace of 172 crates it
took 26 minutes and 19 GB of memory, and the index was 344 MB. Planning with
that index takes a few seconds.

So build the index once per commit of the main branch, in CI, and plan every
change against the index of its base:

```bash
# In CI, for each commit on main: build and store the index
cargo pion index --output index.scip

# For a change: fetch the index of the merge base, then
cargo pion plan --base "$MERGE_BASE" --scip index.scip --scip-revision base
```

With `--scip-revision base` a changed line is attributed to the definition
that held it before the change. Code that is new in the change is reached
through the existing lines that had to change to call it; a new item added
between existing items falls back to the whole crate.

The default, `--scip-revision head`, expects an index of the working tree.

### References from the compiler, instead of an index

The index can be skipped altogether. `driver/` holds `pion-rustc`, a rustc
wrapper that records which definition refers to which while the workspace
compiles:

```bash
# Once: build the wrapper with the nightly the workspace builds with
rustup component add rustc-dev llvm-tools --toolchain "$NIGHTLY"
cargo +"$NIGHTLY" build --release --manifest-path driver/Cargo.toml

# Whenever the workspace is checked anyway
RUSTC_WORKSPACE_WRAPPER=/path/to/pion-rustc PION_REFS_DIR=target/pion/refs \
  cargo check --workspace --all-targets

cargo pion plan --base origin/main --refs target/pion/refs
```

cargo recompiles only what changed, so the records are brought up to date by
the compile that happens anyway. On the 172-crate workspace a full check took
130 s without the wrapper and 120 s with it; the recording itself added up to
15 s across all crates. Compare 26 minutes and 19 GB for the index.

The compiler also knows what an index cannot show:

- Code a macro generated. A derived impl is a definition like any other, so a
  call that goes through it is followed, and a change to the `#[derive(...)]`
  line belongs to it.
- The impl a trait call runs, whenever the type is known. Only calls through a
  generic parameter or `dyn Trait` fall back to the trait method.

The costs: the wrapper uses the compiler's internal API, so it is tied to one
nightly toolchain and needs fixing when that is bumped; and the records are
large (851 MB of JSON for that workspace). If recording a crate fails, the
wrapper runs the untouched rustc instead, so the build is unaffected, and
leaves a marker; pion then runs all tests of that crate whenever a change
touches it or something it depends on.

### What would run if this line changed

```bash
cargo pion impact --scip target/pion/index.scip crates/auth/src/token.rs:42
```

prints one JSON object per `path:line` with the number of crates, tests and
scenarios a change there selects, next to what the crate-level plan would
run. Use it to see how far a change to a given function spreads.

### Output formats

`--format` chooses what `plan` prints:

| Format | Output |
| --- | --- |
| `text` (default) | Crates, tests and scenarios, with the reason for each |
| `json` | The whole plan |
| `nextest` | A filterset for `cargo nextest run -E` |
| `packages` | The selected crates, one per line |
| `github-matrix` | `{"include":[{"package":…,"filterset":…}]}` for a GitHub Actions matrix, one job per crate |
| `scenarios` | The selected scenario files, one per line |

`--deny-uncovered` exits with status 2 when a changed file is covered by no
rule, for callers that gate a deployment on the plan.

## How a change spreads

### Between crates

| Changed file | Tests selected |
| --- | --- |
| A file inside a crate (`.rs`, `Cargo.toml`, `build.rs`, SQL, fixtures, ...) | That crate, and every workspace crate that depends on it |
| `Cargo.lock` | The crates that depend on a package the lockfile now resolves differently |
| The root `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `.cargo/` | Every crate |
| A file no workspace crate owns | None. The file is listed as not covered |

A crate depends on another when its tests are built from it: through normal
and build dependencies transitively, and through its own dev-dependencies. A
dev-dependency does not pass on to the crate's dependents.

### Inside a crate (`--scip`)

Each changed line of a `.rs` file is attributed to the definition that
contains it. From there pion follows references backwards: whatever mentions
a changed definition is affected, and so on, until it reaches test functions.

| Situation | Rule |
| --- | --- |
| A call through `dyn Trait` or a generic | A change to an impl affects everything that refers to the trait method |
| A struct field or enum variant changes | Everything that refers to the type is affected |
| A changed line is outside every definition (an import, an attribute, an `impl` header, a macro call) | The crate runs all its tests, and so do its dependents |
| The file is not in the index (new, deleted, not compiled) | Same |
| An item is defined inside a function body (a mock in a test) | It belongs to that function: it cannot be named from outside |
| A blank line or comment is added between items | Ignored |

### API scenarios (`--scenarios`)

A scenario file is a list of steps with a `request:` that has a `method:` and
a `url:` (the format [muon](https://github.com/quantum-box/muon) runs).

- A REST request is matched against the routes registered in the source as
  `.route("/path", get(handler))`. The scenario is selected when a handler it
  requests reaches a changed definition.
- A GraphQL request is matched by name: a top-level field `createFeature` is
  taken to be served by every method called `create_feature`.
- A request that matches no route makes the scenario *untraced*: it is
  selected whenever its crate is affected at all.
- When the crate under test runs all its tests, all its scenarios run too.

## Checking the selection

Selection from a static graph can miss what the graph does not show: code
generated by macros, reflection, global state. Measure it:

```bash
cargo pion plan --base "$BASE" --scip target/pion/index.scip --format json > plan.json
cargo nextest run --profile ci        # every test, with a JUnit report
cargo pion audit --plan plan.json --junit target/nextest/ci/junit.xml
```

`audit` lists the tests that failed in the full run but were not selected,
and exits with status 1 when there are any.

When the full run uses `cargo test` instead of nextest, pass its output with
`--libtest`. A CI job log works as it is; with GitHub Actions the failed jobs
are enough, since only failures can be misses:

```bash
gh run view "$RUN_ID" --log-failed > failed.log
cargo pion audit --plan plan.json --libtest failed.log
```

## Dependencies

pion runs in CI next to deploy credentials, so its dependencies are kept few
and well known: `guppy` for the dependency graph, `clap`, `serde`,
`serde_json`, `camino` and `thiserror`. The SCIP index, `Cargo.lock`, JUnit
reports and scenario files are read by small readers in this repository
rather than by additional crates. State the reason when adding a dependency.

## License

MIT
