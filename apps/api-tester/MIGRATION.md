# Opening older API workspaces

When the host supplies an existing record containing the former `api_tester_*`
document layout, the typed entry point imports it before projecting or publishing
its resource. It preserves the active unsaved request,
saved collections, request rows, environments, selection and history. Basic user
names and API-key header names remain request metadata; passwords, bearer tokens
and API-key values move to this device's secret store under immutable references.

The original document is the retry source until every protected write succeeds.
On the web, pending IndexedDB acknowledgements defer publication and the host
retries opening on a later turn. A refused write leaves the original untouched;
unlock the store or free browser storage and reopen the workspace to retry.
Acknowledged writes are retained during a multi-field retry. No partial typed
workspace is published, so a failed import cannot silently replace a draft with
an empty request.

**Migration is not secure erasure.** The CRDT still contains the old plaintext
in historical changes, and other participants, exports and backups may have
copies. Rotate credentials that were ever shared this way. To remove their old
values, retire those documents and their copies and create a fresh workspace;
editing or deleting current fields does not remove CRDT history.

## Snapshots outside the new record store

Automatic conversion does **not** discover or open snapshots saved by the former
framework in a different persistence location. A blank new workspace is not proof
that an old draft was absent. Keep the old snapshot and its backups; do not delete
or overwrite them on upgrade.

For those snapshots, open a new, untouched API workspace and invoke the device-only
`ImportLegacy` effect (`import-legacy` in the action listing), supplying `snapshot`
as the base64 encoding of the old raw Automerge file. Discover its provider and
operation with `tilo-control actions list`, then use the normal action invocation
interface. This is a semantic import path, not a file-picker-only feature. Keep
credential-bearing input out of shell history and logs. It is not a JSON or Postman
collection importer. Already protected references require the original device's
secret store; missing credentials refuse import rather than becoming empty values.

Explicit import copies the projected content into the empty typed workspace, not
the old CRDT changes. It awaits every credential write before publishing any
fields, and refuses if the destination changed while awaiting storage. Keep the
source until the new record is durably saved. On failure, retry from that source;
a completed secret write may leave an unused device-local entry. This does not
remove historical plaintext from the source or its copies. Legacy variables marked
secret with nonempty values currently refuse import: they are not copied as shared
plaintext or silently discarded. Ordinary URL, body, header and variable contents
are preserved, not scanned for unmarked credentials; review them before sharing.

The Rust tests exercise the actual typed startup gate and explicit effect, durable
reload, and injected secret-backend quota failure after an earlier successful
write. These native tests are not a real browser quota test. The secret backend's
JavaScript tests separately cover delayed acknowledgements and refused commits;
end-to-end browser persistence and old snapshot discovery are not claimed here.

The framework's host-aware `ResourceApp::import_record` gate is separate from
schema evolution: deterministic schema migration cannot obtain device secrets
or wait for local durable custody. The gate defaults to no operation for other
apps, runs before typed reads, and withholds the resource when import refuses.
