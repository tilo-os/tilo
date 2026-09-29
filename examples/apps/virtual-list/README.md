# Virtual List

An example of **lists and virtualization**.

A `VirtualList` over a `ListModel` that reports a hundred million rows and
loads only the window in view, on a background thread, discarding answers
for windows the user scrolled past. Rows have stable keys and measured
variable heights, and each row's button is a control of its own. The rows
are not resource state; what the user did with them is.

Read:

- `src/lib.rs`: the sparse model (`request_range`, `poll_changes`) and the row delegate.
- `tests/app.rs`: only the viewport is mounted, keys are stable, heights vary, row buttons invoke actions.

The work runs through `ui::background`, on a thread where the target has one
and on the page's executor on the web, so the example runs everywhere.

Start your own app from it:

```bash
tilo-app new my-app --from-example virtual-list
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/virtual-list
tilo-app check examples/apps/virtual-list
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
