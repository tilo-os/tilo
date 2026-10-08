# Committed Code Editor source snapshots

`read-text-snapshot` is a separate read-only file operation registered with the
existing SDK semantic-document reader. The SDK selects the actual retained
widget's reader and reads its current canonical record: either a verified hosted
record loan or the mounted native primary document. It refuses an unidentified
or retired owner and pending edits. It never commits a transaction, writes a
file or grants filesystem access. Ordinary read/edit and device-only Save
behavior remain unchanged.

The SDK supplies `version`, `record` (room/application/record identifiers) and
`basis.heads` from the actual immutable document's `base_heads()`. The app
supplies bounded `content`, with the same Unicode scalar positions, current
whole-file SHA-256 and contextual text as `read-text`. `nextCursor` is null.
Record identifiers are the existing public semantic-record metadata; a
path-addressed record can embed a device-local path. The app does not manufacture
original actor, invocation, checkout or committed-admission facts. The heads
identify the observed committed projection, not a separately captured earlier
admission basis.

For the first page omit `expected_heads`. For subsequent pages supply the exact
sorted heads array as a JSON string. Changed heads refuse with `stale_cursor`,
even when an edit followed by its inverse restores the original bytes. Pending
operations refuse without assigning or closing their transaction. Requests are
closed, file-bound and limited to 8 KiB. Reads allow at most 4096 Unicode scalars,
8192 UTF-8 bytes, 64 heads and one million document operations. Content is at most
16 KiB of encoded JSON, reserving space for the SDK's bounded record/head stamp;
the complete response stays below 32 KiB.

A projection is not a portable signed receipt or a capability. An evaluator must
obtain the actual original terminal response through the authenticated owner
control session and bind its acknowledged operation and published
workspace/application/record/file to the preauthorized source mapping. Offline
JSON and hashes alone cannot establish admission. Original terminal metadata
and the observed committed projection are distinct evidence.

An owner can deliberately materialize complete reads from one exact revision
into a new exclusive source candidate. The evaluator checks complete text,
whole-file digest and allowed paths, preserves the source baseline identity and
records the original observations. It must not overwrite the learner checkout,
expose private grader sources or claim that CRDT edits already changed disk.
Approved checks must use the materialized candidate, not the original disk file.

The earlier hosted-only snapshot draft failed on an actual native invocation
because it required a scope ordinary native effects do not install. Its source,
package and failed terminal receipts are retained separately. This registered
read design reuses the existing canonical-document lane without changing native
custody, permissions or publication gates.
