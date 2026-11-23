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
tilolang new my-app --from-example qr-code
```

Run and verify it in the Tilo checkout:

```bash
tilolang run examples/apps/qr-code
tilolang check examples/apps/qr-code
```

`tilolang examples` lists every example.
