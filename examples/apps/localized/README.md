# Localized

An example of **localization**.

Every word the user reads comes from a Fluent catalog, with arguments and
plural forms decided per language (`locales/en-US.ftl`, `locales/de-DE.ftl`).
Views bind a message id and its arguments rather than a formatted string, so
a language change re-renders without a rebuild. The pseudo-locale test finds
any text that bypassed the catalog.

Read:

- `locales/`: the catalogs; `tilo-app add locale de-DE` created the German one.
- `tests/app.rs`: plurals per language, live switching, and the pseudo-locale check.

Start your own app from it:

```bash
tilo-app new my-app --from-example localized
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/localized
tilo-app check examples/apps/localized
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
