# t: testing and integrating changes

Tilo's checks (tests, lints, builds) run on [t](https://code.tilo.so/tilo/t), which caches each
result by the files it depends on. The checks are declared in [`t.toml`](../t.toml). This page is
how to use t and how changes get from your checkout to a release.

## Setting up

```
cargo install --locked --git https://code.tilo.so/tilo/t
```

Put the server and your token in `~/.config/t/config.toml` (mode `0600`):

```toml
server = "https://t.tilo.so"
token = "…"
```

Ask a maintainer for a token. `T_SERVER` and `T_TOKEN` override the file. Nothing else is needed
locally: checks run on the server, in the builder image.

## How changes reach a release

- **`main`:** push straight to it; no pull request is needed. Rebase on `main` and push; a push
  that isn't a fast-forward is refused. Nothing is gated and nothing is reverted.
- **CI on `main`:** t runs every default check on each new commit of `main`, finds the commit a
  failure began with and tells its author. `main` is sometimes red; that is expected.
- **`stable`:** releases are cut from it. The stabilizer (usually an agent) picks a recent `main`
  commit, runs the checks a release needs, and for each failure commits a fix (brought back to
  `main`) or an exclusion of unfinished work (with an issue saying what and why). It never weakens
  a check to make it pass. When everything passes it moves `stable` there. Only the stabilizer
  pushes to `stable`.

Many people and agents change Tilo at once. Gating every change on a full CI round trip
serialized them, and automatic reverts fought the fixes that followed; this way landing is fast,
and releasability is the stabilizer's job instead of something every push has to prove.

## Testing a change

```
t test <check|package|family|glob>      # exactly that, on your working tree
t test test/tilo-fs --test parser::empty   # one test
t test                                  # what failed for you last time
```

What you test before pushing is up to you; t runs exactly what you name. Results that nobody's
change could have affected come back at once from the cache. Everything else is tested on `main`
after you push.

Exit codes: `0` passed; `1` something failed that may be yours; `3` everything that failed also
fails on `main`; `2` t couldn't do what was asked. Each failure comes with its relevant output and
the command that reruns it.

`t cargo …`, `t build` and `t run` compile on the server's warm builds; programs land in
`./target`.

## After pushing

```
t inbox                               # failures on main traced to your commits
t main                                # what fails on main, since which commit, who is on it
t claim <item>   t release <item>     # say you are fixing a failure, or stopped
```

Fix your own failures on `main` forward. Claim someone else's before you work on it.

Agents use the same commands, or `t mcp` for MCP. Sessions are recognized by their session id
(`CLAUDE_CODE_SESSION_ID`, `PI_SESSION_ID`, `CODEX_THREAD_ID`); `t session name <name>` names one.

## Adding a check

A check is a shell command, or a cargo family that becomes one check per package:

```toml
[[check]]
name = "codemap"
run = "python3 tools/docs/codemap.py --check"
inputs = ["docs/codemap.md", "tools/docs/codemap.py", "**/Cargo.toml"]
```

`inputs` are the files the result depends on: a change to any of them runs the check again,
and nothing else does. List too few and a stale result is reused; list too many and it reruns
for nothing. `default = false` keeps a check out of `all` and out of `main`'s CI: it runs when named, or where
its `on` says (`on = ["stable"]`, `on = ["schedule:…"]`). Image builds, VM end-to-end tests and
long suites are declared this way.

The cargo families (`test/<package>`, `clippy/<package>`, `check/<package>`) need no globs for
Rust code: t derives what each package reads from the manifests and `Cargo.lock`, the way cargo
resolves it. That is the sources of the package and of every local crate it reaches, vendored
crates patched in with `[patch]` included, and the lockfile entries of only the crates it uses,
so a lockfile bump reruns the users of the bumped crate, not the workspace. A shell check that
builds packages names them instead of copying their directories as globs:

```toml
[[check]]
name = "image-registry-server"
run = "bash tools/ci/publish/oci-build.sh . crates/packaging/registry-server/container/Containerfile …"
cargo_packages = ["registry-server"]   # what building it reads,
cargo_dev_dependencies = false         # not what only its tests use,
inputs = ["tools/ci/publish/**", "crates/packaging/registry-server/container/**"]   # and these
```

The lockfile records every optional dependency, so a package reads the crates behind its
features whether a build enables them or not. Where that matters, as for `image-relay`, whose
release build leaves out much of what the workspace's features pull in, a hand-kept list checked
by `tools/policy/check_workflow_package_paths.py` can still be tighter.

Files a package reads that no manifest names (an `include_str!` of a file outside every crate, a
fixture a test opens) still have to be declared: in `tools/ci/suites.toml`, which generates the
families' `package_inputs`, or in the check's `inputs`.

Every check states the cores and memory it must be able to pass with (`requirement`, with a
default at the top of `t.toml`). A check that needs more fails, so a test that slowly needs more
shows up instead of being fed. t usually gives checks more when there is room.

A check that needs particular hardware says so, and runs only on a node that has it:

```toml
requires = ["arch=arm64", "/dev/kvm"]   # also "os=macos", or a label a node was given
```

`t status` lists the connected nodes and what each offers. The format is described in t's
[README](https://code.tilo.so/tilo/t#declaring-checks).

Some of `t.toml` is generated: the `package_inputs` of the cargo families and the `suite-*`
checks from [`tools/ci/suites.toml`](../tools/ci/suites.toml), and the `goldens` family from
`.config/nextest-filters.toml` and `.config/visual-goldens-image`. After changing those, run
`python3 tools/ci/t_toml.py write`; the policy tests fail while `t.toml` is out of date.

## Publishing

Checks can also publish: container images, signatures and releases, from `main` (development
artifacts) and from `stable` and release tags (releases). Their build halves are ordinary checks
you can run before pushing, such as `t test image-room-service`. [Publishing with
t](publishing-on-t.md) lists every publishing workflow, how it maps onto t and which are moved.
