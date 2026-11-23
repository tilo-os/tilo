# Tree Browser

An example of **navigation and master-detail**.

A `TreeView` picks a section and a detail pane edits it. `NavShell` shows
both panes in a wide window and one at a time, with an accessible Back, in a
compact one. The selection is view state (`cx.local`), never stored or
synced; `cx.switch` rebuilds the detail pane with its own bindings when it
changes.

Read:

- `src/lib.rs`: fixed navigation, per-section resource state that starts empty, `cx.switch`.
- `tests/app.rs`: the pane follows the selection, sections keep their own fields, compact navigation works.

Start your own app from it:

```bash
tilolang new my-app --from-example tree-browser
```

Run and verify it in the Tilo checkout:

```bash
tilolang run examples/apps/tree-browser
tilolang check examples/apps/tree-browser
```

`tilolang examples` lists every example.
