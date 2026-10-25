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
it adds to `src/op_set2/op_set.rs`. Lists and text are the next section.
Pinned by `crates/doc/tilo-automerge/tests/map_apply.rs`, which
compares documents, heads and patches with crates.io `automerge` 0.12.0
over randomised histories (including changes that overwrite an op under
another key) and checks the cost does not grow with history. Upstream
`main` walks the same way; the patch applies there unchanged in substance
and is upstreamable.

# Tilo patch: a change to a list or text does not walk the whole sequence

The same apply merges a change into a list or a text by walking every op
the sequence ever held, deleted characters included (`walk_list` and its
`Untangler`): a remote keystroke cost about 0.09 µs per op of the text,
1.8 ms once 20,000 keystrokes had been typed into it and 9 ms at 100,000.
Every collaborator's replica paid that per keystroke.

`walk_list` now seeks over runs of elements the batch cannot affect: no op
of the element is overwritten by the batch, no incoming op inserts after it
or updates it, no incoming insert is still waiting for its place, and (in a
text) the marks before and after the batch are the same. Walking such an
element only advances the walk's visible index by the element's width and
feeds its mark ops to the mark state; it logs no patch and changes no
conflict. The seek adds the run's width from the op set's `top` (list) or
`text` (text) index column, and replays the run's mark ops, found through
the action column, into the mark state. The walk therefore visits the same
elements with the same state and leaves the document, its patches and its
errors byte for byte as before. A change that names an element the object
does not hold (or one by an op that is not an insert) walks op by op, as
always. A remote keystroke now costs 6–8 µs at 1,000, 20,000 or 100,000
keystrokes of history alike, as do a list insert and delete. The
`MapSkip` guard applies here too: the object must hold at least 32 ops per
incoming op, so large batches walk as before (a 100,000-change load is
unchanged at about 260 ms).

The marked spots are `TILO(seq-apply-skip)` in `batch.rs` and the lookups
it adds to `src/op_set2/op_set.rs`. Pinned by
`crates/doc/tilo-automerge/tests/sequence_apply.rs`, which compares
documents, heads and patches with crates.io `automerge` 0.12.0 over
randomised three-actor histories of text and list edits (multi-unit
characters in every text encoding, marks and unmarks, block markers,
counters, nested maps, racing inserts, changes that overwrite an op of
another element or object), shares its harness with `map_apply.rs`, and
checks the cost does not grow with history. Upstream `main` walks the same
way; the patch is upstreamable.

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

# Tilo patch: check a peer's change before applying any of it

Automerge only ever makes changes the apply can place, but a peer can send
any change that parses. `BatchApply::apply` (`src/op_set2/change/batch.rs`)
trusted every one, and wrote each change into the history before walking
its ops. Hand-built single-op changes therefore either panicked the apply
(an insert after an element the sequence never held, an update of an
unknown element next to another op on that sequence, an op in an object the
document does not have, a sequence key in a map, an op naming an actor the
document has never seen, a gap in an actor's sequence numbers), or were
taken without an error while their op was dropped or recorded where the
saved document cannot hold it (an update of an unknown element on its own,
a map key in a list, a delete that overwrites nothing, a predecessor in
another key, element or object, ops numbered at or below their
dependencies'). The document then saved, but the save no longer loaded
("missing ops", "mismatching heads", or a panic in the loader).

The apply now builds and orders the batch's ops first and checks them,
writing nothing until the check passes:

- each change is its actor's next one, has the actor's previous change
  among its ancestors (every change Automerge makes depends on it
  directly), and numbers its ops after every op of its direct dependencies,
  which is where a saved document, storing no start op, takes them to
  begin; a change without ops numbers exactly up to them;
- each op is in an object the document or the batch holds, keyed as that
  object is keyed, does not insert into a map, names only ops with lower
  counters than its own, overwrites only ops in its own object and key or
  element (a delete overwrites at least one), and, in a list or text,
  names an element the walk will meet: HEAD, an insert earlier in the
  batch's ordering, or an insert op of that object in the document
  (`check_sequence_span` follows `Untangler::new`'s own classification).

A batch that fails returns `AutomergeError::InvalidChange` naming the
change, and leaves the document byte for byte as it was: the actors the
batch would have added are removed again and the patch log is not touched.
If the refused change, or one depending on it, came with this call, the
queue is put back as it was; a refused change that waited in the queue for
a dependency from an earlier call is dropped, with what depends on it, and
the rest of the batch is applied, so that it cannot hold back the changes
arriving after it. Changes Automerge makes pass the check unchanged, so the
apply's documents, patches and errors for them are byte for byte upstream's.
The internal assertions and `unwrap`s along the walk (`Untangler::finish`,
`untangle_inner`, `update_history`, the object lookup) now hold by the
check rather than by trust, which also covers builds that abort on panic,
where catching an unwind would not help.

One more apply path read peer data it never recorded: when a text element
holds a value that a batch deletes and a second, later value it does not
(a peer can add one without overwriting the first), `OpValueOption::set`
flagged the later value as exposed without recording the value it
replaces, and the text patch panicked on the missing value, whether or not
anyone read patches. The later value was already the element's value
before the batch (values come in id order), so it now records itself; the
patch replaces the character with itself, as a list's `put_seq` does.

The marked spots are `TILO(peer-change-check)` in `batch.rs`, `error.rs`,
`change_queue.rs`, `change_graph.rs` and `src/op_set2/op_set.rs`. Pinned
by `crates/doc/tilo-automerge/tests/crafted_changes.rs` (each class above
refused with the document unchanged and still loading; the forged value
and the honest delete applied in either order, with patches that replay to
the text; a refused change taking its delivery with it; a refused queued
change holding nothing back; and randomly mutated valid changes, which the
fork never panics on and either takes exactly as crates.io `automerge`
0.12.0 does, into a document that saves, loads and replays, or refuses
where upstream panics or leaves a document that does not). `map_apply.rs`
and `sequence_apply.rs` hold their forged cross-key and cross-element
changes apart from the histories they compare and check the same of them.
Upstream `main` applies the same way; the patch is upstreamable.

# Tilo patch: find an op by its id without searching the document

`OpSet::get_op_id_pos` (`src/op_set2/op_set.rs`) finds an op by searching
the `id_ctr` column for its counter. The column's B-tree skips slabs whose
counters cannot hold it, which works while counters rise along the
document. A text that collaborators typed into at scattered places holds
old and new counters side by side in every slab, so the search decoded
nearly every slab: about 0.25 ms per lookup at 80,000 ops. Taking in one
remote keystroke looks up the element it goes after twice (the peer-change
check and the sequence walk's seek, both patches above), so it cost 39 µs
at 1,000 changes of such history, 128 µs at 5,000 and 504 µs at 20,000,
on the worker and again in the caller; cursors and patch paths pay the
same per lookup.

`OpIdIndex` (`src/op_set2/op_set/op_id_index.rs`) holds where every op was
when it was built, per actor sorted by counter (8 bytes per op), and
follows the document from there: `OpSet::splice` records where each splice
went, ops spliced in since sit in the gaps between the built ops, and a
Fenwick tree over blocks of gaps counts them, so a lookup and a splice are
O(log n). It is built only for a document whose searches have been slow:
each search counts the slabs it may decode, and once those would have
paid for building the index (a slab costs about as much as 16 ops of the
build), the next apply builds it (2.5 ms at 80,000 ops) if the `id_ctr`
column spans at least 64 slabs and at least half as many counters are
lower than the one before them. A text typed in order has next to none
and packs thousands of ops into a slab, and an import or a load searches
little, so they hold nothing more (`patch_log_memory.rs` and
`tilo-doc-codecs`' `import_memory.rs` pin that). A replica taking remote keystrokes
into a scattered text builds it on the third: the 80,000-op document
above holds 4.4 MB, and the index adds 0.65 MB. A document found not
worth it is looked at again once it has grown by an eighth. The index is
dropped, to be rebuilt by the next apply, when ops leave the document (a
rolled back transaction), actors are renumbered, a gap grows past 4,096
ops, or more ops have been spliced in than a quarter of those built.

A position the index gives is used only once the id read there is the one
asked for; otherwise, or with no index, the column is searched as before,
so the index decides how fast an op is found, never which. A one-letter
remote change now costs 11–15 µs at 1,000, 5,000 or 20,000 changes alike
(release, p50 of 500).

The marked spots are `TILO(op-id-index)` in `op_set.rs`, `op_id_index.rs`
(with unit tests checking it against a model under random splices) and
`batch.rs`. With `slow_path_assertions` (or in the crate's own tests)
every lookup is also checked against the search. Pinned by
`crates/doc/tilo-automerge/tests/scattered_apply.rs`, which compares
documents, heads and patches with crates.io `automerge` 0.12.0 over
three-actor histories typed at scattered places and checks that what a
remote letter costs does not grow with such a history. Upstream `main`
looks ops up the same way; the patch is upstreamable.

# Tilo change: count the work an apply, a lookup and a sync walk do

The tests that pin the patches above (`map_apply.rs`, `sequence_apply.rs`,
`scattered_apply.rs` and `idle_sync.rs` in `crates/doc/tilo-automerge`)
compared wall-clock times of a change into a long history and a short one,
so a loaded machine could fail them. `automerge::work()` returns what this
thread has done: the document ops an apply stepped over one at a time
(`walk_list`, `MapWalker::next_doc_op`), the `id_ctr` slabs searches for an
op by its id may have decoded (`OpSet::get_op_id_pos`), and the changes
ancestry walks visited (`ChangeGraph::traverse_ancestors`). The tests compare
those counts. A count is one thread-local add next to work that decodes an
op or a change, and does not change what any of these do. The marked spots
are `TILO(work-count)` in `src/work.rs`, `batch.rs`, `op_set.rs`,
`change_graph.rs` and `lib.rs`.

# Tilo change: the fork's own tests run in CI

The workspace builds this crate as a path dependency and never runs its
tests, so `tools/ci/test-vendored-crates.sh` runs them: t's `vendored`
check whenever this directory or `vendor/automerge-test` changes, and the
nightly. The manifest therefore declares its own `[workspace]` (so
its tests run from this directory however the checkout is nested) and
restores upstream's `automerge-test` dev-dependency, which crates.io
publication drops and `tests/test.rs` needs, from `vendor/automerge-test`
(see its `PROVENANCE.md`). The test run starts from a copy of the workspace
lock, so the fork is tested at the dependency versions Tilo ships.

# Tilo patch: tell which history a waiting change belongs to

A room governor starts a room's document over from a base and writes the new
history under its own actor again, so a change of the history the base
replaced can hold an actor's sequence number that the new history uses too.
Waiting in the queue for what it builds on, such a change made the fork
refuse the new history's change at that number as a duplicate. Only one
change can hold an actor's sequence number in a history, so the receiver
(`tilo-peer-state`'s `room::base::lineage`) drops the waiting one once a
change built on the base it holds claims that number. For that it reads
`Automerge::applied_seq` and `Automerge::queued_changes`, and drops with
`Automerge::discard_queued` / `AutoCommit::discard_queued`, which remove the
named queued changes and those queued on them (`ChangeQueue::
remove_with_dependents`, which `remove_actor_branch_from` now uses too).
Applied changes are never touched. The marked spots are
`TILO(history-of-a-change)` in `automerge.rs`, `autocommit.rs` and
`change_queue.rs`. Pinned by `tilo-peer-state`'s `room::base` tests.
