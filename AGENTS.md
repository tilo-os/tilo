# Agent instructions

## Project Scope

Tilo is a collaborative Linux OS and desktop environment on **Fedora Bootc**: Niri-like
strips of non-overlapping windows, inside workspaces that carry their own collaborators,
permissions and host. It is a stack, not a single app:

- An immutable container-based OS image
- A Wayland compositor
- A P2P FUSE filesystem that projects remote files and hydrates on demand
- A CRDT-native UI toolkit used by system apps
- ~40 native system apps

Treat these as deeply interrelated — a change in `ui` or `fs` can break the OS image build;
changes to the compositor affect sandboxing and collaboration semantics.

Collaboration lives *below* applications rather than inside them — it has to work in the
ebook reader, the file manager and the terminal, not only in the document editor.
**Applications own neither their pixels nor their storage.** An app declares its UI through
`ui`, which the compositor draws, and exports **resources** — hierarchical and typed (an
ebook; the highlights and annotations inside it) — each carrying its own **actions**. The
state behind a resource is a CRDT the system owns.

Most user-facing design questions reduce to that one decision:

- **Accessibility and automation are the same door.** A screen-reader user, a script and an
  agent browse resources and invoke actions; none touch the UI. A capability reachable only
  by clicking is not finished.
- **Search is a system service.** Apps register a search provider per resource type and
  say what each hit is (a message, a video, a file match — `ui::search::SearchRow`); they
  never draw a search field. A hit opens the app at that position through the resource's
  `open` view action with the hit's anchor (`ViewCx::on_open` in a framework app), so a
  person's click and an agent's call land in the same place. Apps open the platform search
  narrowed to themselves or one of their parts (`ui::search::SearchRequest`), and narrow
  lists or find in documents only through the framework's filter field and find bar.
- **Undo, history, device sync and theming are system services**, not forty implementations.
- **Collaboration reduces to transport.** Invite someone to a workspace, give them a role per
  app; each machine draws natively and state syncs as a CRDT. Agents are ordinary
  collaborators — same presence, same cursor colour, same attributed history.
- **A workspace is a unit of collaboration** — its own collaborators, permissions and host —
  not a region of screen space.
- **Apps are ~2 MB and are not installed.** Search, press enter, it downloads and opens;
  unused copies are evicted; keeping one is opt-in. Sandboxing (Landlock + Wayland + portals)
  is what makes that safe, and it is the security boundary — never weaken it for convenience.
- **The filesystem is collaborative and versioned.** Apps register (de)serialisation for
  legacy formats, so `fs` understands both native resources and `.docx`; history, diff and
  undo look the same in the file manager and inside the app.
- **Rendering and state are decoupled**, which is why a workspace can be hosted on another
  machine and still render locally at full speed.
- **The OS owns window chrome**; apps must not draw their own title bar. Windows never
  overlap — reject designs that reintroduce floating.

## Repository Layout

Single Cargo workspace, no submodules. Prefer explicit paths over `git add -A`.

- `crates/session/shell/` — Wayland compositor (`smithay` + `vello`/`wgpu` + Iroh): workspaces, strips,
  sandboxing, collaboration. Also the `tilo-control` CLI and the `shots` renderer.
- `crates/sdk/ui-core/` — the internal toolkit behind `tilo-app`: retained-mode, CRDT-native. `wgpu`+`vello` (render), `taffy` (layout),
  `cosmic-text` (text), `automerge` (state), `winit` (windowing), `fluent` (i18n).
- `crates/system/fs/` — FUSE3 + Iroh, mounted at `/var/workspace`, cache at `/var/cache/tilo-fs`.
- `apps/` — ~40 app crates built on `ui`.
- `crates/<domain>/` — collab, doc, runtime, packaging, session, system, text, format.
- `os/` — Containerfile, systemd units, image build scripts.

## Build & Test

Run plain `cargo`. The toolchain is pinned in `rust-toolchain.toml` and the whole workspace is
edition 2024 — don't add a crate on an older edition. `tools/ci/check-build-env.sh` names any
missing native library, which Rust otherwise reports as `pkg-config exited with status code 1`
inside a `-sys` crate. How those libraries get there is per-machine and `DEVELOPMENT.md`'s
business, not this file's.

Functions take usually ≤5 params, never more than 8: clippy denies it and no `allow` may excuse it.

**Never set `CARGO_TARGET_DIR`, and never point two worktrees at one target directory.** The
first is hashed into sccache's cache key, so it costs every hit even when set to the path
cargo would have picked anyway; the second collides silently and hands you the other
worktree's binary. Delete a landed worktree's `target/` — those bytes serve one worktree, the
same bytes in the cache serve all of them. `DEVELOPMENT.md` carries the measurements.

Scope tests to what you are proving — the compile dominates, and a whole-crate run is minutes:

```bash
cargo test -p shell --lib room_activity
cargo test --locked -p shell --test it interaction_test::   # headless Wayland state, no GPU
```

Shell integration files are modules of the single `it` target, not individual
Cargo targets. Use `--test it MODULE::` to select a module, or its full
`MODULE::TEST` path with `-- --exact` for one case. For remote native execution,
see [focused native tests](DEVELOPMENT.md#fast-checks-before-assembly).

Run `cargo check -p shell --tests` before finishing. A `--lib` run never compiles integration
targets, which is how test files have silently stopped building for days at a time.

Before pushing:

1. Stage only the intended paths, then run `.githooks/pre-commit`; it checks changed-file
   formatting, exact staged Rust blobs for unexplained `allow`s, repository contracts and secrets.
2. Run the narrow tests that prove the change, plus the relevant all-target compile check
   (for shell work, `cargo check -p shell --tests`).
3. Commit the candidate, then inspect the affected plan with
   `python3 "${TILO_CI_TOOLS:?stable CI tools required}/offline_affected_plan.py" --root "$PWD" --bundle "${TILO_CURRENT_WORKTREE_METADATA_BUNDLE:?source-bound CI export required}" --base origin/main --shards 2`.
   Point `TILO_CI_TOOLS` at `tools/ci` in a current trusted checkout, even when this worktree
   is older. Set `TILO_CURRENT_WORKTREE_METADATA_BUNDLE` to the exact-source CI bundle from
   `dev-source-metadata.yml` (tools/ci/README.md). The launcher validates the committed clean
   source, complete workspace/member manifests, lockfile and targets, remaps all metadata paths,
   and runs this worktree's selector while forbidding every non-Git subprocess. A stale
   selector cannot invoke local Cargo through a default, an absolute path or a shell fallback.
   Do not reuse an unverified export or manufacture a partial graph. Without a verified bundle,
   leave selection to normal CI. The current direct selector also rejects a local default;
   older worktrees require the stable launcher rather than their historical relative command.
   Unexpected full-workspace selection is a bug to understand before pushing.
4. Push a task branch and use a PR. Do not probe `main` with a direct push. `.githooks/pre-push`
   refuses a branch that conflicts with the current main or leaves `crates/workspace-hack` stale,
   the two rejections CI otherwise reports a minute after the push. After pushing, let
   the Codex CI hook report completion; inspect a failure once with
   `fgj actions run view <run> --log-failed` and rerun only the failed job when it is transient.

Full image: `./os/build.sh`; boot it with `./os/test-vm.sh`. VM login `tester` /
`tilo`.

## Architecture Essentials

### `ui` — five passes per frame

Every widget implements `Widget` (`crates/sdk/ui-core/src/widget/mod.rs`):

1. `sync(state, path)` — pull visual state from the Automerge document
2. `build_layout(taffy, theme, node_map)` — build the Taffy tree; self-sizing widgets
   implement `measure`
3. `apply_layout(taffy, node, origin)` — computed layout → the widget's `Rect`
4. `draw(scene, transform, text_renderer, theme)` — render into a Vello scene
5. `handle_event(...)` — winit events, which **mutate the CRDT directly**

There is no message-passing layer. Consequences:

- Widget structs hold render and layout caches **only**. State lives in `State::doc`, an
  `AutoCommit`.
- Undo/redo snapshot the whole document (`State::commit_action`). Widgets mark undo points
  with `needs_commit()`/`on_commit()` and restore ephemeral state (caret, selection) via
  `get_state`/`set_state`.
- Focus is 2D and directional — `App::focusable_order` in `crates/sdk/ui-core/src/app.rs`, not tab order.
- **Theming is OS-controlled.** Read every colour and spacing value from the `Theme` passed
  to `draw`; never hard-code. Same for strings: pass `fluent` keys, never literals.
- **Never hand-roll a text input, button, dialog or control.** Extend the shared widget — it
  carries correctness, accessibility and theming that a local copy will not.
- **Apps start empty.** No seeded feeds, messages, events or documents on the live path.
  Sample data belongs behind `--shots` or a demo flag, never in the app a user opens.
- Rewriting a whole CRDT `Text` or list on every keystroke blows up tombstones. Diff and
  splice.

### `shell` — `Workspace → Strip → WindowGroup → Tab`

Tabs tile within a group; groups scroll horizontally within a strip; strips stack vertically;
workspaces switch via the wheel disc. Tabs are a **frame** primitive grouping arbitrary apps —
not per-app tabs.

Each workspace is a bubblewrap sandbox with its own file-access policy; a path outside it
prompts the user, and approval can be scoped to the workspace. The Wayland state is decoupled
from the vello/wgpu paint layer so it runs headless.

### `fs`

Iroh content addressing, `blake3` hashes, `moka` cache. Metadata arrives immediately over
gossip; content hydrates on first read — so `open()` does not mean the bytes are local.

### Actions and resources

`crates/collab/actions` (`tilo-actions`) is the app-facing registry: `resource.rs`,
`action.rs`, `registry.rs`, `executor.rs`, `envelope.rs`. An app registers its resources and
the actions on them; the shell serves them into search, the palette and the control socket.

**This is the app's public surface.** Automation, accessibility and agents use it and nothing
else. When you add a capability to an app, add the action — a feature that exists only as a
click handler is invisible to every one of them.

## Driving a running desktop

**There is one story. Do not build a second one.**

`tilo-control` is the CLI over the compositor's control sockets (never in `/tmp`: anyone could
bind a shared name first). Every image ships it at `/usr/bin/tilo-control`; from a checkout run
`cargo run -q --bin tilo-control --`. It has two halves; reach for the first.

The semantic half needs no development build and no environment variable: every compositor
serves `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock` in the **device admin's** session (the
`wheel` account whose identity owns the device) once first login is complete, to that account's
own unconfined processes — its login or ssh — and never to an app. On a real device, ssh in as
the admin and run `tilo-control actions list`. A development session started with
`$TILO_TEST_CONTROL` serves both halves there, and a set `$TILO_TEST_CONTROL` routes every
subcommand to it.

Semantic — how a collaborator, an agent or a script drives an app:

```bash
cargo run -q --bin tilo-control -- actions list                 # what this desktop can do
cargo run -q --bin tilo-control -- actions invoke PROVIDER ID   # do it: no coordinates
cargo run -q --bin tilo-control -- windows                      # which windows are open, in View terms
cargo run -q --bin tilo-control -- state-dump --pretty          # what it believes is on screen
```

Input — for when the thing under test *is* the pointer, hit-testing or painting (development
sessions only):

```bash
cargo run -q --bin tilo-control -- list-targets      # named rects, via Widget::test_id
cargo run -q --bin tilo-control -- click NAME        # also click-at X Y | drag | key | type
cargo run -q --bin tilo-control -- snapshot PATH     # PNG of the live scene
```

Prefer the semantic half: it is the surface real users and agents depend on, so exercising it
is worth more than a synthetic click that proves only geometry.

The installer and the greeter run before any compositor exists; they answer the same semantic
ops on their own console socket, so it is still `tilo-control --socket <path> actions list`
(docs/agent-action-api.md). In a VM that socket is `os/dev/vm.sh console`. Add `fn test_id(&self) ->
Option<&str>` to a widget to make it addressable by name; `Parent/Child[index]` paths work
without opting in.

Stills and video with no compositor and no display:

```bash
cargo run --bin shots -- --list-scenarios
cargo run --bin shots -- --mode offscreen --out target/<dir>
cargo run --bin shots -- --mode recording --scenario <name> --out target/<file>.h264
```

Output is identical to the live binary except for live-surface-only bugs.
`TILO_SHOTS_THEME=light|dark|high-contrast` — check more than one; single-theme blind spots
have shipped repeatedly.

`scripts/headless-examples/` is the worked-example index: `spawn-headless.sh` starts a
headless compositor and prints `READY <socket>`; `_control.py` is the socket client to inline
rather than rewrite; `full-loop.py` goes from nothing to a PNG in about five seconds.

**Never** run `scripts/screenshot-app.py`, and never screenshot the user's real desktop.
Debug overlays need `TILO_UI_DEBUG` **and** `debug_assertions`.

## Dev VMs

To run your build in the real Tilo OS image (a headful session, real DRM and input, VM-wide
services), use the one tool, `tilo-vm`. Start here:

```bash
cargo build -p tilo-vm && target/debug/tilo-vm guide quickstart
```

You get a **slot** in a shared dev VM: your own user, session and layer stack, swapped with
`tilo-vm swap <component>` and undone with `tilo-vm rollback`. Driving stays the one story above:
`tilo-vm control …` runs the VM's `tilo-control` against your slot, and `tilo-vm shot` is its
snapshot. `tilo-vm doctor` checks the host and prints each fix. Never deploy into a VM with raw
`ssh`/`scp`. Image-level changes (Containerfiles, units, `/etc`, packages) go through
`tilo-vm vm upgrade --from local`: it rebuilds only what changed, moves the running VM onto the new
image with bootc and reboots it; `tilo-vm vm rollback-image` goes back (`tilo-vm guide images`).
Kernel changes (`os/kernel/config-fragments/`, `os/kernel/patches/`) go through
`tilo-vm swap kernel`: it builds incrementally from the release build's inputs, installs the
kernel VM-wide and reboots (`tilo-vm guide kernel`). Installer changes go through
`tilo-vm scenario install`: a throwaway VM installs from your medium through the installer's
actions and cold-boots the result (`tilo-vm guide scenarios`).

## Cargo.lock is committed — do not regenerate it

Move one crate at a time with `cargo update -p <crate> --precise <version>`. A fresh resolve
moves hundreds of transitive versions at once, and a break surfaces inside a dependency rather
than where you touched. Treat a lock change as a real change: read the diff and rebuild before
pushing. `--locked` in CI. Commit a lock-only update with the command declared,
`TILO_CARGO_UPDATE='-p <crate> --precise <version>' git commit`; the pre-commit hook refuses any
other lock change without a manifest beside it (DEVELOPMENT.md, "Pre-commit hook").

Tilo's own crates use one RustCrypto generation, the one iroh uses: `ed25519-dalek` and
`x25519-dalek` 3, `rand`/`rand_core`/`rand_chacha` 0.10, `sha2` 0.11, `hmac` 0.13,
`chacha20poly1305` 0.11. Take OS randomness from `getrandom::SysRng`, wrapped as
`UnwrapErr(SysRng)` where an infallible `CryptoRng` is required. The previous generation is in
the lock only through third-party crates (openmls, frost-ed25519, authenticode, smithay, …); do
not add a direct dependency on it. The two sanctioned exceptions exist because the newest
releases of those dependencies still require it: frost's `rand_core` 0.6 is reached only
through `threshold-identity`'s RNG bridge, and tilo-crypto keeps authenticode's stack
(`authenticode-sha2`, `x509-cert`/`cms` 0.2 on `der` 0.7).

## Traps that have cost real time

- **Adding a field to `Workspace`/`WorkspaceTree`/`Strip` in `crates/session/shell/src/shell/state.rs`
  ripples to every by-field test fixture.** Symptom `E0063: missing field`;
  `crates/session/shell/tests/concept_parity_spec.rs` is 2200+ lines.
- **`git worktree remove --force` permanently deletes untracked files.** `git status --short`
  and commit before removing.
- **`git stash` is unsafe here** — the stash stack is shared across worktrees and concurrent
  sessions. Commit WIP instead.
- **Never write a `pkill`/`pgrep -f` pattern that matches your own command line.**
- **Clean up compositors and runtime dirs you spawn.** Leaked compositors hold DRM
  descriptors and starve the GPU for every session on the machine.
- **Verify a "known-red" test list at your own base commit.** A handed-down one is usually
  from a different tree.
- **A check that cannot observe its subject must fail, not skip.**

## Verification covenant

Declared in the header of `crates/session/shell/tests/concept_parity_spec.rs` and applying to **all** tests:
never `#[ignore]` a failing test, never weaken an assertion, always assert on the production
path rather than a tautological helper.

## Where to read first

`docs/architecture/{README,overview,collaboration,compositor,security}.md`,
`docs/headless-testing.md`, and `crates/sdk/ui-core/HIG.md` before non-trivial changes in
those subsystems. `DEVELOPMENT.md` for the build environment.

## CI

Use a task branch and a pull request for routine integration. Group related,
tested changes into small coherent increments; do not push work-in-progress or
minute-by-minute fixups directly to main. Keep the PR updated rather than opening
another PR for each iteration. Open the PR ready (not draft) and move on: it lands
automatically once the required checks (`CI / affected packages` and
`CI / fast quality`) pass on its current head. It does not need to be up to date
with main; resolve only real conflicts. Park a PR with draft, the `hold` label or
a `WIP` title. If a landed PR breaks main, it is reverted automatically and the
PR gets a comment on how to reland. Review is optional and informational and never
blocks integration. See `DEVELOPMENT.md`.

Keep check/lint results for every main commit. Affected Tilo/Linux apps package
after relevant main changes; unchanged collection members are reused. Full OS
and desktop distribution builds remain explicit or scheduled; do not restore
broad per-push full builds to obtain a development artifact. Use the existing workflow's
manual dispatch and the smallest applicable app/architecture inputs instead.

The existing CI scheduler owns runner slots, admission and queue order. Submit
work normally; do not negotiate build slots with other agents, reserve workers,
or repeatedly poll a queued job. Use automatic completion notifications and
investigate concrete failures. Infrastructure maintenance coordination is a
bounded operator task, never a prerequisite for ordinary development.

After a successful normal `git push`, final CI status is automatically sent to this task when its runs finish. Keep relevant CI green before merging; use `fgj actions` to investigate failures.
