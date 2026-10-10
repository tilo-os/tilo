# Recorded GTK native dictation and field controls

These are historical source-pinned drivers and full recorded outputs from the
hardware-isolated GTK qualification at compositor/worker source head
`920cc7541449bdf53b557acf4939682bb1bdc47b`. They are not an installer or a
current-head launcher. `TILO_QUALIFICATION_BUILD_ROOT` denotes the owned remote Cargo drop-in
worktree root; artifact mounts are under its `target`, distinct from `src`.
JSON and logs use `${BUILD_ROOT}` in place of the absolute remote task path.
The adjacent architectural ledger retains original and portable hashes.
Terminal colour/control bytes are preserved as JSON-escaped stdout lines in
`*.log.json`; concatenate their `lines` to recover the full portable stream.
This keeps repository blobs text/LF without discarding output or changing policy.

`atomic-*` proves selected-range dictation, ONE native Undo/Redo, cancellation,
fresh restart and focus away/back in actual GNOME Text Editor. Stock GTK's
strict one-step Undo test remains failed; this is a client adapter result.
`privacy-*` separately proves standard GTK field focus, password/PIN refusal,
PRIVATE-hint consent/cancellation, and ordinary keyboard editing/history.
That additional fixture does not recognize speech or establish personalized-data
learning behavior. `private-refusal-failed-*` preserves the earlier failing
assertion that PRIVATE should behave as a secret purpose; GTK documents a
different meaning. No failed assertion is relabeled as passing.

Full remote evidence was exported while holding the existing owned remote Cargo drop-in
worktree lock against artifact collection, then released. A rerun needs matching
source/binary/module guards, verified public fixtures and fresh output names.
Never bypass a receipt to reuse an old result. No local VM, hardware microphone,
host desktop, persistent catalogue or runtime activation was used.

Model: pinned MIT Whisper Small Q5_1. Public speech: Google FLEURS, Conneau et al.
(2022), German development case `de_de-0`, CC-BY-4.0. Fedora GTK/Pango/Graphene/
GdkPixbuf headers were privately extracted only after exact Fedora 44 signature,
header/payload digest and package-version checks; metadata/notices remain with
those private inputs. No model, audio, header or compiled runtime is included.

Runtime versions, exact hashes, limits, successful and failed scope boundaries:
[GTK client transaction evidence](../../../../docs/architecture/dictation-gtk-client-transaction.json).
