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

# Tilo patch: list the change log's hashes without rebuilding any change

A history reader that keeps what it read of a document (the shell's activity
pane, refreshed on every desktop change) has to know which changes are new
and that the ones it read are still the start of the log. `get_changes(&[])`
answers both by rebuilding every change from the op set, and
`get_changes_meta(&[])` by building every change's metadata: either is
O(history) per refresh. `Automerge::change_hashes` borrows the change graph's
own hash list, which is in the order `get_changes(&[])` lists the changes and
to which adding a change only appends. Pinned by
`crates/doc/tilo-automerge/tests/change_hashes.rs`.

# Tilo patch: a change to a map does not walk the whole map

`BatchApply::apply` (`src/op_set2/change/batch.rs`) merges the ops of incoming
changes into each object they touch with one walk over every op of that
object, from its first. A map keeps every value ever written to it (an
overwritten value stays, with a successor), so one change that rewrites one
key cost about 0.09 µs per op the map ever held: 437 µs per change once a
key had been rewritten 5,000 times, 1.7 ms at 20,000. Every replica that
takes remote changes as they arrive paid that per change.

`MapWalker` now seeks over the ops a batch cannot affect: those that are
neither overwritten by the batch (in its pred cache) nor visible in a key
the batch writes or overwrites. Walking such an op changes no state and
logs no patch, so the walk visits the same ops in the same order and leaves
the document, its patches and its errors byte for byte as before; where the
walk would stop is found with the key and id columns, and checked against
the ops either side of it (anything unexpected walks op by op). A map change
now costs what it costs on a short history, 10–20 µs, at 1,000, 5,000 or
20,000 changes alike. The skip is used only when the object holds at least 32 ops per
incoming op, so loading a large batch takes the walk it always did. The
apply also no longer copies the index of every object in the document to
record the few objects a batch creates.

The marked spots are `TILO(map-apply-skip)` in `batch.rs` and the lookups
it adds to `src/op_set2/op_set.rs`. Lists and text still walk the whole
sequence. Pinned by `crates/doc/tilo-automerge/tests/map_apply.rs`, which
compares documents, heads and patches with crates.io `automerge` 0.12.0
over randomised histories (including changes that overwrite an op under
another key) and checks the cost does not grow with history. Upstream
`main` walks the same way; the patch applies there unchanged in substance
and is upstreamable.

# Tilo patch: load with the change hashes the saver listed

A compacted document chunk stores each change's metadata but not its hash,
so `Document::reconstruct` re-encodes every change from the op set and
SHA-256s it to build the change graph: about 45 % of loading a large
document, to learn hashes the program that saved it had in memory.
`LoadOptions::known_change_hashes` takes them instead, in the order
`Automerge::change_hashes` listed them at the save. The load then builds only
the op set's indexes (one pass over the ops, noting each actor's lowest
counter) and `ChangeGraphCols::finalize_known` builds the same graph
`finalize` does: op counts from the dependencies' maximum op, where Automerge
starts every change that has some, and from the actor's lowest counter for a
first change without any; authors from the stored extra bytes.

Hashes are used only if they are one per change, all distinct, the ones no
change depends on are exactly the document's stored heads, and no change
after an actor's first lacks dependencies; otherwise the changes are rebuilt
as before. They are not checked against the changes themselves, so a caller
must pass only hashes it listed for exactly these bytes (Tilo's `app-state`
binds them to the document chunk by digest and never takes them from peers).
Without the option nothing changes. Pinned by
`crates/doc/tilo-automerge/tests/known_hashes.rs`.

# Tilo patch: no hash lookup per op on an empty map

Reconstructing a document looks every op's id up in the change collector's
`preds` map and the index builder's `counters` map, both of which are empty
for nearly every op (they hold only ops still waiting for a successor or an
increment). Skipping the SipHash when the map is empty saved 8–10 % of
loading a large document; the maps' contents and the result are unchanged.
Covered by the same test.
