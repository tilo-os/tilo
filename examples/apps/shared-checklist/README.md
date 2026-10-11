# Shared Checklist

An example of **collaboration and undo**.

What collaboration means for an app's state, shown by tests that fork a
second replica, edit both concurrently and sync them. Text fields merge
character by character, booleans settle on one value everywhere, collection
items keep stable ids so concurrent adds keep both, each edit and verb is an
attributed version, and undo takes back only your own. The next item's title
is a form: local to the window until it is added as one invocation. The app
contains no networking, locking or undo code.

Read:

- `tests/app.rs`: the lesson; each test is one merge rule.
- `src/lib.rs`: an ordinary resource; nothing in it is collaboration-specific.

Start your own app from it:

```bash
tilo-app new my-app --from-example shared-checklist
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/shared-checklist
tilo-app check examples/apps/shared-checklist
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
