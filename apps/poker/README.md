# Poker

Texas hold'em for everyone in the workspace: six seats, blinds of 5 and 10, side
pots and a showdown. Opening the table seats nobody: you watch until you
take a seat. Every move is an action on the table, so a person, an agent and
a script play by the same rules.

The commands below are the `tilolang` command. It needs rustup and no system packages:

```bash
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilolang-cli
```

The app is written on the Tilo app framework: its state is a `#[derive(Model)]` resource, every edit is an `#[action]` method, and the view binds controls to those actions (`cx.input`, `cx.checkbox`, `cx.button`, `cx.control`, `cx.each`). Storage, sync, collaboration, history, per-author undo, permissions, the action palette, agents, scripts and search come from the framework; `Replica` tests the resource and `TestApp` tests the running app. `tilolang doctor` rejects apps that remove this surface.

Agents, scripts, and search invoke the same address as the visible control. Discover the live catalogue, then submit an invocation shaped like:

```json
{"resource":"so.tilo.poker.table","action":"raise","arguments":{"to":40}}
```

Develop: run the app, rebuilding and reloading it as you edit:

```bash
tilolang dev
tilolang run
tilolang test
```

Verify: `check` runs lints, structure, formatting, Clippy, tests, certification and audited scenarios:

```bash
tilolang check --fix
tilolang scenario run
tilolang scenario run --artifacts target/scenario-artifacts
tilolang scenario run --audit --artifacts target/scenario-artifacts
tilolang shot
tilolang inspect --actions tests/headless-actions.json
tilolang inspect --audit --audit-report target/audit/report.json --actions tests/headless-actions.json
```

Grow: add screens, components, scenarios and translations:

```bash
tilolang add component profile-card --template form
tilolang add component project-tree --template tree
tilolang add component activity-list --template list
tilolang add component results-table --template table
tilolang add component article-body --template text
tilolang add screen settings --template settings
tilolang add scenario empty-state
tilolang add locale de-DE
tilolang locale sync
```

Ship: build, sign and verify a package for each platform (`--platform tilo|linux|windows|macos`, `--arch x86_64|aarch64`; `build`, `run`, `dev` and `check` take the same flags):

```bash
tilolang package
tilolang check --platform all
```

Publish: create a publisher key, prove a namespace, and upload what `dist/` holds:

```bash
tilolang keys new
tilolang apply --domain example.com
tilolang publish
tilolang add ci --forge github
```

Every command accepts `--json` and then writes one JSON document to stdout, for scripts and agents.

The same source runs on Linux and macOS; `--target` picks the platform (each builds on its own host OS):

```bash
tilolang run --target linux
tilolang run --target macos
tilolang build --target macos
tilolang package --target linux
tilolang package --target macos
```

Cargo remains available directly with `cargo test` and `cargo run`.

`tilolang add screen account-settings` creates `ui/account-settings.json` and a callable `screens::account_settings(&theme)` constructor.

User-facing strings start in `locales/en-US.ftl`. `tilolang add locale de-DE` registers the locale: it writes a complete editable copy of that message contract, or keeps the catalog already at `locales/de-DE.ftl` and registers it without touching its text. After changing the source, `tilolang locale sync` adds missing units without replacing translations; `tilolang doctor` reports differences deliberately left for a translator while the toolkit falls back to the source catalog.

App id: `so.tilo.poker`
