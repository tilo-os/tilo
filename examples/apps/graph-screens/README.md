# Graph Screens

An example of **serialized screens**.

Presentation as data: the top of the window is a serialized UI graph under
`ui/`, validated at startup and editable while the app runs with
`tilolang ui edit`. A component node mounts a widget built by Rust
(`src/components/`), and every screen `tilolang add screen` registered is
hosted by the router. Durable state still goes through the resource and its
actions, bound by the native controls below the screens.

Read:

- `ui/main.json`, `ui/about.json`: the screens.
- `src/components/project_summary.rs`: a component the graph mounts by name.
- `tests/app.rs`: the component renders, every screen builds, the About link navigates through a published action, graph edits have their own undo.

Start your own app from it:

```bash
tilolang new my-app --from-example graph-screens
```

Run and verify it in the Tilo checkout:

```bash
tilolang run examples/apps/graph-screens
tilolang check examples/apps/graph-screens
```

`tilolang examples` lists every example.
