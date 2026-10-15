# Running Tilo's checks with t

[t](https://code.tilo.so/tilo/t) runs Tilo's tests, lints and builds on a shared server
and caches each result by the files it depends on. It is how you check a change before
pushing it to `main`, and it is also the CI on `main`: the server follows `main` and runs every
check on each new commit. People and agents use it the same way, from the command line or
through MCP.

The checks are declared in [`t.toml`](../t.toml) at the root of the repository. Change that
file to add or change a check; nothing else needs to know about it.

## Setting up

```
cargo install --locked --git https://code.tilo.so/tilo/t
```

Then put the server and your token in `~/.config/t/config.toml` (mode `0600`):

```toml
server = "https://code.tilo.so/t"
token = "…"
```

Ask a maintainer for a token. `T_SERVER` and `T_TOKEN` override the file. Your machine needs
neither a Rust toolchain for Tilo nor any of its system dependencies: everything runs on the
server, in the same builder image CI uses.

## Before you push

```
t
```

runs the default checks your files affect: the tests, Clippy and policy checks of every
package your change reaches, against your working tree, uncommitted changes included. Only
the files that changed are uploaded. A check whose inputs nobody has changed returns the
stored result at once, whoever ran it first.

What the exit code means:

| code | meaning |
|---|---|
| `0` | everything passed |
| `1` | something failed that may be yours |
| `3` | everything that failed also fails on `main`: not caused by your change |
| `2` | t could not do what was asked (no connection, bad arguments) |

Each failure comes with its relevant output and the command that reproduces it.
`t log <check>` shows the full log.

## Everyday commands

```
t suggest                      # what to run next: what failed for you, what your change affects
t test clippy                  # one family (test, clippy, check, goldens), a check (fmt, deny, wasm, ...), a package, a check id, a glob, or `all`
t test test/tilo-fs            # one package's tests
t test 'suite-*'               # the suites outside Cargo, one check per group of tools/ci/suites.toml
t test 'nightly-*'             # the scheduled suites (also 'weekly-*'), only when asked for
t test test/shell --test 'interaction_test::'   # selected tests; never cached
t failed                       # run again what failed for you recently
t cargo check -p shell --tests # any compiling cargo subcommand, output streamed
t build -p terminal            # build on the server; the programs land in ./target
t run -p ebook -- --help       # build on the server, run here
t status                       # how busy the server is: running and queued jobs
```

Builds reuse a build directory on the server that already holds a build of a nearby version
of the tree, so a `t cargo` or `t test` of a small change compiles only what the change
touches, even when the server is busy with other people's work.

## `main`

```
t main                         # what fails on main, since which commit, who is on it
t claim test/tilo-fs           # say you are fixing it
t release test/tilo-fs         # say you stopped
```

CI on `main` informs; it never reverts anything. If a failure there is yours, fix it forward.
If you pick up someone else's, claim it so nobody duplicates the work. How changes move from
`main` to the `stable` branch is described in [DEVELOPMENT.md](../DEVELOPMENT.md#routine-delivery-workflow).

## Agents

`t mcp` serves all of the above as MCP tools over stdio. Agent sessions are recognized by
their session id (`CLAUDE_CODE_SESSION_ID`, `PI_SESSION_ID`, `CODEX_THREAD_ID`), so their runs,
failures and claims show up as theirs. `t session` shows the current session's record, and
`t session name <name>` gives it a readable name.

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
for nothing. `default = false` keeps a check out of a plain `t` run, so it only runs when asked
for by name (and on `main`).

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
