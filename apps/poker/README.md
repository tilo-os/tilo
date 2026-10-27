# Poker

Texas hold'em for everyone in the room: six seats, blinds of 5 and 10, side
pots and a showdown. Opening the table seats nobody: you watch until you
take a seat. Every move is an action on the table, so a person, an agent and
a script play by the same rules.

Play with other people, each in their own browser window:

```bash
tilo-app dev --platform web --share
```

Open the join page it prints once per player.

The commands below are the `tilo-app` CLI. It needs rustup and no system packages:

```bash
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli
```

The app is written on the Tilo app framework: its state is a `#[derive(Model)]` resource, every edit is an `#[action]` method, and the view binds controls to those actions (`cx.input`, `cx.checkbox`, `cx.button`, `cx.control`, `cx.each`). Storage, sync, collaboration, history, per-author undo, permissions, the action palette, agents, scripts and search come from the framework; `Replica` tests the resource and `TestApp` tests the running app. `tilo-app doctor` rejects apps that remove this surface.

Agents, scripts, and search invoke the same address as the visible control. Discover the live catalogue, then submit an invocation shaped like:

```json
{"resource":"so.tilo.poker.table","action":"raise","arguments":{"to":40}}
```

Develop: run the app, rebuilding and reloading it as you edit:

```bash
tilo-app dev
tilo-app run
tilo-app test
```

Verify: `check` runs lints, structure, formatting, Clippy, tests, certification and audited scenarios:

```bash
tilo-app check --fix
tilo-app scenario run
tilo-app scenario run --artifacts target/scenario-artifacts
tilo-app scenario run --audit --artifacts target/scenario-artifacts
tilo-app shot
tilo-app inspect --actions tests/headless-actions.json
tilo-app inspect --audit --audit-report target/audit/report.json --actions tests/headless-actions.json
```

Grow: add screens, components, scenarios and translations:

```bash
tilo-app add component profile-card --template form
tilo-app add component project-tree --template tree
tilo-app add component activity-list --template list
tilo-app add component results-table --template table
tilo-app add component article-body --template text
tilo-app add screen settings --template settings
tilo-app add scenario empty-state
tilo-app add locale de-DE
tilo-app locale sync
```

Ship: build, sign and verify a package for each platform (`--platform tilo|linux|windows|macos|web`, `--arch x86_64|aarch64`; `build`, `run`, `dev` and `check` take the same flags):

```bash
tilo-app package
tilo-app package --platform web
tilo-app check --platform all
```

Publish: create a publisher key, prove a namespace, and upload what `dist/` holds:

```bash
tilo-app keys new
tilo-app apply --domain example.com
tilo-app publish
tilo-app add ci --forge github
```

Every command accepts `--json` and then writes one JSON document to stdout, for scripts and agents.

The same source runs on Linux, macOS and the web; `--target` picks the platform (each builds on its own host OS, the web on any):

```bash
tilo-app run --target linux
tilo-app run --target macos
tilo-app run --target web
tilo-app build --target macos
tilo-app package --target linux
tilo-app package --target macos
tilo-app package --target web
```

Cargo remains available directly with `cargo test` and `cargo run`.

`tilo-app add screen account-settings` creates `ui/account-settings.json` and a callable `screens::account_settings(&theme)` constructor.

User-facing strings start in `locales/en-US.ftl`. `tilo-app add locale de-DE` creates and registers a complete editable copy of that message contract. After changing the source, `tilo-app locale sync` adds missing units without replacing translations; `tilo-app doctor` reports differences deliberately left for a translator while the toolkit falls back to the source catalog.

App id: `so.tilo.poker`
