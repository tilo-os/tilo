# Coding agents

## Scope and navigation

- Tilo's collaborative desktop/OS, apps, SDK and filesystem share a workspace.
- Use the [code map](docs/codemap.md); avoid repository-wide scans.
- Read only the relevant sections of [docs/policies.md](docs/policies.md).
- Development: [choose a task](docs/development/README.md); prefer a tool's
  `--help` or guide.

| Work                  | Read first                                                           |
| --------------------- | -------------------------------------------------------------------- |
| Architecture/security | `docs/architecture/overview.md`, `docs/architecture/security.md`     |
| Collaboration         | `docs/architecture/collaboration.md`                                 |
| Compositor            | `docs/architecture/compositor.md`, `docs/headless-testing.md`        |
| Apps/UI               | `crates/sdk/ui-core/HIG.md`, `docs/design-language.md`               |
| Builds/integration    | `docs/t.md`                                                          |
| Dev VM/device         | `tilo device guide quickstart` (source: `crates/dev/tilo-vm/guide/`) |

## Make the change

- Read production code and tests first; inspect history for regressions.
- Preserve existing capabilities; ask when intent is unclear.
- State: owned resource/CRDT. Widgets: presentation/cache, not a second state
  store.
- Bind app edits to typed resource/actions; do not add click-only mutations.
- Use shared controls, theme tokens, localisation and platform services.
- Search, window chrome, undo, history and collaboration belong to the system.
- Do not add floating windows, app title bars or private app search fields.
- Apply appearance changes live; use the shared motion primitives and
  reduced-motion policy.
- Start apps empty; fixtures belong in tests, shots or explicit demos.
- Diff/splice CRDT text and lists; do not replace them on every edit.
- Never weaken sandboxing, capability grants or cross-workspace isolation.
- Never bypass denials with ambient credentials or unconfined fallbacks.
- Public code/docs/messages must not reference private trackers or discussions.
- Explain non-obvious decisions, not code, names or historical commentary.

## Build and test

- Use the pinned toolchain, edition 2024 and `--locked`; select affected crates.
- Do not set `CARGO_TARGET_DIR` or share target directories between worktrees.
- Do not regenerate `Cargo.lock`; use `cargo update -p NAME --precise VERSION`.
- Follow `docs/policies.md` for lock-only changes, generated files and
  dependency exceptions.
- Run the affected checks: `t scope`, then `t test <checks>`; `t suggest` lists
  extensive checks.
- Rerun failures, not whole suites; distinguish inherited failures from new
  ones.
- Shell integration tests: `cargo test --locked -p shell --test it MODULE::`.
- Shell changes also need `cargo check --locked -p shell --tests` and desktop
  smoke coverage.
- Browser changes need wasm checks; native tests cannot compile wasm-only code.
- Use `check/<crate>`/`clippy/<crate>` for browser crates and `wasm` for the
  complete web runtime.
- Never ignore a failure, weaken an assertion or test only a tautological
  helper.
- A check unable to observe its subject must fail, not silently skip.
- Test through production interfaces; include cancellation, denial and reconnect
  where relevant.
- Inject paths/stores; use `tilo-test-env` only when the environment itself is
  the interface.
- Report the exact checks run and remaining gaps; do not equate implementation
  with verification.

## Run and inspect

- Prefer `tilo action`/`tilo-control` resources and actions over pointer
  automation.
- Use input injection only for input, hit-testing or painting tests.
- Use `shots` or a disposable headless session for images; never capture the
  user's desktop.
- Check visual changes in multiple themes; do not assume headless proof covers
  hardware.
- Clean up every compositor, runtime directory, container and VM you start.
- Do not use `pkill`/`pgrep -f` patterns that match your own command.
- Use `tilo device` for dev VM lifecycle/swaps; do not invent an SSH/SCP
  deployment path.
- Use `tilo device guide images|kernel|scenarios` for the corresponding change.

## Git and delivery

- Preserve concurrent edits; stage explicit paths, not `git add -A`.
- Do not use the shared stash; never remove a worktree containing uncommitted
  work.
- Install hooks with `python3 tools/dev/install-git-hooks.py`; run
  `.githooks/pre-commit`.
- Follow the user's review/commit/push instructions; otherwise use `docs/t.md`'s
  workflow.
- Keep unstable `main` and release-qualified `stable` distinct; do not introduce
  PR auto-merge.
- Leave runner admission and queue order to the scheduler; do not reserve slots
  or poll queues.
- After integration, inspect `t inbox`; fix regressions you caused and claim
  others before work.
- App packaging is affected-only; full image/distribution builds stay explicit
  or scheduled.
- Get development artifacts through the existing smallest applicable
  build/dispatch.
