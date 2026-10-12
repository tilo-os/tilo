# Code Editor file text actions

Each file of a Code Editor project is published as its item
(`code_editor.project/files/<id>`) and offers `read-text` through the same
resource/action door as its open and save operations. Its text is edited by a
field edit at `buffers.<id>`. Neither accepts a filesystem path, command,
identity or grading configuration. The selected record and file retain the
framework's current permission checks.

`read-text` is a read-permission, idempotent effect bound to that file's ID. Open
the file first so its collaborative body exists; reading does not hydrate disk
content or bypass file access grants. Its `start_chars` and `max_chars` use
zero-based **Unicode scalar values**, matching the existing Automerge splice
coordinates. They are neither UTF-8 bytes, UTF-16 code units nor grapheme
clusters. A combining mark has its own position.

The effect reads the immutable document snapshot lent to the current invocation.
It returns the whole current file's UTF-8 SHA-256, total character count, actual
range, next position, EOF flag, text and up to 32 scalars of surrounding context.
The requested range is 1–4096 scalars; text is additionally capped at 8192 UTF-8
bytes. The **entire serialized JSON reply**, including context and metadata, is
bounded to 32768 bytes. An out-of-range position or unavailable/deleted file
refuses; EOF can return an empty range. The next position reports the actual
bounded range, so callers can page without splitting a scalar.

An edit is the project's `field-edit` at the file's text carrying the text's
own `contextual` operation (`SourceEdit::Contextual`): the current base digest,
start and deletion counts, inserted text, deleted text and bounded surrounding
context from a read. Counts must be nonnegative integers; deletion count must
equal the deleted text's scalar count. Inserted/deleted/context text together
may occupy at most 16384 UTF-8 bytes; each context may contain at most 32
scalars.

At a matching base digest, its scalar position and deletion count select the
range. With a stale base, the contextual splice permits only one untouched
matching range; overlapping or ambiguous changes refuse before any splice. The
editor's own keystrokes are the same edit, so attribution, history and undo are
one machinery. It creates no private document or disk writer. Saving the result
keeps the existing save confirmation and device policy.

Mounted-app public-door tests exercise Unicode ranges, current content identity,
UTF-8/aggregate limits, bounded editing, framework undo/redo, disjoint contextual
rebasing, atomic conflict/ambiguity refusal, malformed parameters, foreign file
arguments and Viewer read/write separation. These are production-path app
checks, not evidence of a model-authored repair or a separately packaged native
process run.

The developer qualification script `scripts/headless-examples/code-editor-text-proof.py`
packages and installs the normal signed native app and runtime, enrolls a private
session through the existing bootstrap, and places a generic UTF-8 fixture in
that room's ordinary app directory. It opens the file as the authorized human and supplies only the actual
advertised approval parameters; no unattended Agent confirmation is inferred. It invokes
public open/read/edit actions,
observes original terminal receipts, and checks overlapping stale refusal through
another read. It requires already built platform/bootstrap binaries and a new
private output directory:

```sh
python3 scripts/headless-examples/code-editor-text-proof.py \
  --binaries target/debug --out target/platform-integration/code-editor-text-v1
```

The project resource declares its kind (`code_editor.project`) as the literal
required by the normal packager. The proof checks the actual installed signed manifest includes this
resource before launch. It records observed source and executable/package hashes separately; those
observations do not attest which source bytes a compiler consumed. The script
cleans up its own processes and does not change the operator's profile. An
unobservable terminal, changed source or refused native mount fails qualification;
a successful mounted-app test alone does not establish this native result.

Packaging uses the operator's ordinary build configuration before runtime
admission. The runtime phase uses its separate private profile and the existing
canonical OS font catalog, mounted read-only. It checks prepared source, signed
package, runtime and executable hashes at the phase boundary and refuses drift.
These checks record the actual inputs; they do not establish compiler lineage.

The CRDT buffer and the source file remain distinct: the native proof checks that
editing leaves the fixture's disk bytes unchanged. A coding check must first use
an explicit owner-admitted export into its authorized source snapshot. This
capability does not grant an Agent the existing device-only Save operations.
