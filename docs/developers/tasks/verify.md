# Diagnose and verify an app

From anywhere inside the app:

```bash
tilo-app doctor --json
tilo-app test --json
tilo-app inspect --audit --output target/inspect.json --audit-report target/audit.json --json
tilo-app check --artifacts target/check --json
```

`doctor` reports manifest, resource/lifecycle wiring, serialized screens,
translations and scenario problems together. `test` runs Rust tests;
`check` also runs strict lints and saved accessibility-audited scenarios.
Fix a reported failure instead of treating an unavailable check as a pass.

`inspect` writes the actual post-layout widget and accessibility tree without
opening a window. Inspect action bindings, accessible names and bounds there.
Use semantic resource actions for behaviour tests; use pointer events when
the subject is hit-testing or geometry.

Capture the production view in more than one theme and window size:

```bash
TILO_SHOTS_THEME=light tilo-app shot --output target/light.png --viewport 480x900
TILO_SHOTS_THEME=high-contrast tilo-app shot --output target/contrast.png --viewport 1200x900
```

Pass `--scenario NAME` only for a scenario your app declares. Put illustrative
data in that capture fixture, not the live app. Check that artwork is decoded,
controls fit, long copy wraps and the primary action remains reachable.

`--json` returns one result document; progress goes to stderr. It does not
bypass permission checks or confirmation. Headless peer tests prove resource
merging; real device transport and authenticated membership need separate
integration journeys.

## Verify an installed resource workflow

Use the authorized session's `tilo-control` socket. Discover the current
permission-filtered descriptors with `actions list`, then invoke the returned
provider and id with the declared parameters. Keep the same caller, workspace
and record throughout the test.

- **Completion:** `accepted` means admitted. Retain `broker_invocation_id`
  and follow it with `tilo-control actions status ID --wait`. If status becomes
  unavailable, record the outcome as unknown; do not repeat a mutation just
  because its reply was lost.
- **Result:** observe the changed resource through its current action surface
  and production view. Allow a bounded wait for cold resource publication;
  an empty first listing does not establish that the record is empty. Resolve
  the returned resource identity rather than guessing one from a title.
- **Missing actions:** when an opted-in native startup checkpoint is available,
  inspect `native_admission`: `manager_connected`, `custody_reconciled`,
  `action_history_ready`, `directory_enqueued` and the directory RPC counters
  distinguish connection, applied history and publication. A painted window
  does not prove action readiness. A missing snapshot is inconclusive; ordinary
  development slots need not serve the device-owner diagnostic socket. Use
  the session's existing authorized diagnostics and retain its publication
  checks. These observations do not establish durable-owner ACK.
- **Reopening:** close the view, restart the owned test session if that is the
  subject, and open the application normally with the original caller and
  workspace. Verify the original record and changed content without replaying
  the edit. A closed-tab undo stack is not a restart/reopen API.
- **Durability and sync:** saved local bytes and a repainted view prove those
  observations. Where the contract requires an owner acknowledgement or
  remote delivery, observe that boundary separately using the real authorized
  owner or peer. In-memory replica tests do not prove device transport.

More: [Build an app](../../app-development.md),
[Headless testing](../../headless-testing.md).
