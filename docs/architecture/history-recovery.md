# History UI and native recovery

Native resource History uses the same registered `review-restore` and
`restore-reviewed` actions as automation. `DocumentHistory::native_history_actions`
is an optional toolkit capability declaring those addresses, without a dependency
from `tilo-ui` to `tilo-app`. The mounted component forwards the declaration from
its resource root. A declaration is not an admission or an authority grant.

The History host checks registration, exact owner lifetime, restore readiness and
the selected preview. It resolves the actions only in that captured component's
published tree and invokes the ordinary bound action door with the runner's real
transport. That door rechecks current grants and owns attribution, undo,
replication and record persistence. Confirmation does not use the old schema
restore engine for native resources and does not manufacture another undo span.

The whole-version UI enumerates differing content paths with the existing native
diff implementation. This is an explicit exact-path selection, not an empty
selection meaning “everything”. The native action validates every path and
preserves document bookkeeping. Selective callers use the same actions with
explicit paths; the current UI presents whole-version recovery, not a path picker.

The dialog retains both its original UI selection witness and the native review
object returned by the admitted read action. Confirmation copies that native
object unchanged. Selection changes, pending edits, stale live heads (including
unrelated changes), revoked grants and retired owners refuse writes. An explicit
refresh creates a new review; automatic presentation refresh never replaces a
pinned review behind the confirmation button.

Standalone, legacy and schema-owned documents without the capability retain
the existing restore path. Checked-file registrations use their checked-file
owner protocol and never opt into native resource recovery. Native errors do
not fall back to a less restrictive engine. The synchronous seam rejects
background/streaming declarations before invoking them.

## Outside consequences are not restored

`HistoryEdit::RestoreReviewed` applies the existing exact-path CRDT restoration
operation. Unlike native undo/redo, it does not decode verified compensation
verbs or enqueue outbox consequences. `mark_document_jobs` therefore does not
mark `restore-reviewed` as a compensation job; it remains an immediate document
action. This integration proves shared document recovery, not external-effect
parity (for example re-uploading a file or reversing a sent message). Adding
such semantics requires an explicit recovery contract and production outbox
receipt tests, not merely adding this action's name to the job predicate.

Production coverage lives in `tilo-app/tests/framework.rs` (UI action recording,
pinned stale review, explicit refresh, revoked permission, persisted recovery
and undo) and `tilo-app/tests/history_review.rs` (selective paths, peer work,
attribution and causal validation). `tilo-ui` History tests cover the fallback
owners and checked-file lifecycle.
