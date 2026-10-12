# Start an app and reload it

```bash
tilo-app new reading-list --id org.example.reading-list
cd reading-list
tilo-app dev --timings
```

Replace the example id with a namespace you control before publishing.
Without an id, the generated `local.dev.*` id works for local development.
`tilo-app new --list-templates` describes the four starting shapes; choose
`--template document`, `collection` or `graph` when it fits your app.
`tilo-app examples` lists complete apps you can copy with `--from-example`.

Edit `src/lib.rs`: its model is the durable resource, its `#[action]` methods
are edits, and its view binds shared controls to those methods. Put visible
copy in `locales/en-US.ftl`. Keep sample data in capture scenarios, so a real
app starts empty.

Normal native dev builds reload in place and keep the open document; a failed
rebuild keeps the working app. Other Cargo profiles may restart it. `--timings`
reports where a reload spent time. `tilo-app dev --target web` reloads the
browser build into the same document.

On a foreign desktop, launching an app's default view again asks its running
window to come forward through the runtime's `activate-window` action. The
existing process and document owner stay in place. The window manager decides
whether to focus the window or request attention. After the owner exits, the
next launch starts normally; a leftover launch-lock file is not a live owner.

An unready or older running app returns a bounded activation diagnostic rather
than starting a competing document writer. Update its SDK or use the existing
window. App-specific command arguments still go through the app's parser;
this default-view activation path does not discard or reinterpret them.

The generated tests cover action dispatch, undo, persistence and headless
interaction; the starter also tests peer edits and per-author undo. Transport,
workspace membership and sign-in still need host integration tests.

Next: `tilo-app guide actions` or `tilo-app guide verify`.
More: [Getting started](../getting-started.md).
