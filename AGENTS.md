# Coding agents

## Scope and navigation

- Tilo's collaborative desktop/OS, apps, SDK and filesystem share a workspace.
- Use the [code map](docs/codemap.md); avoid repository-wide scans.
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

- State is an owned resource/CRDT; a widget holds presentation and caches.
- Bind app edits to typed resource/actions, not click-only mutations.
- Shared controls, theme tokens, localisation, platform services; search, window
  chrome, undo, history and collaboration belong to the system.
- No floating windows, app title bars or private app search fields.
- Appearance changes apply live, through the shared motion primitives.
- Start apps empty; fixtures belong in tests, shots or explicit demos.
- Diff and splice CRDT text and lists instead of replacing them.
- Sandboxing, capability grants and cross-workspace isolation are the security
  boundary; never weaken them or bypass a denial with ambient credentials.
- No private trackers or discussions in public code, docs or messages.

## Build and test

- Do not set `CARGO_TARGET_DIR`, or point two worktrees at one target directory.
- Do not regenerate `Cargo.lock`; `cargo update -p NAME --precise VERSION`.
  Lock-only changes and generated files: `docs/policies.md`.
- `t scope`, then `t test <checks>`; `t suggest` lists the extensive ones.
- Extensive checks (images, macOS, VMs) only when you change what they test.
- Shell: `cargo test --locked -p shell --test it MODULE::` and
  `cargo check --locked -p shell --tests`.
- Browser: `check/<crate>`/`clippy/<crate>`, and `wasm` for the web runtime.
- Never ignore a failure, weaken an assertion, or lean on a tautological helper.
- Test through production interfaces, including cancellation and denial.

## Run and inspect

- Prefer `tilo action`/`tilo-control` resources and actions to pointer
  automation; inject input only to test input, hit-testing or painting.
- Images come from `shots` or a disposable headless session, never the user's
  desktop.
- Check visual changes in more than one theme.
- Clean up every compositor, runtime directory, container and VM you start.
- `tilo device` owns dev VM lifecycle and swaps; no SSH/SCP deployment path.
  `tilo device guide images|kernel|scenarios` for those changes.

## Git and delivery

- After integration, inspect `t inbox`; fix what you broke, claim what you did
  not.
- App packaging is affected-only; full image builds stay explicit or scheduled.
