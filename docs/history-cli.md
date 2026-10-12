# Native history from a terminal

`tilo-control history` is a stateless JSON client of the desktop's published
resource actions, not Git's repository protocol. It does not replace
`/usr/bin/git`, inspect `.git`, maintain a revision cache, or create another
history store. Existing `tilo-control` commands are unchanged.

Run from the device admin's own unconfined login or SSH session while the desktop
is signed in. The default is its private admin socket; `--socket PATH` and
`TILO_TEST_CONTROL` retain their usual meanings. Socket access is not a grant to
edit: normal action admission and resource permissions still apply.

## Exact targets

Every operation requires either all four `--application APP --room ROOM
--record RECORD --resource RESOURCE` flags or `--provider PROVIDER --id ID`
exactly as returned by `tilo-control actions list --programmatic --json`.
The latter must name the action for that operation, not its sibling action.
Titles, ordering and query scores never select an application. Opaque IDs are
never constructed. Multiple matches, absent matches and incomplete listings
fail without invoking anything.

For example (replace the record and resource with published identities):

```sh
tilo-control history log --application so.tilo.notes --room personal \
  --record my-record --resource root --args-json '{"last":50}'
tilo-control history read --provider app-actions --id "$READ_ID" \
  --args-json '{"version":"saved","item":"published-item"}'
tilo-control history diff --provider app-actions --id "$DIFF_ID" \
  --args-json '{"from":"saved"}'
tilo-control history review --provider app-actions --id "$REVIEW_ID" \
  --args-json '{"version":"saved","paths":["/title"]}' > review.json
tilo-control history restore --provider app-actions --id "$RESTORE_REVIEWED_ID" \
  --review-file review.json
tilo-control history checkpoint --provider app-actions --id "$CHECKPOINT_ID" \
  --args-json '{"name":"before editing"}'
tilo-control history status --invocation-id 42
```

`log` publishes `history`; `review` invokes `review-restore`; `restore` invokes
**only** `restore-reviewed`, never the older unchecked `restore`. `read`, `diff`
and `checkpoint` invoke their corresponding published actions. Argument objects
are strict: log accepts optional `last` (1–500); read optional `item`/`version`;
diff requires `from`, accepts optional `to`; review requires `version` and a
nonempty `paths` array; checkpoint requires `name`. Text must be nonempty.
Restore accepts only a review file, not an arbitrary argument object.

Successful operations output one JSON object containing `target`, `operation`,
`result` and the unmodified wire `response`. Preserve the **entire** successful
review output as the review file (at most 1 MiB). Restore verifies its operation,
success status, result, four-part target and native review shape before invoking.
It forwards full target/live causal hashes and exact paths unchanged. The file
is a write precondition, **not** a permission token or signed provenance; native
admission and causal validation are authoritative. Checkpoint names are resolved
by review once; restore never re-resolves them. Any later edit requires a new
review. Pending-edit and stale-review errors are retained, not retried as writes.

Receipts are followed via `actions_status` for up to 20 seconds. Exit status is
0 for succeeded, 1 for refusal/failed/cancelled, 2 for usage/connection problems,
and 3 for a still-pending job. Pending output retains `broker_invocation_id` for
`history status`; failed/cancelled receipts and error details remain JSON.
Protocol refusals are output as the original JSON error response. No destructive
"clear history" operation exists: status reports the job's actual state.

## Compatible protocol and limitations

The desktop must provide full `actions_enumerate` rows with authenticated
`application`, `room`, `record`, `resource`, `operation`, `provider` and `id`,
complete/paginated listings (including programmatic actions), `actions_invoke`,
and terminal `actions_status` receipts. Only the native `app-actions` provider
is supported. Older desktops lacking hierarchy, checked restore or receipts fail
closed; the client never falls back to title matching or unchecked restoration.
Resource-specific published implementations remain authoritative, including
historical read size bounds. This is a client, not an export/import or Git bridge.

## Guest shells

There is deliberately no `ush` guest shim. The existing launcher custody preamble
in `crates/runtime/ush/src/host.rs::read_caller` identifies the app connecting to
the runtime; it does not admit a guest's history invocation into the desktop action
broker with collaborator identity, room/resource permissions and cancellation
custody. `crates/runtime/ush/src/broker.rs::Request` enumerates guest package,
permission, update, status and legacy-launch requests, not native resource action
invocations. Guest command shims and relay operations do not supply that action-door
custody. The existing explicitly admitted device-owner Host session can run this
ordinary executable already, without changes; it is not guest authority.

The unmet prerequisite is a narrowly typed, launcher-authenticated action service
handoff carrying the guest's admitted caller/actor and exact resource target
through the broker's normal admission, receipt and cancellation lifecycle. Until
that exists, no owner admin socket is bound or proxied into a guest, no generic
host executable/control escape is added, and no sandbox boundary is weakened.
