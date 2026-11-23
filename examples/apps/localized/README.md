# Localized

An example of **localization**.

Every word the user reads comes from a Fluent catalog, with arguments and
plural forms decided per language (`locales/en-US.ftl`, `locales/de-DE.ftl`).
Views bind a message id and its arguments rather than a formatted string, so
a language change re-renders without a rebuild. The pseudo-locale test finds
any text that bypassed the catalog.

Read:

- `locales/`: the catalogs; `tilolang add locale de-DE` created the German one.
- `tests/app.rs`: plurals per language, live switching, and the pseudo-locale check.

Start your own app from it:

```bash
tilolang new my-app --from-example localized
```

Run and verify it in the Tilo checkout:

```bash
tilolang run examples/apps/localized
tilolang check examples/apps/localized
```

`tilolang examples` lists every example.
