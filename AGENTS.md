# Coding agents

## Scope and navigation

- Tilo's collaborative desktop/OS, apps, SDK and filesystem share a workspace.
- Use the [code map](docs/codemap.md); avoid repository-wide scans.
- Development: [CONTRIBUTING.md](CONTRIBUTING.md); prefer a tool's `--help` or guide.

| Work                  | Read first                                                           |
| --------------------- | -------------------------------------------------------------------- |
| Architecture/security | `docs/architecture/workspace.md`, `docs/security/README.md`           |
| Product flows         | `docs/architecture/flows.md`                                         |
| Collaboration         | `docs/architecture/workspace.md`                                     |
| Compositor            | `docs/headless-testing.md`                                           |
| Apps/UI               | `crates/ui/ui-core/HIG.md`, `docs/design-language.md`               |
| tilolang              | `docs/designs/app-language/README.md`                                |
| Dev VM/device         | `tilo device guide quickstart` (source: `tools/tilo-vm/guide/`) |

## What Tilo is

The [architecture overview](docs/architecture/README.md) states these; work keeps them:

- A workspace spans devices and has members: people and agents. It has one set of resources,
  shared under the permissions its owners set; only identity is per person.
- One identity system. Your devices share your identity, and it controls them. It is also
  your login at tilo.sh; there is no separate account. Your existing accounts (mail, cloud
  storage, calendars and others) connect to a workspace, to bring their data over or to keep
  using them there.
- Decentralized first: a server is a device that stores more. tilo.sh is our cloud offering.
- Compute is shared: a program goes wherever it fits, and a Linux VM exists only when one is
  needed. Apps are untrusted packages.
- Everything is resources, actions and views, in Rust and tilolang alike. People, agents,
  scripts and automations call the same actions, and every view of a resource shows the change.
- Views compose, can be shown again anywhere, and can be written quickly, also by an agent in a
  chat. The shell is a view.

- The default apps are enough for 95% of people: real Files, Writer, Ebooks, Settings, a video
  editor, never proofs of concept. Their UX is polished: Settings, Writer, the video editor and
  the rest have custom interactive animations and visualizations on their pages. None of this
  is optional.
- Tilo feels immediate: an app opens in under 100 ms when it's already on the device, and
  under 500 ms when it first has to be installed. Apps are small, and the web version loads
  about 1 MB of WASM and HTML.
- The shell is a view a person can replace entirely. It can look like Windows 95 or work like
  a Miro or Figma canvas, and tilolang is powerful enough to write either.
- Any program can be a Tilo app, a Minecraft launcher or a CAD program included. Apps climb
  a ladder from tilolang to fully hand-written Rust. Each step allows more customization, and
  every step keeps actions callable from anywhere and tied to views, history and undo,
  identity, permissions, views that can be shown elsewhere, coherent UX and good animation.
  Those are the default experience, never something an app opts into.

Apps are written in Rust or tilolang against one app interface. Every system service reaches
both through the same path; a second path for one language is a duplicate to remove.

## Principles

These hold for every app, the shell and the SDK. When a doc or code disagrees, these win.

1. **One resource/action/view model.** State is resources; changes are typed actions on them;
   views present resources. The Rust core, tilolang apps, the shell, agents and automations all
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

## Designing

Think about the person using what you build. When you're working on a detail, step back and
ask whether the larger thing could be done a better way; if it could, say so before going on.

- No traditional desktop or SaaS thinking: storage, permissions or accounts per app or per
  person, or a server that owns things. A workspace has one set of resources, many devices and
  many members.
- Don't rebuild the system: search, history, undo, sync, restore, tabs, who did what. If its
  version falls short, improve it there.
- Fail fast, hard and legibly; failing is no excuse for failing in an ugly way. An error is
  either one a person will realistically see, and is designed for that, or one that shouldn't
  exist at all: unwrap and fail. Avoid the middle.
- If you notice you're duplicating functionality, stop and flag it.
- Shared, good-looking components: modern, beautiful, coherent, animated.
- Real conditions: 10k items, long names, any aspect ratio, others and agents changing things
  while you look.
- No tests that copy the implementation. Aim for integration-test speed (milliseconds) with
  end-to-end depth (realistic user flows across systems).

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
- Build and test on a build server, never on the machine you edit on: `t test` covers
  the checks your change reaches, `t test <check>` one of them, `t cargo test --locked
  -p NAME` the tests of the packages you touched, and `t test policy` the policy suite
  (one step: `t test policy/<step>`). t gives each its own job there, keyed by what it
  reads, and keeps the builds those jobs use.
- Extensive checks (images, macOS, VMs) only when you change what they test.
- Session: `t cargo test --locked -p tilo-session MODULE::` names one of the session's
  tests (the desktop, its windows and its strips) there; `t cargo check --locked -p
  tilo-session --tests` type-checks them while you iterate. A test that starts another
  app (the lock screen, the on-screen keyboard) belongs to
  `t test`: t builds those apps for the job (`t.toml` `starts`), which a plain
  `cargo test` does not.
- Browser crates: `t cargo check --locked --target wasm32-unknown-unknown -p NAME` too.
- Never ignore a failure, weaken an assertion, or lean on a tautological helper.
- Test through production interfaces, including cancellation and denial.

## Run and inspect

- Tilo is primarily a native desktop/OS. Drive native apps and the desktop
  headlessly with `cargo headless` (no window; prints how to drive it, `--ready
  FILE` for scripts) and `tilo` / `tilo-control` resources and actions (`--help`
  on each, `--check` for a smoke test). Use `cargo desktop` for a window on the
  developer's desktop.
- Use `cargo web` and headless Chromium only when changing web/WASM code, never
  as a substitute for native headless testing. Never leave persistent browser
  driver servers running; always close browsers on exit with a short timeout.
- Prefer `tilo` / `tilo-control` resources and actions to pointer automation;
  inject input only to test input, hit-testing or painting.
- Images come from `shots` or a disposable headless session, never the user's
  desktop.
- Check visual changes in more than one theme.
- Clean up every compositor, browser, runtime directory, container and VM you
  start.
- `tilo device` owns dev VM lifecycle and swaps; no SSH/SCP deployment path.
  `tilo device guide images|kernel|scenarios` for those changes.

## Git and delivery

- Commit small and push straight to `main` (`git pull --rebase origin main`,
  then `git push origin HEAD:main`). There is no merge queue and no
  integration branch. The pre-commit hook is fast and reads only what the
  commit stages; the pre-push hook lints the packages the push changes through
  t's per-package clippy checks, and compiles the packages that build directly
  on them through its check families (only a compile error there refuses),
  answered from t's cache when a rebase changed nothing they read (or `$CARGO`,
  any cargo drop-in, when set). Neither covers the policy suite or a package's
  tests: those are t's, above.
- No tests while iterating. Run the focused tests once, when the work is done,
  then push.
- A red `main` is fixed by the next commit, never by a revert or a hold.
- After your change lands, watch CI on `main`; fix what you broke.
- App packaging is affected-only; full image builds stay explicit or scheduled.
