# Cross-target: one source for Linux, macOS and the web

A Tilo app is one Rust crate that builds unchanged for Linux, macOS and the
web. The app holds no platform code: no target `cfg`, no `std::os`, no
platform paths, no OS tools. The SDK (`ui`, the `tilo-app` package) picks a
runner per target, and the platform crates under `crates/platform/` pick each
service's backend. This page is the contract; the commands are in
[`../app-development.md`](../app-development.md#linux-macos-and-the-web-from-one-source).

## Build it

```bash
tilo-app check --target linux|macos|web   # portability, capabilities; cargo check where the host can
tilo-app run --target web                 # serve and reload on change
tilo-app build --target macos             # a signed development .app
tilo-app package --target linux           # a signed .tapp
```

`tilo-app.toml` lists the targets (`platforms`, default all three). An app
whose purpose is platform-specific (a terminal needs a PTY) narrows them with a
`platform-reason`; `tilo-app` refuses to build it for anything else, and
launchers show it as unavailable there.

## Services go through the SDK

| Need | Call | Linux | macOS | Web |
|---|---|---|---|---|
| App files | `cx.files()` / `ui::storage::AppFiles` | `$XDG_DATA_HOME/<id>`, … | `~/Library/Application Support/<id>`, … | origin-private file system |
| Preferences | `cx.settings()` / `ui::storage::Settings` | desktop `settings.json` section | same | the host's settings source |
| Secrets | `cx.secrets()` / `ui::storage::secrets` | Secret Service | Keychain | wrapped IndexedDB |
| Documents | resources and records (`ui::persist` for one-document apps) | Automerge, synced by the runtime | same | same |
| Files the user picks | `ui::portal::PickRequest` | portal / GTK | open and save panels | file pickers (by contents) |
| Network | `tilo_net` (`Client`, `status`, `ws`) | reqwest | reqwest | `fetch`, the host's fetch proxy |
| Time, threads | `ui::time`, `tilo_rt` / `ui::background` | std | std | the page's clock and executor |
| Notifications, location, camera, clipboard | `ui::notify`, `ui::location`, `tilo_capture`, `ui::clipboard` | each platform crate's backend | | |
| The workspace mount, login and host name | `tilo_paths::workspace_root`, `tilo_paths::session` | | | none |
| Owner-only files (keys, host-key stores) | `tilo_files::native::private` | mode 0600/0700 | same | refused |
| This process's id and open-file limit, other processes' open files | `tilo_process` (`id`, `raise_open_file_limit`, `open_file_count`, `probe_process_table`) | std, `setrlimit`, `/proc` | std, `setrlimit`; no open-file count | `None`; no limit |

What a target cannot provide is an `Unsupported` error at run time and an entry
in its `Feature`'s targets, never a missing item at compile time. Handle
it: grey the control out and say why. Where no crate offers what you need, add
it to the platform crate with a backend per target instead of branching in the
app.

## Capabilities

`[capabilities]` in `tilo-app.toml` lists what the app uses, `required` or
`optional`:

```toml
[capabilities]
required = ["network.http"]
optional = ["files.open", "files.save", "app-files", "notifications"]
```

`tilo-app check` and `build` fail when the code uses a capability the manifest
does not list (the evidence table is `tools/policy/app_capability_usage.toml`:
dependencies, feature selections, SDK paths), and when a required one is
unavailable on a declared target. The fix-it names the line to add;
`tilo-app add capability <name>` adds it. macOS takes entitlements and usage
strings from this list, and a prompted service used without them ends the
process.

## What the gate checks

`tools/policy/check_app_portability.py` (and `tilo-app check --lint`) reject
platform code in an app that declares all three platforms (rules R1–R4 and the
web traps W1–W3 are errors with no allowance). Apps narrowed to fewer
platforms count their remaining sites against a shrink-only baseline.
`tools/policy/check_app_capabilities.py` runs the capability check over every
app in the same lane.

## Toolkit knobs

Debug, capture and benchmark switches are environment variables named
`TILO_UI_*` (`TILO_UI_DEBUG`, `TILO_UI_PRESENT`, …), read through
`ui::env`. [`../performance-flags.md`](../performance-flags.md) lists the
rendering ones.

## Packaging

The same crate produces a `.tapp` (Linux), a `.app` zip (macOS) and an
immutable web version (`dist/web/<app-id>/versions/<digest>/`); only the
packaging metadata differs. See [`hello-world.md`](./hello-world.md) for the
full recipe from `tilo-app new` to a launched app.
