# Tilo patch: keep each text object's mark changes in its own patch

`PatchLog::mark` (`src/patches/patch_log.rs`) extends the previous event when
that event is a mark change, without checking which object it belongs to. A
diff that walks several text objects whose formatting changed therefore folds
the mark changes of every later object into the patch of the first one: one
`PatchAction::Mark` on the first object carrying every object's ranges, and no
patch at all for the others.

Tilo reverts an action by replaying the diff from after it back to before it,
so undoing one `SetMark` over spans in several paragraphs (each paragraph its
own text object) cleared the mark from the first paragraph only, and reapplied
the other paragraphs' ranges to the first.

The patch makes a mark event extend the previous one only when both are on the
same object; everything else is crates.io `automerge` 0.12.0, verbatim. It is
pinned by `crates/doc/tilo-automerge/tests/diff_marks.rs`. Upstream `main`
has the same code; remove this copy once a release carries the fix.

Three `#[allow]`s (the `log!` macro's import, `GenericColIter::try_next` and
`ValueRange::try_splice`) silence warnings the current compiler raises in the
upstream code, since CI builds path dependencies with `-D warnings`.

# Tilo patch: read committed history through a shared reference

`AutoCommit::get_heads`, `diff` and `document` take `&mut self` because they
close the open transaction first. Tilo hands widgets the document read-only
and commits before every frame, so `committed_heads`, `committed_diff`,
`committed_fork_at` and `committed_document` read the committed document
from `&self` (an open transaction is not part of it). Pinned by
`crates/doc/tilo-automerge/tests/committed_readers.rs`.

# Tilo patch: an idle sync message does not walk the whole history

Receiving a sync message trims the hashes sent so far to those the peer has
not seen (`Automerge::filter_changes` → `ChangeGraph::remove_ancestors`), by
walking every ancestor of the peer's heads. Between two peers that resume
from persisted sync states and have nothing to send, that set is empty, yet
the walk visited the document's whole change graph on every message: about
2 ms at 50,000 changes, more than the rest of the exchange together. tilo-fs
runs such an exchange with every peer every few seconds. `remove_ancestors`
now returns at once for an empty set and stops walking once it has removed
everything; the set it leaves is unchanged. Pinned by
`crates/doc/tilo-automerge/tests/idle_sync.rs`.
