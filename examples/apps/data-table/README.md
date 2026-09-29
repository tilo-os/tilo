# Data Table

An example of **tables**.

A `DataTable` built from column definitions over a sparse `TableModel`. Text
columns format a row into a string that is also the cell's accessible value;
a widget column builds a live button per visible cell, bound to the
resource through `cx.actions()` because cells are built long after the view
function returned.

Read:

- `src/lib.rs`: the columns, the model, and the resource that records which rows were opened.
- `tests/app.rs`: selection by stable row id, cell values, touch-sized rows, undoable cell actions.

The work runs through `ui::background`, on a thread where the target has one
and on the page's executor on the web, so the example runs everywhere.

Start your own app from it:

```bash
tilo-app new my-app --from-example data-table
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/data-table
tilo-app check examples/apps/data-table
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
