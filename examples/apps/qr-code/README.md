# QR Code

An example of **generated visuals**.

A QR code drawn from resource text. `cx.value` follows the payload into view
state and `ModelView` rebuilds only the code when it changes. The image
carries alternative text naming what it encodes, and until there is a payload
the app shows an empty state saying what to do.

Read:

- `src/lib.rs`: the resource, the followed value, the generated widget with its alt text.
- `tests/app.rs`: starts empty, describes the code, clearing is undoable.

Start your own app from it:

```bash
tilo-app new my-app --from-example qr-code
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/qr-code
tilo-app check examples/apps/qr-code
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
