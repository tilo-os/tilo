# Local Files clipboard transfer boundary

Status: PR [3009](https://code.tilo.so/tilo/tilo/pulls/3009) remains draft with
`hold` pending required current-head validation. Mounted journeys, the strict
sandboxed-child SDK route, and the signed Files entrypoint's actual
Copy/Paste/keyboard Undo/Redo with a terminal History receipt pass. The final
visible-resource assertions and polished offscreen screenshots also pass. Packaged
Trash listing and Restore remain unqualified.

## Purpose and first slice

A file selected in one Files window previously could not be pasted in another:
its clipboard belonged to that widget. Ordinary path-based file operations also
could adopt a replacement source or receiving folder. The first slice connects
selection, Copy/Cut, Paste, history and recovery through the existing typed
action/resource surface. It does not duplicate window navigation, Contacts,
Chat resources, the on-screen keyboard or the UI crate split.

The useful desktop affordance is continuity of the exact selected resource:
changing the shown folder or a filename must not silently change what gets
copied. The useful recovery affordance is finding one's bytes through Files'
existing Trash and Restore actions after Undo or an interrupted transfer.
Adding more menus before those journeys worked would not solve either gap.

## Current mounted journeys

| Journey | Before | Implemented and tested behavior |
| --- | --- | --- |
| Select, Copy/Cut, switch Files window, Paste | Each window had its own path clipboard | An opaque offer names selected retained objects; the receiving record supplies its retained shown-folder descriptor. |
| Replace a selected name or shown destination, then Paste | Queued paths could adopt the replacement | Capture/transfer refuses identity changes and preserves protected bytes. |
| Paste, edit the destination, Undo | Physical transfers were disconnected from semantic history | Native compensation preserves the later bytes: Copy goes to standard Trash; Cut returns to the original source without replacement. |
| Undo, Redo, Undo again | Native Redo was unavailable | Canonical history queues the verified inverse and changes its settlement marker only after native compensation succeeds. |
| Cancel Cut or kill the process during transfer/Undo, restart, Recover | Private staging could leave users hunting for files | Durable receipts resume exact recorded phases under fresh grants; interrupted Cut restores its source and recoverable copies appear in Trash. |
| Withdraw a grant while two files are being pasted | A partial result could be reported as an unqualified success or lose history | The published subset retains a verified receipt and remains undoable; protected remaining sources stay intact. |

The mounted tests drive the production `CertifiedActionSession`, native session
broker and two production Files components. Semantic actions and keyboard
Copy/Paste/Undo reach the same effect handlers. An authenticated agent's transfer
can be compensated by a freshly admitted human selecting that author's history;
Undo uses the human's current authority. Fifteen actual offscreen captures show
before/Paste/Undo/Redo/Trash in light, dark and high-contrast themes. These are
mounted-component proofs, not compositor-input or packaged-app qualification.

## Authority and object custody

The service uses the existing authenticated collaboration lane. Offers and
tickets are bound to exact peer, record route and workspace; they neither grant
filesystem access nor delegate the publisher's role. The first slice is within
one workspace. Cross-workspace transfer refuses.

Files retains selected objects and the actual shown folder before acting.
Listing metadata is a stale-view precondition, not authority. Host capture
compares a descriptor duplicated from its retained process handle with a fresh,
authorized no-follow lookup and the supplied object identity. Selection refresh
cannot adopt a replacement under a still-selected name. Virtual views provide
no receiving-folder descriptor. Hosted refusal never falls back to the legacy
path clipboard.

The broker retains the original invocation's actual caller, cancellation state
and execution custody. Current role, workspace and source/receiving grants are
rechecked at bounded I/O boundaries, publication, source deletion and recovery.
Receipt JSON, invocation IDs and serialized actor fields cannot create authority.
No socket, sharing grant, permission override or path-only fallback is added.

SDK causal drains keep admitted document jobs alive until native compensation
settles. `EffectCx::reply_then` records a verified external receipt through its
original retained job; cancelled/failed replies cannot edit history. The
independent delayed-receipt primitive relanded and merged normally in
[3165](https://code.tilo.so/tilo/tilo/pulls/3165). Native Undo/Redo bookkeeping
remains pending while I/O runs; failed compensation leaves the original history
entry in effect. Restoring a grant and retrying can settle it truthfully.

## Durable recovery and resource budgets

Host receipts are versioned, bounded owner-private files beneath the persisted
workspace profile. A durable pending index avoids scanning all historical
receipts. Write-ahead phases precede physical publication and destructive I/O;
file and parent-directory synchronization precede settlement. Stored paths and
object seals are revalidated beneath freshly authorized folders.

Compensation runs on bounded host workers rather than the frame/RPC thread.
Same-filesystem renames use retained directories and no-replace publication.
Cross-filesystem compensation durably stages a complete copy before publishing
and quarantining/removing its exact source. Restart resumes the recorded move;
source/destination collisions or revoked authority preserve recovery material.
Real tests use two different filesystem devices and SIGKILL immediately after
compensation publication, then reopen the persisted profile and Recover.

Recoverable copies use standard owner-private home Trash files and `.trashinfo`
metadata. Files' existing Trash listing and Restore action discover and redeem
them. A cancelled Paste promptly retires its local poll state, allowing a fresh
Recover, while the host retains the interrupted receipt. Missing observations
fail rather than claiming successful completion.

Admission bounds four workers, 256 active operations and 4,096 reserved
descriptors; offers expire after ten minutes. Recursive copy/removal and byte
chunks are bounded. No background training, cloud service or additional
infrastructure is involved.

## Executing-process custody on the production entry path

The strict sandbox test uses the production Files manifest, CapabilityPlan,
`spawn_launch`, bubblewrap recipe and fully enforced Landlock. Its child opens
the inherited collaboration descriptor through the production SDK service hub,
joins two certified Files record routes, then completes Copy/Cut/Paste/Undo.
An ungranted host control file remains unreadable and its bytes remain intact.
The eight-case bridge suite includes this journey and stale-object, ticket,
record-route and current-role refusals.

Previously the launcher's `SO_PEERPIDFD` was used to resolve descriptor numbers
opened by the executing app. The first selected-file Capture failed with `Bad
file descriptor`. The repaired launcher handoff requests executing-peer custody;
the SDK completes one challenge on that same inherited stream before RPC starts.
The broker first admits the original launcher through its existing live launch
record and retains its original app/workspace and creation evidence. It then
checks the actual kernel sender credentials against one temporary connected
proof socket, obtains that exact task's kernel pidfd, and verifies its ancestry
under the same current recorded launch. A pathname, environment flag, claimed
PID or ancestry alone cannot authorize it.

The proof socket closes after the ready acknowledgment. Only the app and original
launcher pidfds remain, scoped to the live connection; either process's death
ends native descriptor/transfer custody even if a descendant retains the stream.
A single async death watcher owns both lifetimes. The handshake has a ten-second
bound and uses the existing reactor, with no additional worker pool. Malformed,
missing, excess, non-socket or foreign-creator rights are closed and refused;
there is no unverified fallback on a requested executing-peer handoff.
Normal Linux launches using this selector require the updated SDK service hub;
older packaged SDK/runtime clients are not qualified by the legacy-helper tests.
This is a startup protocol compatibility requirement for any eventual rollout.

The independent runtime service connection uses its existing raw RPC protocol,
which has no launcher QCL1 handshake. It clears an inherited executing-peer
selector rather than advertising a challenge it cannot perform. Real SDK
publication, sealed-write restart, joined-room key and child-process regressions
exercise that connection; the native manifested launcher still performs the
full executing-peer proof.

Focused real-process tests cover the actual child rather than the socket creator,
wrong launch start time/parent, an inherited foreign proof, wrong nonce/user,
malformed rights, exited handles and a restarted process. A real executing-task
death test keeps both the original launcher and a socket-retaining descendant
alive and verifies native custody is revoked. Original caller, current grants,
workspace membership, role, cancellation and resource seals still govern every
operation. No persistent trust, permission, service pathname, filesystem grant,
Landlock/ptrace setting or isolation policy changes.

The earlier authorization pause was an interpretation, not an automatic-review
rejection. The parent explicitly authorized this bounded source correction.
Keep the PR held until required current-head checks pass, then use the normal
protected lander.

## Native-entry qualification

The `files_clipboard_native` example packages Files through the normal SDK,
installs the signed app/runtime in an isolated profile, joins through the normal
signed invitation, waits for the published native authorship surface and invokes
Applications Open. It observes a presented Files primary frame and uses the
exact published record and semantic folder identity.

The earlier GoTo failure exposed session-consent loss during authorship
publication. After the bounded consent correction below, the real entrypoint
can navigate, select and Copy. A second gap was that the normal own-process
window still selected the legacy path clipboard. Its native service selection
now uses the existing canonical collaboration descriptor and remains fixed for
that window: losing the service cannot select a legacy fallback. Explicit
hosted service refusal remains a refusal; document opening services are intact.

The signed journey now passes actual pasted bytes, unchanged source bytes,
physical Undo retaining a later destination edit in standard Trash, keyboard
Redo restoring those edited bytes, and a subsequent terminal History receipt
with actual document versions. Physical compensation alone cannot pass this
proof. A stronger observer checks the receiving entry's appearance, withdrawal,
and exact identity after restoration; it observes the document publication
prefix before inspecting the reconciled surface. Packaged Trash listing and
Restore remain unqualified; the earlier three-theme proofs cover mounted
components.

The publication failure was a readiness race: a known committed record can be
refreshing when its original authorized Undo settlement enters the local queue.
The manager now retains a Document for an already-required committed route.
Hosted workers still require original invocation custody before admission;
ordinary signed app windows retain their existing certified publication policy.
It does not issue a redundant Join that can reset that read
gate. The existing bounded worker still waits at most three seconds for the
exact route, then validates the original custody and body before host commit.
Unknown routes, missing required hosted custody and legacy signed operations
retain their existing refusal. Queue or worker failure still blocks the terminal outcome.
The production SDK regression holds the gate closed and proves there is no
host commit, signed operation or completed barrier until readiness returns;
caller expiry, foreign-body and persistence refusal cases retain their checks.

Read-only snapshots from the initial failed run established the cause: before Open there is
no Files filesystem grant; after Open acknowledgment the existing OS policy has
created a `FsPath(home)` Write grant with `EndOfSession` expiry; by presentation
that grant is gone, leaving the signed app authorship and person membership.
`applications::grant_file_manager_home` adds it to `Workspace::capabilities`,
while `WorkspaceTree::project_native_room` replaces that list with admitted
signed capabilities when authorship completes. The launcher and clipboard host
correctly refuse access with that grant absent. No additional fixture grant or
assertion bypass is used to hide this failure.

The authorized correction retains only the exact OS-issued Files session home
capability already present in this workspace. An in-memory provenance ledger in
`NativeStateRuntime` binds it to the existing workspace, room, selected local
identity and certified device. Native projection verifies current selected
identity and admitted membership before carrying the original row forward. It
does not mint a replacement grant, widen its path or expiry, or adopt incoming
signed rows as local consent. Revocation records a tombstone shared by stale tree
clones. The ledger is not serialized, so reopening the session cannot reconstruct
this host grant from durable app authorship. Clipboard capture and every retained
filesystem boundary also check the current consent; original-caller identity is
checked before initiating native clipboard effects.

The real native membership-broker regression passes grant survival after Files
authorship, another app's refusal, and continued refusal after revocation and a
subsequent authorship publication. The session-reopen regression proves the ledger cannot reconstruct a grant;
an unattested local identity refuses the helper without leaving a usable home
grant. The native clipboard bridge rejects a different person's private home
consent while retaining explicit shared-directory grants. These tests pass.

Background Nearby discovery failure no longer interrupts ordinary file work
with a toast. It withdraws stale discovered targets; explicitly opening its
picker or sending still reports failure. Privacy policy and grants are unchanged.

## Verification and provenance

All Cargo compilation, tests and offscreen rendering use a Cargo drop-in on
the existing approved SSH runner with four jobs. Scoped logs distinguish passing
mounted/lifecycle tests from the strict sandbox and hostile-process proofs; screenshots do not imply
a live compositor, deployment, full image or hardware qualification.

Implementation uses the repository's AGPL-3.0-only code. Rendering reuses the
existing OS font catalog and its declared licenses in
`crates/sdk/ui-core/assets/fonts/manifest.toml` and `LICENSES/` (including
OFL-1.1, LPPL-1.3c and GPL-3.0-or-later WITH Font-exception-2.0); no font or other asset is introduced by this feature.
The existing rustix dependency declares Apache-2.0 with LLVM exception,
Apache-2.0 or MIT; its public ancillary-data APIs are used without copied code.
Cap'n Proto bindings use the workspace-pinned MIT-licensed `capnpc` and existing
compiler. No third-party code, assets or crate versions were added. The process binding
adds two internal runtime-portal dependency edges and declares the existing
rustix net feature. Cargo.lock changes only those two edges, without a resolve. No laptop builds, real-desktop capture, reboot,
deployment, infrastructure or spending is part of this change.
