# Universal version control: native reads and coverage audit

**Status: partial native coverage.** This describes the current native
framework slice, not a claim that every resource has complete version control.
The goal is one history door for people, accessibility, scripts and agents:
inspect old content, compare exact versions, then restore selected content as
new, attributed, undoable work. History is not rewritten.

See [history-experience.md](history-experience.md) for the workspace experience
and [search-and-history-views.md](search-and-history-views.md) for scope rules.
Those product descriptions are not evidence that every route is implemented.

## Native action workflow

On a readable `ResourceApp` resource, discover the registered actions and use
its existing action door (for example through `tilo-control actions`). The
following are action arguments, not a new CLI or a separate history service:

1. `history` lists changes and checkpoints. Choose a version.
2. `read({version})` returns `{version, heads, content}`. `content` is the
   resource's typed read representation at that version, not an Automerge dump.
   `read({version, item})` reads a nested item using an id from the typed read.
   Without `version`, the existing current-content response remains unchanged.
3. `diff({from, to})` compares two versions; omit `to` for the committed live
   version. It returns differing `items`, display names `from`/`to`, and exact
   full-hash arrays `from_heads`/`to_heads`. Keep the complete arrays: concurrent
   heads are one frontier, not competing versions to choose arbitrarily.
4. `compare({from, to, items?})` returns frozen typed before/after content and
   the same full frontier identities. Omit `items` for the whole resource, or
   supply 1–32 distinct published resource IDs (at most 512 bytes each). Each
   side is `present` with `content`, or explicitly `absent`. Absence means no
   published item after decoding and demand-loading; unreadable schemas,
   demand-load failures and content-decoder errors fail instead. `READABLE=false`
   cannot disclose content. The **entire** response is bounded to 256 KiB, not
   each side separately. See [typed comparison](history-typed-comparison.md).
5. `review-restore({version, paths})` validates the selection without writing.
   Its response contains `items` and a `review` object with `schema: 1`,
   `target_heads`, `live_heads` and `paths`.
6. Copy that object unchanged into `restore-reviewed({review})`. The edit
   checks the live frontier again and appends a restore attributed to the
   invoking collaborator (including an agent acting for a person).

For example, after comparing an old checkpoint against now:

```json
{"version": "good", "paths": ["cards/card-id"]}
```

is an argument to `review-restore`; the returned `review`, not this request,
becomes the argument to `restore-reviewed`. A diff is structural selection
information, not a complete before/after content renderer: use `compare` or
typed reads to inspect content at the pinned versions. Consume the returned
immutable `from` identity as the review's `version`; restore paths still come
from `diff`, not comparison item IDs.

For terminal use, see [native history from a terminal](history-cli.md) for
`tilo-control history log/read/diff/review/restore/checkpoint/status`. Every
operation requires an exact published provider/action ID or the complete
application/room/record/resource target. The client forwards the successful
review output unchanged and invokes only `restore-reviewed`; ambiguity or an
incomplete listing fails closed. There is no `history compare` subcommand:
invoke the published `compare` action through `tilo-control actions invoke`
([action API](agent-action-api.md)). Neither interface is a Git bridge or a
sandbox escape.

### Exact selection and conservative staleness

- Paths are the **exact differing paths from `diff`**, not bare item ids.
  `cards/id` selects that map entry; a root such as `title` or `cards` selects
  its entire value, including descendants. This is not arbitrary deep JSON
  Pointer addressing or a wildcard selector.
- Selection must contain **1–256 paths**, each at most **1024 bytes**.
  Duplicates are harmless, but do not enlarge these input bounds.
- Empty, unknown, unchanged, malformed, ambiguous or bookkeeping paths fail.
  Legacy paths are unescaped: slash/tilde-containing components are refused
  rather than guessed. A replaced collection requires selecting its root,
  not an entry that could overwrite or lose unselected siblings.
- Every selection is validated before patch writes. Review also tries patch
  applicability on a disposable document clone.
- Any new committed change invalidates a review, even to an unrelated item or
  a checkpoint. This is intentionally conservative, not an item-level conflict
  detector. An A→B→A edit is still new causal history and is rejected (**ABA**).
- Pending edits must be finished before review or reviewed restore; these
  operations must not silently commit someone else's open transaction.

The review is stateless causal data, not a signed approval, authorization token
or server-local session. Current action admission and write permissions still
apply. A viewer can read/review but cannot restore. Matching frontiers bind the
write to causal state; they are not a new universal resource-identity scheme.

### Immutable version identity and disclosure

`steps::resolve` accepts checkpoint names and individual change identifiers.
Full 64-hex change hashes are resolved **before checkpoint names**, fixing the
case where a mutable checkpoint name shadows an immutable hash. A `+`-joined
frontier requires full hashes, at most 64 unique known heads, and no head that
is an ancestor of another. It also takes precedence over checkpoint lookup.
Historical `read` returns that full identity and every head, so its `version`
can be reused without losing concurrent work or re-resolving a mutable name.
Reviewed restoration pins full target heads once; it never resolves the
checkpoint name again when applying or replaying the edit.

Historical reads fork a frozen document and use the resource's existing typed
reader/content adapter. They do **not** expose raw/private document keys as a
fallback. The serialized historical response is limited to **256 KiB**; an
oversized response fails rather than returning misleading partial content.
Read a smaller typed item instead. Review responses use the same bound.
This does not prove every app's adapter redacts every sensitive field.

## Implementation and UI boundary

- `crates/sdk/tilo-app/src/framework/root.rs`: `read_spec`, `read`, typed frozen
  historical reads and action publication/dispatch.
- `crates/sdk/tilo-app/src/framework/history.rs`: history specs, full diff head
  arrays, `HistoryEdit::RestoreReviewed` and resource-registered compensation.
- `crates/sdk/tilo-app/src/framework/history/compare.rs`: bounded typed
  comparison, published result schema and frozen before/after sides.
- `crates/sdk/tilo-app/src/framework/history/review.rs`: `RestoreReview`,
  `answer`, `apply`, bounds and the two new action specs.
- `crates/sdk/ui-core/src/history_source/steps.rs`: `resolve`, `changed_items`,
  `restore_paths` and the existing forward patch/undo machinery.
- `crates/sdk/tilo-app/src/framework/preview.rs`: existing History mode builds
  the app's read-only view from a frozen historical resource reader.

**Compatibility:** existing `restore({version, ids?})` is preserved and remains
unchecked by the new review precondition. Its legacy id/whole-document behavior
is not the exact-path protocol above. Existing undo, redo and checkpoint remain.
The managed History UI's admitted recovery controls invoke native
`review-restore` and `restore-reviewed` through the owner's registered action
surface. `crates/sdk/tilo-app/tests/framework.rs` proves the UI uses admitted
native actions for selective recovery, retains unrelated work and rechecks
freshness and authority at confirmation.
`crates/sdk/ui-core/src/history_mode/integration_tests.rs` additionally proves
a failed native review cannot downgrade to legacy restore after capability loss.
Owners without the native surface retain the separate legacy checked-owner route. These tests do not certify every app, owner or
cross-owner restore path.

## Repository-grounded coverage audit

“Existing” below means an implementation is present, not all-app certification.
Standalone `tilo.history` is the UI-core action provider, distinct from the
`tilo-history` crate that renders history sentences.

| Capability | Native `ResourceApp` | Standalone `tilo.history` | Files | Workspace |
|---|---|---|---|---|
| History/provenance | Framework history actions | Existing session/provider | File version records/projection | Aggregates tree and resource records |
| Historical content | New bounded typed `read(version, item?)` | Existing session preview | Existing frontier/version preview reads | Entry/detail views; not a typed snapshot of every resource |
| Compare | Typed `compare` plus structural `diff`, full head arrays | Existing History model | Separate checked-file history model | Existing causal detail/replay views |
| Restore safety | Exact-path review + stale-frontier guard; admitted managed UI actions | Existing document-bound review/confirmation | Separate checked review/restore protocol | Existing undo/detail routes; no workspace-wide atomic restore |
| This slice's proof | Native action door, managed UI recovery and replay fixtures | Not newly certified here | Not newly certified here | Closed-record routing mock only; no native owner cold restore proof |

Exact references for the other columns:

- Standalone: `crates/sdk/ui-core/src/standalone_history.rs` (`StandaloneHistory`,
  `review_restore`, `apply_restore`), `standalone_history_host.rs` (provider),
  and `standalone_history_pane.rs` (surface), in the same directory.
- Files: `crates/system/fs/src/history.rs`, `history_read.rs`
  (`read_file_history`, `read_file_history_preview_at_frontier`), and
  `checked_restore.rs` (`checked_history_review` and checked restoration);
  wire types: `crates/system/fs-protocol/src/files/checked_history.rs` and
  `history_read.rs`. These are not the native `RestoreReview` wire format.
- Workspace: `crates/session/desktop-surface/src/activity/index.rs`,
  `view_actions.rs`, `replay.rs`, and `pane.rs`; shell integration and cold
  history fixtures: `crates/session/shell/src/shell/workspace_history.rs`.
  Existing cold aggregation is not proof of cold action-host restore routing.

## Evidence, assumptions and remaining work

`crates/sdk/tilo-app/tests/history_review.rs` exercises the production native
`TestApp` action door: frozen typed resource/item reads, private raw-key omission,
preview agreement, concurrent head preservation, selective restoration with peer
work preserved, agent attribution/undo, stale/ABA refusal without history writes,
checkpoint pinning, cross-document refusal, invalid selections and viewer denial.
`framework/history/review.rs` tests pending-edit refusal, bounded reads and
registered-action replay beside concurrent work, verification and duplicate
replay. `crates/sdk/ui-core/src/history_source/steps_tests.rs` covers exact-path
patch behavior and version resolution; `tests/history_actions.rs` retains the
existing action coverage. These are source-level test claims, not a test-run
report or an all-resource proof.

`crates/sdk/tilo-app/tests/history_compare.rs` additionally proves typed content,
added/deleted sides, concurrent frontier identity, readability refusal and the
combined-response bound. The Code Editor tests below couple that protocol to
production demanded Source text and selective recovery. The executable's
`crates/session/control-cli/tests/history_socket.rs` tests exact discovery,
review forwarding and receipt handling on the JSON-line socket protocol; its
scripted server is not a running desktop.

The closed-record fixture in
`crates/collab/tilo-action-broker/src/collab_broker_capnp/launchable.rs` proves
retained-catalogue session-host routing, exact record launch and result custody
across broker restart. Its fake app supplies history answers: this is **routing
mock proof only**, not native owner execution, document mutation or cold restore.
See [typed comparison evidence](history-typed-comparison.md#what-the-tests-prove).

Assumptions: retained target changes, readable current resource schema, committed
frontiers, normal action admission and the resource's own registered decoder.
No multi-resource atomic snapshots, branch/merge product workflow, all-types
schema-evolution proof or universal redaction proof is delivered here. Closed
resources and cold/remote native owners still need execution end-to-end tests
for this workflow; the routing fixture does not close that gap. Retained local
launch providers deny remote callers. Cross-owner atomic restoration and
remote granted history transport remain unproven; managed UI integration does
not expand those limits.

Git records **`d8bc16deea` — “feat: one resource/action/view model”** as
landed. Coverage must be audited against the current implementations and their
tests: a shared resource model alone does not prove historical reads or safe
restoration for every resource type.

## Code Editor source projects

The production `CodeEditorShell` Project now answers typed native reads. A whole
read inventories every recorded file/folder (stable id, nested resource address,
relative path, kind, language and text availability); it does not dump the Model
or include terminal state, command outboxes or machine-local disk paths. Item
reads accept the stable id or published file address and demand only that file's
text from the requested document. Generic `compare` uses the **published file
resource address**, not arbitrary read-adapter selectors such as `inventory:0`;
those pseudoitems are paging arguments to Project `read`, not published children.
Rename changes the historical path, not the id. A deleted item remains readable
at a retained earlier frontier.

Responses are capped at 240 KiB including JSON escaping (below the framework's
256 KiB historical envelope limit). `read({version, item: "inventory:0"})` reads
128 entries and returns `next` for the next page. Pin every page to the same
returned immutable `version`; unversioned pages can observe different commits.
A too-large whole inventory or single text fails explicitly, never truncates.
The inventory's `scope: "recorded-project"` and `complete` refer to the record,
not the source tree on disk. The existing importer lists directories lazily,
ignores configured build/VCS names and symlinks, and records text only after
opening it. A checkpoint is therefore not a whole-source filesystem snapshot.
Source text itself is disclosed as requested content; the adapter is not a
secret detector for credentials deliberately stored inside source files.

`apps/code-editor/tests/source_history.rs` exercises the actual ResourceApp:
whole-record checkpoints, frozen reads and typed comparison across rename/edit
and delete, consumption of comparison frontiers for selective reviewed recovery,
demanded text from the frozen document (not the live model), and explicit
unreadable-text refusal rather than false absence. It also proves reviewed
recovery of deleted metadata plus text under the same id, selective restoration retaining
unrelated work, agent attribution/undo/redo, replica reconstruction, private-key
and terminal-queue omission, bounded refusal and complete inventory paging.
Replica reconstruction is not a new certification of cold-host routing or
registered-action duplicate-delivery replay; the framework's existing replay
proof remains the one described above.

See [Code Editor qualification](../apps/code-editor/SOURCE_HISTORY.md) for the
repeatable real-root action-door harness and explicit unsupported coverage.
Binary contents, historical POSIX modes, symlink targets and whole-source
coverage are not supported by Project. Native text history does not silently
save or materialize a restored disk tree, and does not replace file-service
history or custody. Full Git-free source qualification must fail, even when the
opt-in focused text workload passes.

Research leads only: Ink & Switch [local-first software](https://www.inkandswitch.com/local-first/)
and [Patchwork](https://www.inkandswitch.com/patchwork/). They are pointers for
further study, not evaluated findings or evidence of Tilo's coverage.
