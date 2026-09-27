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
