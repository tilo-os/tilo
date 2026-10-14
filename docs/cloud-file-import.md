# Connecting cloud accounts and importing files

In **Settings → Accounts**, add Google or Dropbox. For Google, choose **Files**,
**Mail**, or both; Mail appears in the Mail app after Gmail access and the mailbox
identity have been verified. Choose **Keep files in sync** for an ongoing
connection or **Import files once** for independent local copies, then select
**Continue** to authorize the selected services in your browser.

After connecting, use **Browse files** or **Choose files to import…** in Settings,
or open the account in Files' sidebar. For a one-time import, select the files
or folders and choose a destination. A persistent sync connection is not required.

Cloud accounts offer two file modes:

- **Sync** exposes the normal ongoing folder under `~/Remote/<account>`.
  Edits to ordinary files there upload to the provider. Accounts saved before
  the mode setting existed keep this behaviour.
- **Import** appears in Files' places list without creating a filesystem mount.
  Opening the account browses the provider through the storage daemon. Files
  receives names, opaque IDs, revisions and file bytes, never provider credentials.

In an import account, select one or more files or folders and choose **Import
once…**. Files opens the desktop's shared writable-directory chooser. No
Downloads destination is assumed. Cancelling the chooser writes nothing.
The document-portal chooser displays system-owned read/write consent and exports
only the selected destination; importing adds no broader filesystem or sandbox
permission. The native shell chooser does not issue new grants: it offers only
folders already writable by the app within its workspace scope.

Folders are enumerated recursively with provider pagination. The transfer shows
enumeration, preparation/export and copy progress, completed-entry and byte
counts, and individual failures. **Cancel import** closes the active daemon read
and discards its private incomplete staging data. Only completed top-level
outputs are published, without replacing existing files. Completed outputs remain.
If staging has been replaced or contains unexpected user data, cleanup refuses
to delete it rather than risking those files.
**Retry failed** starts another recorded operation for entries that did not
complete; successful copies are not copied again. **Show destination** opens the
chosen folder.

Imports are ordinary independent files: later cloud edits do not change them,
and editing an imported copy does not upload it. Existing files are never
replaced. Each colliding name gets a fresh name. Destination components are
opened without following symlinks, and output creation is relative to that
anchored directory. Known synced cloud destinations are rejected both when the
request is admitted and when it executes.

## Native cloud documents

Supported Google Docs, Sheets and Slides are exported by the daemon as Office
files. Import streams the exported bytes, not provider-link JSON or a URL
shortcut. Unsupported document types fail explicitly instead of producing a
misleading local document. In synced Drive locations, native documents are
read-only converted views: editing those exports cannot safely round-trip to
the original cloud document. Files explains this distinction in the synced view.
An independent imported copy is editable. Conversion can change formatting or
omit provider-specific features; it is not a lossless native-document round trip.
Drawings use an advertised SVG or PDF export. Forms and Sites have no supported
independent representation in this workflow.

Changing an existing synced account to import mode first checks for active writes
and queued uploads, then freezes and stops its mount before committing the change.
Pending work prevents the change; finish syncing and retry. An interrupted commit
has an explicit recovery action in Settings and does not automatically restart
writable sync underneath it.

## Actions and history

Browse, selection, import, progress, cancellation, retry and reveal use the same
typed Files effects as the visible controls. Cloud rows expose selection and
folder-opening actions. Discover the current action directory with:

```sh
tilo-control actions list
```

Cloud imports enter Files' existing device-targeted document outbox. Destination
admission must happen on the executing Files host: the `cloud-import` action
refuses another device as its target before inspecting the destination. Invoke
Files on the receiving device and choose the destination in its workspace
instead. A sender's local inode is not receiving-device consent.

The worker records the admitted directory identity and each actual output's
identity and path. Undo refuses replaced outputs, renamed or replaced destinations,
and symlink substitutions. Compensation uses anchored directories and no-replace
moves; its Trash receipt retains the identities for redo. It does not alter the
cloud originals. Interrupted, unfinished outbox operations are not a durable
resumable-download protocol.

The compatibility `import-once` action on a mounted entry still accepts an
explicit `folder` destination and uses the ordinary mounted-file copy path.
It no longer has an automatic Downloads menu command. Provider-backed browse
and streamed native exports are the import-account workflow described above.

## Verification

`apps/file-explorer/src/cloud_transfer.rs` tests the real framed cloud client
against a local protocol-service fake: exported bytes, collision receipts,
recursive paginated folders, provider failure, cancellation/CloseRead, cleanup,
malformed names and destination symlinks.

`provider_actions_choose_destination_and_import_office_bytes_without_mount`
opens an import account through its Files effect, selects a provider row, uses
the shared recording host's writable-directory chooser, imports an Office ZIP
payload, observes progress and recorded output paths, edits the independent
copy and reveals its destination. Account projection tests check that import
accounts have no mount and do not reserve synced folder names. Resource tests
exercise queued imports and their undo records.

The shell's directory-chooser tests inspect the actual shared picker request,
including its trusted write intent and system-owned title and confirmation label.
Backend tests observe the connected workspace grant store, explicit read-only
versus writable export responses, cancellation and selected-only URIs. Native
chooser-service tests reject read-only and out-of-scope destinations without
adding workspace grants.

A separate private-session-bus check with the host's external
`xdg-document-portal` exercised directory `AddFull` exports: its per-app FUSE
view accepted writes for the selected read/write directory, rejected writes for
a read-only export, and exposed no sibling directory. This checks the external
broker, not the complete chooser-to-frontend-to-broker path: the repository does
not implement the XDG frontend's export step, and the private broker check was
not driven by an application's returned chooser URI. Full hosted application
access through that frontend remains an integration verification gap.

The Settings acceptance test connects through isolated OAuth fixtures and drives
the real storage daemon and published Files actions: Office exports, destination
selection, progress, cancellation, retry, independent editing and undo. The hosted
UX test renders those production widgets in light and dark themes, including
narrow layouts and error states. Transition tests cover daemon restart and
recovery of a pinned mode-change commit.

These are not a login to a person's cloud account. They do not prove provider
project approval or scope availability, full desktop access through the external
XDG frontend, every process-crash scenario, or a lossless native-document round trip.
