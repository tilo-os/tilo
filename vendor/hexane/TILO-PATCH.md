# Tilo patch: a slab split gives back the capacity it no longer uses

`RawColumn` (`src/raw.rs`) loads a column as a single slab, since it cannot tell where one
value ends and the next begins and a value never crosses slabs. An insert that takes a slab
past `max_segments` splits it at the insert: `split_off` moves the suffix into a new buffer
and `truncate` cuts the slab down to the prefix, but a truncated `Vec` keeps its capacity.
The left half therefore went on holding a buffer the size of the whole slab, and so did
every left half after it: a column of a few megabytes, edited after it loaded, held tens of
times its size, and the memory grew with every split rather than with the data.

A synced folder's tree is one such document, loaded from disk and edited once for every file
written into the folder. A `git checkout` of a 20,000-file repository into one grew the
process holding it from 1 GB to 4 GB in about a minute: 2.8 GB of it was
`RawColumn::try_splice_slice`'s buffers, live, from 3,669 splits.

`splice_slab` now shrinks the left half after a split (`shrink_to_fit`) when it holds more
than twice what it uses. The copy is of a prefix less than half the buffer; a
split near the slab's end, which leaves the left half nearly whole, copies nothing, so a
column edited at its end does not copy itself on every edit. Pinned by
`raw::tests::splits_of_a_loaded_column_keep_no_more_than_they_hold`: 2,000 inserts into a
loaded 4 MB column held 30 MB before, and now hold less than three times the column.

Everything else is crates.io `hexane` 1.0.0-alpha.5, verbatim. Remove this copy once a
release carries an equivalent fix.
