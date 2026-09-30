# Background Task

An example of **async work**.

Slow work on a worker thread, with a spinner while it runs and the result
when it finishes. The input is resource state; the progress is view state in
a `LatestTask`, whose completions go stale when the task restarts or is
cancelled, so a slow answer never overwrites a newer one. Run and Cancel are
effect actions, so anyone who can see the app's actions can start and stop
the work.

Read:

- `src/lib.rs`: the computation as plain testable functions, the task resource, the worker loop.
- `tests/app.rs`: the work itself, a completed run, a cancelled run, the durable input.

The work runs through `ui::background`, on a thread where the target has one
and on the page's executor on the web, so the example runs everywhere.

Start your own app from it:

```bash
tilo-app new my-app --from-example background-task
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/background-task
tilo-app check examples/apps/background-task
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
