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

## Verify an installed resource

Use the authorized session's `tilo-control` socket. Run `actions list` for
permission-filtered descriptors, then invoke the returned provider and id.
Keep the caller, workspace and record fixed.

- **Completion:** `accepted` only means admitted. Save `broker_invocation_id`;
  follow it with `tilo-control actions status ID --wait`. If unavailable,
  outcome is unknown; do not replay a mutation to replace a lost reply.
- **Result:** observe the changed resource through its action surface and
  production view. Wait boundedly for cold publication; the first empty
  listing is inconclusive. Use the returned resource id, not its title.
- **Missing actions:** if an opted-in `native_admission` checkpoint exists,
  inspect `manager_connected`, `custody_reconciled`, `action_history_ready`,
  `directory_enqueued` and directory RPC counters to distinguish connection,
  applied history and publication. A painted window proves no action readiness.
  A missing snapshot is inconclusive; ordinary dev slots need not serve the
  device-owner socket. Use existing authorized diagnostics and retain
  publication checks. These observations do not prove durable-owner ACK.
- **Reopen:** close the view; restart the owned session if under test; reopen
  normally with the same caller and workspace. Verify the original record and
  edit without replay. A closed-tab undo stack is no reopen API.
- **Durability and sync:** saved bytes and a repainted view prove only those
  facts. Observe required owner ACK or remote delivery separately with the
  real authorized owner or peer. In-memory replica tests do not prove transport.

More: [Build an app](../../app-development.md),
[Headless testing](../../headless-testing.md).
