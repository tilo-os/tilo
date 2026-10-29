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

Once per clone, turn on the git hooks and merge drivers; every worktree of the clone has them:

```
python3 tools/dev/install-git-hooks.py
```

## How changes reach a release

- **`main`** is where changes integrate: push straight to it, no pull request, no queue. A push
  that isn't a fast-forward is refused; rebase on `main` and push again.
- **The push gate.** The pre-push hook runs t on exactly the commit being pushed, as committed
  (`--rev`: uncommitted changes and untracked files don't count, and no checkout is made). It
  has to be fast, a minute for a typical change, so it runs only what is cheap to catch and
  annoying to break (`tools/ci/pre_push.py`), of what the commit touches (`t scope --rev`):
  - `fmt-changed`, rustfmt of the Rust files the commit changes;
  - the policy steps (`policy/*`) and the generated-file and manifest checks (`t-toml`,
    `codemap`, `sdk-policy`, `lockfile`, `hakari`, `deny`) whose inputs it touches; the policy
    suite's own tests only when it touches `tools/policy/`. A policy step runs when
    `tools/policy/steps.toml` marks it `pre_push = true`, which it may only be when it builds
    nothing and reads no Git index of the tree: those take minutes each and wait for main;
  - `changes`, the diff gates (allow reasons, commit messages, interfaces);
  - clippy of the packages whose files it changes, not of their dependents;
  - the aarch64 size check of each app whose files it changes (`app-size/<app>-aarch64`), about a
    minute, when t has the commit's shared runtime built (main keeps it built, `idle`). A commit
    that changes what the runtime is built from (the toolkit, the SDK) or more than three apps
    skips it and says so; main's whole-collection size gate holds it to the budgets. Each
    push's decision, checked or skipped, is recorded in `.git/t-gate-sizes`.

  No tests, VM checks, images, suites or dependents: `main` runs those on every commit, or on
  its newest head (below), and tells the commit that broke one. The hook also prints the
  opt-in checks `t suggest` says the change touches, and runs none of them. A failure the change
  causes refuses the push; one that `main` has too, at the commit the push branched from,
  doesn't. A check red on `main` runs all the same and is compared with `main`'s one finding at
  a time: each lint of `clippy/*` by lint and file, each problem a `policy/*` step prints by the
  file it is about (`tools/policy/findings.py`), so a push that adds one is refused however red
  `main` is -- a file pushed over a limit is its own finding even when `main` is already red for
  other files. A check that fails as a whole (a compile error, a stale generated file) is
  compared as a whole, and the hook names each one that passed that way; a test check whose
  tests did not run is refused instead, since main failing it as a whole says nothing of which
  tests the change breaks. A test check is compared test by test: cargo's and nextest's tests,
  and each test of the policy suite's Python tests (`tools/policy/run_unit_tests.py`). A failure
  that a check's `fix` repairs is fixed for you to commit, never amended: into your working
  tree when you push `HEAD` (files you edited since are left alone), else as a patch in
  `.git/t-fix.patch`. Inputs the change doesn't touch are cached, so a push after a rebase costs
  little.
- **CI on `main`** runs every default check on each new commit, finds the commit a failure began
  with and tells whoever is behind it (`t inbox`). `main` is sometimes red; whoever broke it fixes
  it forward. Nothing is reverted.
- **Heavy checks on `main`.** The heavy checks that matter for `main`'s health (the SDK builds,
  the web and Android builds, the goldens, FUSE, render counters, examples, the security
  invariants, the OS and VM acceptance suites, the desktop and imager preflights, app lints, the
  portable toolkit build: `on = [..., "idle"]` in `t.toml`) run on `main`'s newest head whenever t has room left after the default checks and
  everyone's own runs, at a priority that never delays either. Each commit's key is recorded, so
  when one fails t bisects it to the commit that broke it and tells that commit's session, as for
  a default check; `t main` and the notice mark it as heavy. The size budgets are among them as
  one check per architecture over the whole collection (`app-sizes-<arch>`); the per-app size
  checks are not (there are too many).
- **`stable`** is the release branch. The stabilizer (usually an agent) picks a recent `main`
  commit and runs the heavy checks there too (images, VM and FUSE suites, size budgets, the goldens:
  `heavy = true` in `t.toml`) with `t stable verify <commit>`, which runs only those without a
  result for the candidate's inputs. For each failure it commits a fix (brought back to `main`) or an
  exclusion of unfinished work. It never weakens a check to make it pass. Only the stabilizer
  pushes to `stable`.

Many people and agents change Tilo at once, about a commit a minute. Gating every change on the
full suite would serialize them; this way landing takes minutes, a slip that takes seconds to
find never reaches `main`, and releasability is the stabilizer's job.

Generated files (`Cargo.lock`, `crates/workspace-hack`, the generated parts of `t.toml`, the code
map, the web desktop's app registry) are regenerated, not merged: where a rebase finds both sides
changed one, the merge driver takes `main`'s side (`.gitattributes`, `.githooks/merge-generated`),
and the gate's `t fix` puts back what your commit needs. `Cargo.lock` is only relocked: what the
merged manifests require is added, and no locked version moves.

## Cutting stable

Stable is cut from a green head of `main`, twice a day, by one command:

```
python3 tools/ci/stable_cut.py cut       # select, verify, and print the push
python3 tools/ci/stable_cut.py select    # only the candidate, and why recent heads are not
python3 tools/ci/stable_cut.py checks    # what a candidate is verified by
```

1. **Select.** The candidate is `main`'s newest commit none of whose default checks fails
   (`t main --coverage`; checks `t stable` excludes aside). t runs `main`'s default checks as it
   has room, so the candidate may have no result yet for some: those run with its verification.
2. **Verify.** `t stable verify <commit> <checks>` runs the gating set: every `heavy` check
   (images, the OS and VM acceptance suites, FUSE, sizes, goldens), mostly from the cache, since
   `main` ran most of them on the same inputs. Any failure holds the cut. A second run reports
   the performance checks (`perf-*`, `vm-acceptance-perf`, `weekly-latency`: on `stable` but
   not `heavy`), and `vm-acceptance` reports its budget scenarios as `over-budget` (`--budgets
   report`): t's shared, GPU-less workers measure the worker as much as the commit, so budgets
   gate again once they run on dedicated or GPU runners.
3. **Push.** For a candidate that passed, the command prints `git push origin
   <commit>:refs/heads/stable`; the stabilizer runs it. The push runs the publishing checks
   (`on = ["stable"]`: sign-\*, publish-\*, release-\*, submit-\*, seal-\*), which sign with
   the release keys in KMS and publish the OS, the apps, the RPMs and the kernel and its
   buildroot. Production (tilo.coffee) is not among them: its own session releases it.

A failure is fixed on `main` and the next head is cut, or the check is excluded with a reason
(`t stable exclude <check> --reason ...`) when the work is unfinished.

## Testing a change

```
t test <check|package|family|glob>      # exactly that, on your working tree
t test test/tilo-fs --test parser::empty   # one test
t test                                  # what failed for you last time
t suggest                               # what your change reaches, and the heavy checks it touches
t scope                                 # the checks your change touches (--names for $(...))
t test --rev HEAD $(t scope --rev HEAD --names)   # the last commit as committed
t fix                                   # apply the `fix` of what fails (fmt, hakari, lockfile, …)
```

While you work, test what you change: `t scope` names the checks your change touches (the
packages you touched, the command checks whose inputs changed), and `t test $(t scope --names)`
runs them; the push gate runs the cheap ones of them again on the pushed commit, cached where
nothing changed, and leaves the tests to you and to `main`.
Heavy checks run for you only when named: `t suggest` lists those your change touches (their key
differs from your base commit's), with the `t test <check>` that runs each. Skip one and `main`
still runs it when it has room, and tells you if your commit broke it. Results nobody's change
could have affected come back at
once from the cache.

The extensive checks (the OS images and their layers, the macOS checks, the VM suites) hold
scarce machines for hours: the one Mac, the KVM and large-disk nodes. Run one when you change
what it tests: an image layer's files, the runtime or an app on macOS, a VM scenario. Don't run
them for a small unrelated change: `main` runs them anyway (on its heads, when it has room, or
nightly) and bisects a failure to the commit that caused it, and `stable` runs them all on each
release candidate. `t test` runs only what you name, and `t test $(t scope --names)` only the
default checks your change touches.

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
Claude Code sessions in this repository see new `t inbox` notices as they arrive:
`.claude/settings.json` runs `t inbox --hook claude` at session start, on each prompt and after
each Bash command.

Agents use the same commands, or `t mcp` for MCP. Sessions are recognized by their session id
(`CLAUDE_CODE_SESSION_ID`, `PI_SESSION_ID`, `CODEX_THREAD_ID`); `t session name <name>` names one.

## Adding a check

A check is a shell command, or a cargo family that becomes one check per package:

```toml
[[check]]
name = "codemap"
run = "python3 tools/docs/codemap.py --check"
fix = "python3 tools/docs/codemap.py"    # what `t fix codemap` runs, and t itself on main
```

Nothing says what a check reads: t records what its job read as it runs (each file it looked
up, each directory it listed), and a later run whose files answer that the same way takes the
result; a change to anything else leaves it standing. `default = false` keeps a check out of `all` and out of `main`'s CI: it runs when named, or where
its `on` says (`on = ["stable"]`, `on = ["schedule:…"]`, or `on = ["idle"]`: `main`'s newest
head when t has room, bisected and attributed when it fails). Image builds, VM end-to-end tests
and long suites are declared this way.

The cargo families (`test/<package>`, `clippy/<package>`, `check/<package>`) are keyed by what
cargo reads to build each package: the manifests and `Cargo.lock`, the way cargo resolves them,
and what rustc read, so a lockfile bump reruns the users of the bumped crate, not the workspace.
Their tests are keyed by what they read as they ran, a fixture outside the package included.

Tilo's `check` and `clippy` families run under `tools/ci/t_cargo_compile.py`: for a browser crate
(`python3 tools/ci/package_sets.py wasm`) it first builds the library for
`wasm32-unknown-unknown` with the features the web build gives it, then runs the native command.
Code under `cfg(target_arch = "wasm32")` builds for no other target, so a native-only check of a
browser crate passes while the web build breaks. `test/<package>` stays native; the `wasm` check
lints the page runtime with every web app linked in.

Checks declare no cores, memory or disk: t books each job from what its runs measured (the
key's peak and parallelism, else its check's, else a default for its kind) and grows a job that
asks for more. `t.toml` refuses `requirement`, `cost` and a `[defaults]` table.

A check that needs particular hardware says so, and runs only on a node that has it:

```toml
requires = ["arch=arm64", "/dev/kvm"]   # also "os=macos", or a label a node was given
```

`t status` lists the connected nodes and what each offers. The format is described in t's
[README](https://code.tilo.so/tilo/t#declaring-checks).

The repository policy suite is one check per step, `policy/<step>` (`policy/tests/<step>` for
the suite's own tests): its command is in
[`tools/policy/steps.toml`](../tools/policy/steps.toml), which `tools/policy/run.sh` runs as a
whole. `t test policy` runs every step. A new step goes there.

Some of `t.toml` is generated: the `suite-*` checks from
[`tools/ci/suites.toml`](../tools/ci/suites.toml), the `policy/*` checks from
`tools/policy/steps.toml`, the package lists of the cargo families, the `goldens` family from `.config/nextest-filters.toml` and `.config/visual-goldens-image`, and the size
checks below from the default collection's apps and the runtime's budget. After changing those, run
`t fix t-toml` (or `python3 tools/ci/t_toml.py write`); `t-toml` fails while `t.toml` is out of
date.

A check that exists once per architecture (or per app) is declared once, with a `[check.matrix]`
that t expands into one check per value (t's README, "Matrices"): `name = "sign-{arch}"` with
`arch = ["x86_64", "aarch64"]` is `sign-x86_64` and `sign-aarch64`, with the same keys as if each
were written out. Scripts and tests that read `t.toml`'s checks read them expanded, through
`tools/ci/t_matrix.py` (`t_matrix.checks()`); `python3 tools/ci/t_matrix.py --only image-`
lists the expanded names.

## App and runtime sizes

Each app of the default collection has a size check per architecture, keyed only by what that
app's tapp is built from (its own crates, the shared runtime and engines it links, the manifests
and lockfile Cargo resolves the graph from, the packaging scripts):

```
t test app-size/notes-aarch64          # Notes' tapp against its budget
t test app-size/notes-x86_64
t test runtime-size-aarch64            # the shared runtime against its budget
```

They don't run on `main`'s commits by default, so run the ones for the apps your change
concerns: an app you rewrote, or `runtime-size-*` and a few apps for a change to the toolkit.

What every one of them builds alike, the runtime, its engines and the packing tools,
`thin-runtime-aarch64` and `thin-runtime-x86_64` build once (`tools/apps/t-release.sh
thin-runtime`). The size checks need it: `t test app-size/notes-aarch64` brings it along, and
receives its build directory, so the check itself compiles only Notes' own crates against the
shared runtime, packs and measures it. Its result stays cached until the runtime, its engines,
the packing tools or the collection's manifests change, which is far less often than any one app
changes. On arm64 the shared build's command takes about 16 minutes from a cold build
directory; with it cached, an app's check took 7 to 13 minutes on a loaded node, of which compiling
the app's own crate is 3 to 6 minutes and the rest is baking the runtime bundle and packing. Both
spend most of that waiting for the node's disk: the app's compile reads the shared build's
metadata (about 2 GB) from it.

The whole collection's gate, `app-sizes-x86_64` and `app-sizes-aarch64`, runs on each new head
of `stable`, when dispatched, and on `main`'s newest head when t has room (`idle`), where t
bisects a failure to the commit that took an app over its budget. Budgets and how to raise one:
[app package size](measurements/app-package-size.md), [runtime size](measurements/runtime-size.md).

## Test time per package

Every whole run of the `test` family records, per package, how many tests ran and how long
they took, into `target/test-timings.json` in the build directory of the node that ran the
job. `t log` stops at nextest's own summary and the node's build directory is not the
checkout's, so the table is read from the records, not from the log: the tool warns about a
package at least two seconds and 20% slower than
[`docs/measurements/test-timings.json`](measurements/test-timings.json). The warning never
fails a job. The baseline is moved deliberately, from a run that finished
its tests; [test time per package](measurements/test-timings.md) has the commands and what is
left out.

## Publishing

Checks can also publish: container images, signatures and releases, from `main` (development
artifacts) and from `stable` and release tags (releases). Their build halves are ordinary checks
you can run before pushing, such as `t test image-room-service`. [Publishing with
t](publishing-on-t.md) lists every publishing workflow, how it maps onto t and which are moved.
