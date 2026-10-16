# Universal version control: first slice and coverage audit

**Status: partial.** This describes the current native
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
4. `review-restore({version, paths})` validates the selection without writing.
   Its response contains `items` and a `review` object with `schema: 1`,
   `target_heads`, `live_heads` and `paths`.
5. Copy that object unchanged into `restore-reviewed({review})`. The edit
   checks the live frontier again and appends a restore attributed to the
   invoking collaborator (including an agent acting for a person).

For example, after comparing an old checkpoint against now:

```json
{"version": "good", "paths": ["cards/card-id"]}
```

is an argument to `review-restore`; the returned `review`, not this request,
becomes the argument to `restore-reviewed`. A diff is structural selection
information, not a complete before/after content renderer: use typed reads to
inspect content at the pinned versions.

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
- `crates/sdk/tilo-app/src/framework/history/review.rs`: `RestoreReview`,
  `answer`, `apply`, bounds and the two new action specs.
- `crates/sdk/ui-core/src/history_source/steps.rs`: `resolve`, `changed_items`,
  `restore_paths` and the existing forward patch/undo machinery.
- `crates/sdk/tilo-app/src/framework/preview.rs`: existing History mode builds
  the app's read-only view from a frozen historical resource reader.

**Compatibility:** existing `restore({version, ids?})` is preserved and remains
unchecked by the new review precondition. Its legacy id/whole-document behavior
is not the exact-path protocol above. Existing undo, redo and checkpoint remain.
The tests compare historical action-read content with the existing UI preview;
they do **not** establish that UI restore controls invoke these new review
and restore actions. This slice does not replace existing UI restore routing.

## Repository-grounded coverage audit

“Existing” below means an implementation is present, not all-app certification.
Standalone `tilo.history` is the UI-core action provider, distinct from the
`tilo-history` crate that renders history sentences.

| Capability | Native `ResourceApp` | Standalone `tilo.history` | Files | Workspace |
|---|---|---|---|---|
| History/provenance | Framework history actions | Existing session/provider | File version records/projection | Aggregates tree and resource records |
| Historical content | New bounded typed `read(version, item?)` | Existing session preview | Existing frontier/version preview reads | Entry/detail views; not a typed snapshot of every resource |
| Compare | `diff` items plus full head arrays | Existing History model | Separate checked-file history model | Existing causal detail/replay views |
| Restore safety | New exact-path review + stale-frontier guard | Existing document-bound review/confirmation | Separate checked review/restore protocol | Existing undo/detail routes; no workspace-wide atomic restore |
| This slice's proof | Native action door and replay fixtures | Not newly certified here | Not newly certified here | Closed/cold host routing not tested here |

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

Assumptions: retained target changes, readable current resource schema, committed
frontiers, normal action admission and the resource's own registered decoder.
No multi-resource atomic snapshots, branch/merge product workflow, all-types
schema-evolution proof or universal redaction proof is delivered here. Closed
resources, cold/remote hosts and restart routing need end-to-end tests for this
new workflow. Broader coverage and UI review-action integration remain work.

That old **#3875 was closed unmerged** describes that PR, not absence of
the architecture: Git records **`d8bc16deea` — “feat: one
resource/action/view model”** as landed. Audit current code rather than treating
that PR's disposition as the implementation inventory.

Research leads only: Ink & Switch [local-first software](https://www.inkandswitch.com/local-first/)
and [Patchwork](https://www.inkandswitch.com/patchwork/). They are pointers for
further study, not evaluated findings or evidence of Tilo's coverage.
