# Private GTK 3 recorded-speech qualification archives

See [the source qualification ledger](../../../../docs/architecture/dictation-gtk3-native-evidence.json)
and [its interpretation](../../../../docs/architecture/dictation-gtk3-native-evidence.md).
These are receipts and public-data drivers, not runtime installation or model
activation instructions.

The native fixture uses standard GTK 3 / GtkSourceView 4 controls. It changes
neither input-method handling nor undo behavior. GTK/runtime binaries, headers,
model weights and audio are not redistributed here. Signed package provenance
and the public recording/model pins are retained in the manifests and receipts.

Failed diagnostics remain failed. The successful run uses the compositor's
`SAVE_TARGETS` payload filter and preserves the strict editing, one-step undo,
cancellation, focus retirement, protected-field and ordinary-recovery assertions.
Each ledger archive resolves to one canonical repository file, including aliases
into earlier qualification directories. `.log.json` files retain raw escape
characters and exact line endings: concatenate `lines` to recover portable stdout.
Path substitutions replace the owned remote build root with `${BUILD_ROOT}`
and the developer home with `/var/home/you`. The ledger retains original byte
digests separately from normalized file and reconstructed-stdout digests.
