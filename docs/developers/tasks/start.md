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

The generated tests cover action dispatch, undo, persistence and headless
interaction; the starter also tests peer edits and per-author undo. Transport,
workspace membership and sign-in still need host integration tests.

Next: `tilo-app guide actions` or `tilo-app guide verify`.
More: [Getting started](../getting-started.md).
