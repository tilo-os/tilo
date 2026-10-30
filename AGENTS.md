# Coding agents

## Scope and navigation

- Tilo's collaborative desktop/OS, apps, SDK and filesystem share a workspace.
- Use the [code map](docs/codemap.md); avoid repository-wide scans.
- Development: [CONTRIBUTING.md](CONTRIBUTING.md); prefer a tool's `--help` or guide.

| Work                  | Read first                                                           |
| --------------------- | -------------------------------------------------------------------- |
| Architecture/security | `docs/architecture/workspace.md`, `docs/security/README.md`           |
| Collaboration         | `docs/architecture/workspace.md`                                     |
| Compositor            | `docs/headless-testing.md`                                           |
| Apps/UI               | `crates/sdk/ui-core/HIG.md`, `docs/design-language.md`               |
| tilolang              | `docs/designs/app-language/README.md`                                |
| Dev VM/device         | `tilo device guide quickstart` (source: `crates/dev/tilo-vm/guide/`) |

## Principles

These hold for every app, the shell and the SDK. When a doc or code disagrees, these win.

1. **One resource/action/view model.** State is resources; changes are typed actions on them;
   views present resources. Native apps, tilolang apps, the shell, agents and automations all
   use this one model. No second model beside it.
2. **State is CRDT-backed changes.** Resources are owned CRDT documents; edits are recorded
   changes (diff and splice text and lists, never replace them). A widget holds presentation and
   caches, never state.
3. **No tabs, search boxes or history in apps.** Tabs and window groups, search, history and
   undo, window chrome and titles are system functionality. An app shows one thing; its search
   button opens system search scoped to it.
4. **The system does what's needed more than once.** Storage, sync, presence, history,
   permissions, notifications, and indexing and search of what the workspace holds: if more than
   one app needs it, the system provides it and apps don't reimplement it. Find the existing
   system service before building anything app-local. An app adds only what is its own: a search
   provider over a service it connects to (a video site's catalogue, a mail server), never a
   private copy of what it already publishes.
5. **One set of UI components** for every app and the shell. An app adds its own component
   only when it really needs one and the shared library couldn't gain it; otherwise the shared
   component improves.
6. **The nice properties come by default.** Collaboration and sync between people and
   devices, searchability, composability, and agents and automations acting on an app's
   resources and actions are properties of the system architecture. Every app gets them by
   publishing its resources and actions, not because its author worked hard for them.

## Make the change

- Bind app edits to typed resource/actions, not click-only mutations.
- Shared controls, theme tokens, localisation and platform services.
- No floating windows, app title bars or private app search fields.
- Appearance changes apply live, through the shared motion primitives.
- Start apps empty; fixtures belong in tests, shots or explicit demos.
- Sandboxing, capability grants and cross-workspace isolation are the security
  boundary; never weaken them or bypass a denial with ambient credentials.
- No private trackers or discussions in public code, docs or messages.

## Build and test

- Do not set `CARGO_TARGET_DIR`, or point two worktrees at one target directory.
- Do not regenerate `Cargo.lock`; `cargo update -p NAME --precise VERSION`.
  Lock-only changes and generated files: `docs/policies.md`.
- Run the tests of the packages you touched (`cargo test --locked -p NAME`) and
  `bash tools/policy/run.sh`. `tools/ci/test-changed.sh` lints and tests what
  your change affects, edits included; `-- NAME` or `-E FILTERSET` narrows it.
- Extensive checks (images, macOS, VMs) only when you change what they test.
- Shell: `cargo test --locked -p shell --test it MODULE::` and
  `cargo check --locked -p shell --tests`.
- Browser crates: also check them for `--target wasm32-unknown-unknown`.
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

- After your change lands, watch CI on `main`; fix what you broke.
- App packaging is affected-only; full image builds stay explicit or scheduled.
