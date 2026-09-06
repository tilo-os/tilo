# Precompile review 1 — rejected, no Cargo/runtime

Terra froze a first diagnostic implementation. Root found four reporting/test
issues before executing it:

1. Snapshot vector is oldest-to-newest, but hit rank used its zero-based oldest
index. MRU rank 1 must instead use len-index, before removal.
2. A geometry hit updated mode but retained the original descriptor. Subsequent
comparisons would not be against the most-recent same-geometry observation.
3. Reported extracted-span byte semantics said every successfully extracted
input, but pending/invalid-route guards returned before accounting. Reconcile
implementation and definition; keep mutually-exclusive rejection totals.
4. Required benchmark LRU/gating/rejection and config/span extraction tests were
absent; only one upload-uniqueness test was added. Add focused regression tests
before any timing or build can be authorized.

Also reject zero-path empty scene sentinel and non-word-aligned packed uploads
rather than treating their padding as geometry; parse config/spans through a
pure helper for direct testing without constructing a GPU renderer.

No runtime result or performance claim exists for this version. Only the three
previously authorized files may change. Existing evidence remains untouched.
