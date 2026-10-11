# Build your first Tilo app

A Tilo app is a Rust crate built on the `tilo-app` SDK. The app declares what
it is about, its resources, and the actions that change them, and draws a view
bound to those actions. Tilo supplies the rest: durable storage, sync and
collaboration, history and per-author undo, permissions, search, the command
palette, and the door that scripts, assistive tools and agents use to drive the
app without the screen.

One command-line tool, `tilo-app`, takes an app from an empty directory to a
signed package in the store. This page walks through that path; the
[complete guide](../app-development.md) covers each step in depth.

Tilo is in alpha. The SDK and the tool change often, and an app builds against
the exact SDK revision its `Cargo.lock` pins, so an app keeps building while
the SDK moves on.

## Install

You need Rust, installed with [rustup](https://rustup.rs). Then install
`tilo-app`:

```bash
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli
```

Run `tilo-app --help` to see every command. Each one accepts `--json` and then
writes exactly one JSON document to stdout, so scripts and agents can drive the
tool as well as people can.

For one task at a time, run `tilo-app guide`: `start`, `actions`, `collaborate`,
`permissions`, `network`, `verify` and `publish` are short workflows embedded in the CLI, available offline and
outside an app directory. `tilo-app guide actions --json` returns only that
guide, without searching or reading the full manual.

## Create an app

```bash
tilo-app new reading-list --id org.example.reading-list
cd reading-list
```

The id is the app's permanent name. Choose one in a namespace you control,
such as a domain you own written backwards, because publishing checks that you
own it. Without `--id` the app is named `local.dev.reading-list`, which works
locally but cannot be published.

`tilo-app new` with no arguments asks for the name, title, id and starting
template instead. `--template` picks one directly: `starter` (the default,
the smallest correct app), `document`, `collection` or `graph`;
`tilo-app new --list-templates` describes them.

To learn one idea at a time, start from an example instead: a complete,
tested program such as `counter`, `shared-checklist`, `data-table` or
`localized`. `tilo-app examples` lists them, and
`tilo-app new my-list --from-example shared-checklist` copies one and renames
it to your app. To inspect its collaboration and undo tests without a checkout,
run `tilo-app examples shared-checklist --file tests/app.rs --json`.
[Templates and examples](../examples.md) describes each.

The new directory is a complete Cargo project with its own git repository:

- `AGENTS.md` is a compact guide for coding agents: where to start, how to
  add a resource action, and which commands verify the result. It travels
  with the app, including apps created from an example.
- `src/` holds the app: a `#[derive(Model)]` resource, its `#[action]`
  methods, and a view whose controls invoke those actions.
- `tilo-app.toml` is the package manifest: id, title, entrypoints and the
  sandbox declaration.
- `tests/app.rs` tests the actions without a window and drives the running app
  headlessly.
- `rust-toolchain.toml`, `Cargo.lock` and `.cargo/config.toml` pin the
  compiler, dependency versions and build flags Tilo itself is tested with.

## Run it while you edit

```bash
tilo-app dev
```

`dev` builds the app, opens it and watches the source. When a save builds, the
new version replaces the running one, and the open document, the current
screen and the caret carry over. A build that fails leaves the last good
version running until the next save fixes it. `tilo-app new reading-list
--dev` creates the app and starts this loop in one step.

The same app root answers the other everyday commands:

```bash
tilo-app run      # build once and open it
tilo-app test     # the Rust tests, plus the accessibility and action audit
tilo-app check    # the full quality gate: structure, format, Clippy, tests, scenarios
tilo-app shot     # a screenshot of the live widget tree, no display needed
```

## Make every change an action

An app changes its state only through actions: small, serializable operations
such as **Add book** or **Rename list**. A button, a menu item, a keyboard
shortcut and a row in the command palette that do the same thing all invoke
the same action, which is why undo, collaboration, scripting and agents work
without extra code. `tilo-app test` fails on a control that is not bound to an
action.

[Resources and actions](../action-model-recipe.md) explains the model, and
[The app shape](../app-development.md#the-app-shape) shows it in code. The
[API reference](../../crates/sdk/tilo-app) documents every type.

## Run it on other platforms

The same source runs on Tilo OS, other Linux desktops, Windows, macOS and the
web. `build`, `run`, `dev`, `check` and `package` take `--platform` and
`--arch`:

```bash
tilo-app dev --platform web          # serve it on http://127.0.0.1:4321/, reload on save
tilo-app run --platform linux        # a self-contained build for this Linux desktop
tilo-app check --platform all        # lint every other target too
```

Build on the system you target, or with a cross toolchain Cargo can drive.
See [Other platforms and architectures](../app-development.md#other-platforms-and-architectures),
[The web](../app-development.md#the-web) and [Android](../android.md).

## Package it

```bash
tilo-app package
```

`package` checks the app, builds it, and writes a signed, verified package to
`dist/reading-list.tapp`. On Tilo OS an app links the toolkit the OS ships, so
a typical package is well under a megabyte. Build one package per platform you
ship:

```bash
tilo-app package --arch aarch64                      # Tilo OS on ARM
tilo-app package --platform linux --arch x86_64      # other Linux desktops
tilo-app package --platform macos --arch aarch64     # macOS
tilo-app package --platform windows --arch x86_64    # Windows
tilo-app package --platform web                      # the browser
```

[Build an app](../app-development.md#other-platforms-and-architectures) covers
size budgets, payload files, signing identities and what each platform's
package contains.

## Build it in CI

```bash
tilo-app add ci --forge github     # or: --forge forgejo, --forge gitea
```

This writes a workflow that runs `tilo-app check` on every push and pull
request, packages each platform, and publishes when you push a tag named
`v*`. Add one secret, `TILO_PUBLISHER_KEY`, holding your publisher key; builds
without it, such as pull requests from forks, are never published.
[From CI](../publishing.md#from-ci) lists what each forge's workflow builds.
On GitHub the generated entry workflow calls `tilo-build.yml`, which other
repositories can [reuse](../publishing.md#reuse-a-github-build-workflow)
through GitHub's built-in workflow imports.

## Publish it

```bash
tilo-app keys new                                              # once
tilo-app apply --domain example.com --contact you@example.com  # once
tilo-app publish
```

`keys new` creates your publisher key. `apply` asks the registry for the
namespace your app ids live in and tells you where to put the proof that you
own it: a DNS record, a file on your web site, or a repository on your git
forge. `publish` uploads everything in `dist/` and requests the store entry.
Nothing is public until the registry's owner approves it.
[Publish a Tilo app](../publishing.md) walks through each step.

## Where to go next

- [Templates and examples](../examples.md): what each template starts you
  with and what each example teaches.
- [A resource workflow](../sdk-resource-journey.md): a small checklist showing
  validation, identity, permission revocation, confirmation, undo and persistence
  through shared UI controls and the agent action surface.
- [Build an app](../app-development.md): templates, screens, localization,
  text, large collections, testing and every `tilo-app` command.
- [Interface guidelines](../../crates/sdk/ui-core/HIG.md): layout, navigation
  and the shared controls every app builds on.
- [Scripts and agents](../agent-action-api.md): how programs discover and
  invoke an app's actions.
- [API reference](../../crates/sdk/tilo-app): the `tilo-app` crate.
