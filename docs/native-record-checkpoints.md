# Native room app-record checkpoints

`tilo_joined_room::NativeWorkspaceOwner` implements a local, durable
checkpoint for an explicit set of native room app records. The desktop publishes
capture, inspect, name-bind and find through its ordinary room action service.
This is not a whole-workspace backup, an isolated project, a file checkpoint,
or a general search provider.

## Implemented boundary

- `capture_checkpoint`: authenticated caller certificate, separately held current
  governor signing key, non-nil immutable UUID, bounded human name, and a sorted,
  distinct, nonempty set of `(app, record)` routes. At most 32 routes, 3 MiB per
  snapshot, 16 MiB per capture, 128 retained checkpoints and 128 names. Metadata
  blocks are capped at 256 KiB; content-backend quotas can refuse sooner.
- The owner's mutation turn pins one committed room generation across every
  record read. The manifest records that generation, its durable aggregate hash,
  full sorted Automerge heads, exact content address/length, encoding, and the
  `tilo:record` kind/surface and `tilo:version` identities actually in each record.
  Absent legacy identities remain explicitly absent: the owner cannot infer an
  installed application schema from them. This does not migrate schemas or
  promise that today's app can interpret an old resource.
- Snapshots, manifests, original acknowledgements and movable-name mappings are
  content, retained and read back through the existing `Store::put_history` /
  `read_history` backend. The index is referenced by `native_checkpoint_root_v1`
  in the actual admitted room document. Governor-signed durable mutation and
  outgoing canonical governance publication cross the existing owner barrier.
  There is no separate authority database or private unanchored manifest table.
- Acknowledgements are returned only after successful owner publication. Retrying
  the same UUID and original capture request returns the original manifest and
  publication generation, including after later mutation or movement of names.
  A changed route set or original capture name cannot reuse that UUID. Missing
  retained data cannot be treated as absence or acknowledged as a successful retry.
- `bind_checkpoint_name` can move a name to an existing immutable ID;
  `resolve_checkpoint_name` returns that ID's original acknowledgement. Naming
  does not mutate the manifest or reinstall historical permissions.
- `read_checkpoint` materializes a bounded set of saved documents in memory. It
  checks the exact current authenticated certificate and current Read permission
  for **every** route before returning any historical metadata or bytes. It does
  not install records, copy grants, or publish a destination. Current revocation
  therefore affects historical reads and capture retries.
- Missing, corrupt, encrypted, causally incomplete and checkpoint-pending source
  records are refusals, not partial captures. Damaged retained blocks, unsupported
  encodings and mismatching heads/schema identities fail materialization. The
  governing owner validates retained manifests and snapshots on reopen. A member
  may reopen signed references without the governor's content, but cannot read
  unavailable blocks. Content transfer/hydration for these references is not
  implemented.
- A definite pre-publication failure installs neither index nor acknowledgement.
  An uncertain publication uses the existing owner fence and requires reopen.
  Pre-retained but unpublished blocks can remain orphaned; this code does not run
  a garbage collector with an incomplete root set.

## Ordinary public room actions

The existing `workspace_memberships` room service publishes the
`desktop:checkpoints` resource (`workspace-checkpoint` kind), using
`room_service::serve_room` and the normal broker invocation journal:

| Action | Arguments | Result |
| --- | --- | --- |
| `capture` | `name`, sorted distinct `routes: [{app, record}]` | Durable acknowledgement; the invocation UUID is the immutable checkpoint ID |
| `inspect` | immutable `id` | Verified manifest, including authenticated person attribution when captured by this service |
| `name-bind` | `name`, immutable `id` | Original acknowledgement of the selected checkpoint |
| `find` | exact `name` | Original acknowledgement of the currently named checkpoint |

These actions are available through ordinary catalogue discovery and invocation
(including `tilo-control actions list` / `actions invoke`), not a new socket,
JSON control shim, shell-admin shortcut or wire exposure of `OwnerCommitRequest`.
The closed typed family rejects extra authority fields, malformed IDs, overlong
names, unsorted/duplicate/empty/excessive route sets, and desktop governance
coverage. Public argument envelopes are capped at 160 KiB. Existing owner commit
and target postcard positions are unchanged.

Public execution is person-only: the broker retains the original admitted device
certificate and invocation, never accepting caller, author, agent, signing key,
room or invocation from arguments. Capture and naming additionally require the
person's **current room undoable-write permission** (`ActionKind::Undoable`, the
policy model's Write category); current Read is required on **every source** for
capture, inspection, name movement and resolution. No historical certificate,
membership, grant or authority is installed or copied. Abstract action discovery
reveals no checkpoint names or source records.

Shell `NativeOwnerCommitService` obtains the selected current governor through
`trusted_current_device`, separately from the caller. It refuses remote-governed
rooms, missing governor custody and unavailable/fenced owners, including reads.
There is no remote forwarding or content hydration fallback.

Mutation invocation bindings and original acknowledgements live in the existing
retained checkpoint index, published atomically with capture or name movement.
They bind the exact original command and caller evidence. A different caller or
changed command cannot reuse an invocation. Retrying a name movement after
another movement returns its original result without moving the name back.
At most 256 mutation invocations are retained; exhaustion is an explicit refusal,
not eviction of retry evidence. The broker journal binds reads and their results
as well. Before disclosing a successful journal result, the service rechecks
current source permissions, custody and retained integrity against the **result's
immutable ID**, even when a name has since moved.

Public results contain acknowledgements or verified manifests only, never raw
private document snapshots. Every read/resolution and successful-result retry
verifies retained snapshots; missing or corrupt content is an explicit refusal,
not a partial manifest or an empty checkpoint. The internal materialization API
remains host-only.

## Cross-owner and isolation prerequisites

Only native room app records share the barrier implemented here. Collaborative
filesystem records, document/media blobs referenced by app documents, external
files, packages, settings, and arbitrary exported application resources are
**not** included. Capturing an app document does not recursively retain the
content it references.

- [ ] A file/content owner must expose a durable capture epoch and an authenticated
  retained-version root participating in the same all-or-nothing publication.
  Joining independent successful reads is not an atomic cross-owner checkpoint.
- [ ] Isolated project/branch/copy creation needs a fresh destination room and fresh
  record/reference identities, explicit reference rewriting, current authorization
  to every source, and fresh destination grants. Reopening the captured room or
  copying its historical grants does not provide isolation.
- [ ] Destination owners must stage the complete bounded destination and publish
  it all-or-nothing. The current native single-room APIs do not provide a reviewed
  cross-room/file-owner transaction. No partial destination or false atomicity is
  implemented here.
- [ ] Schema-aware materialization needs verified app-version/migration authority
  for records with absent or incompatible identities; native snapshot restoration
  alone cannot make that promise.

Production-path tests are in
`crates/collab/tilo-joined-room/src/checkpoint_tests.rs`: one-generation multi-route
capture, signed outgoing root, exact retry/reopen, later-mutation pinned reads,
movable names and collisions, bounds, content damage, publication failure,
current per-route grant revocation, stale signed-root admission and certificate
substitution/removal. The installed shell service and certified broker resource
path are exercised in
`crates/session/shell/src/shell/broker_services/native_checkpoint_tests.rs`,
including reopen/retry, argument substitution, remote-governor refusal, missing
custody, retained-content corruption and cached-result source revocation.
