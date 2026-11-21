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

## What Tilo is

People and agents work in one shared workspace instead of each needing a whole environment of
their own. Every device (a laptop, a phone, a server, a browser tab) holds the workspace or
reaches it. Compute in the workspace is shared: a program goes wherever it fits, and a Linux VM
exists only when a program needs one. Apps are untrusted packages. Identity belongs to people
and devices, not to a service.

A workspace has one set of resources: its files, mail, calendar, vault, models, connections,
agents and skills. Everyone in the workspace uses that one set. There is no second copy per
person and nothing that only an owner can reach. The workspace's owners decide who may do
what, through permissions they set. The code never decides that something can't be shared, such
as a password in the vault. Only things that belong to a person's identity are per person:
their keys and devices, appearance, and device settings.

Tilo is a small Rust core plus apps written in tilolang. The core holds workspace state, sync,
identity and permissions, the tilolang runtime, running programs, and drawing views. Everything
a person sees is tilolang: apps, the shell, Chat, agent views. A tilolang change needs no Rust
build and can't break the core. A Rust app gets replaced by a tilolang app built from what the
Rust app does, and the Rust app is deleted in the same piece of work. Don't add new Rust apps or
app UI in Rust.

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
- Build and test on a build server, never on the machine you edit on: `t test` covers
  the checks your change reaches, `t test <check>` one of them, `t cargo test --locked
  -p NAME` the tests of the packages you touched, and `t test policy` the policy suite
  (one step: `t test policy/<step>`). t gives each its own job there, keyed by what it
  reads, and keeps the builds those jobs use.
- Extensive checks (images, macOS, VMs) only when you change what they test.
- Session: `t cargo test --locked -p tilo-session MODULE::` names one of the session's
  tests (the desktop, its windows and its strips) there; `t cargo check --locked -p
  tilo-session --tests` type-checks them while you iterate. A test that starts another
  app (the lock screen, the on-screen keyboard, the quick-settings panel) belongs to
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

## How agents go wrong here

The whole codebase is written by agents, at hundreds of commits a day. Nobody reads all of it,
so the ways agents go wrong pile up. Check your work against each of these:

- **A fix that adds a path.** A special case, a retry, a flag or a second reader makes the
  symptom go away and leaves the cause. When the same thing breaks again, the earlier fixes are
  the bug. Find the design that keeps breaking and change it once, deleting the patches.
- **Another "single" implementation.** A new crate meant to replace three old ones is a fourth
  one until the old ones are deleted. Don't start a replacement you won't finish. Move the users
  of an existing copy onto the one that stays, and delete the rest. Deleted code is the measure.
- **Building beside instead of finding.** Before writing a store, a sync path, a sign-in, a
  presence feed, a settings page or a place agents start from, find the one that exists. There almost
  certainly is one.
- **Waiting instead of fixing what makes you wait.** A build, test or deploy that takes hours
  should take minutes. Don't plan around slow infrastructure. Fix it, or report it to whoever
  owns it, with the timings.
- **Done because the code exists or a test passes.** Done means you used it the way a person
  would: in a headless Tilo shell with a screenshot you looked at, or through the same actions
  an agent uses. A browser check proves the web page, not Tilo.
- **Reporting things as fine.** Say what is broken, how badly, and what you didn't check. A red
  check is never written down as acceptable: fix it or delete it.
- **Eroding what nobody measures.** Duplication, the developer's experience, the user's
  experience, the agent's experience, and the model above all decay when every change only
  climbs its own goal. Leave each of them as good as you found it or better.

## Git and delivery

- Commit small and push straight to `main` (`git pull --rebase origin main`,
  then `git push origin HEAD:main`). There is no merge queue and no
  integration branch. The pre-commit hook is fast and reads only what the
  commit stages; the pre-push hook lints the packages the push changes, through
  the `cargo` on your PATH or `$CARGO` (any cargo drop-in, one that builds on
  another machine included). Neither covers the policy suite or a package's
  tests: those are t's, above.
- No tests while iterating. Run the focused tests once, when the work is done,
  then push.
- A red `main` is fixed by the next commit, never by a revert or a hold.
- After your change lands, watch CI on `main`; fix what you broke.
- App packaging is affected-only; full image builds stay explicit or scheduled.
