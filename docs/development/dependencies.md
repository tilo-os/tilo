# Change a dependency

For native build prerequisites, use [setup](setup.md). This page covers changes
to Cargo dependencies and their generated metadata.

## Add, remove or change features

1. Explain why the dependency is needed: licence, upstream provenance,
   maintenance, size and supported targets. Use workspace dependencies where
   appropriate and avoid unused or wildcard dependencies.
2. Edit the relevant manifests. Keep the committed `Cargo.lock`; do not delete
   it or run an unrestricted update to obtain a fresh resolve.
3. Repair generated feature unification and, if the manifests require it, relock
   through the existing checks:

   ```sh
   t fix hakari
   t fix lockfile
   ```

4. Review the returned diff, including changed manifests and lockfile entries.
   Test the affected packages/targets using [the testing workflow](testing.md).
   Select `hakari`, `lockfile` and `deny` when checking dependency changes. The
   optional whole-tree unused-dependency check is `machete`; it is not a
   mandatory check for every contribution.

[Dependency policy](../policies.md#dependencies-and-runtime) and
[deny.toml](../../deny.toml) define source, advisory and licence restrictions. A
justified exception belongs with the rule it changes, not in an ad hoc Cargo
invocation. Vendored changes also retain upstream licences and provenance; see
[legal and interoperability](../policies.md#legal-and-interoperability).

## Update one locked crate deliberately

Choose the crate and exact version, then use Cargo's targeted update:

```sh
cargo update -p "$crate" --precise "$version"
```

Set `crate` and `version` to the intended package and reviewed version first;
they are not repository defaults. This resolves dependencies but compiles no
Rust. Review transitive changes: unrelated version moves are not acceptable.

For a lock-only commit, stage the intended lock change and declare the command
at commit time:

```sh
TILO_CARGO_UPDATE="-p $crate --precise $version" git commit
```

Several deliberate updates can be declared with semicolon-separated clauses. The
declaration does not exempt arbitrary lock churn.
[check_lock_update.py](../../tools/ci/check_lock_update.py) requires
working-tree lock/manifests to match the index and `cargo metadata --locked` to
accept them. Every moved package must be declared at the requested version;
added/removed entries must belong to those updates' dependency graphs. See
[Git tools](tools/git.md) for hook installation and staged-change failures.

A no-version-move relock after a rebase needs no update declaration.
`t fix lockfile` supplies missing manifest-required entries without asking Cargo
to update locked versions. Never hand-edit checksums or versions.

## Keep workspace-hack generated

[crates/workspace-hack](../../crates/workspace-hack/Cargo.toml) unifies
third-party feature sets for managed native packages, reducing duplicate
compilation across package selections.
[.config/hakari.toml](../../.config/hakari.toml) owns its configuration and
exclusions. Do not edit between the `HAKARI SECTION` markers by hand.

`t fix hakari` runs generation and dependency management remotely; no local tool
installation is needed. The `hakari` check verifies the generated manifest,
managed dependency edges and unified features. For a package also built for
`wasm32-unknown-unknown`, its `workspace-hack` dependency belongs under
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, not unconditional
dependencies. The policy check reports incorrect placement.

## Repair other generated files

Resolve source/manifests first, then use the owning generator rather than
merging generated text by hand. The
[Git tools page](tools/git.md#resolve-generated-file-conflicts) covers rebase
handling. Schema bindings use
[tools/regenerate-capnp.sh](../../tools/regenerate-capnp.sh) and its declared
crate layouts; regeneration can install/build a plugin and run a container. It
is not a cheap diagnostic or a prerequisite for ordinary source builds.
