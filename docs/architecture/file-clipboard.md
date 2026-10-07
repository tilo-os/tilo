# Local Files clipboard transfer boundary

Status: PR [3009](https://code.tilo.so/tilo/tilo/pulls/3009) remains draft with
`hold`. Mounted desktop journeys now work; the positive sandboxed-child proof
exposes a descriptor-process mismatch. Extending native executing-process
attestation awaits authorization under the user's security-change constraint.

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

## Confirmed remaining sandbox boundary

The positive sandbox test uses the production Files manifest, CapabilityPlan,
`spawn_launch`, bubblewrap recipe and fully enforced Landlock. Its child joins
certified record routes through launcher-created inherited service descriptors.
The first selected-file Capture then fails with `Bad file descriptor`.

The launcher creates that socket before spawning the sandbox. The broker's
`SO_PEERPIDFD` therefore pins the launcher, whereas Files opens its selected
object in the executing app process. `pidfd_getfd` resolves the child's descriptor
number in the wrong process. Same-process mounted/primitive tests cannot qualify
that boundary. Direct sandbox connections correctly lack launcher admission and
are not a workaround.

The proposed fix binds the actual executing sender through kernel credentials
and retained process custody under the exact admitted launch, preserving the
launcher-owned app/workspace and creation evidence. It must reject wrong launch,
room, image, closed/replaced descriptors, PID reuse, disconnect and revoked
grants. It must not trust an app-provided PID, role or environment identity,
infer authority merely from ancestry, or loosen Landlock/ptrace policy. This
security-boundary extension is pending authorization; the positive assertion is
not ignored or weakened. Keep the PR held until that journey and required
exact-head CI pass, then use the normal protected lander.

## Verification and provenance

All Cargo compilation, tests and offscreen rendering run through `rcargo` on
the existing approved SSH runner with four jobs. Scoped logs distinguish passing
mounted/lifecycle tests from the failing sandbox proof; screenshots do not imply
a live compositor, deployment, full image or hardware qualification.

Implementation and rendering use existing repository AGPL-3.0-only code/assets.
Cap'n Proto bindings use the workspace-pinned MIT-licensed `capnpc` and existing
compiler. No third-party code/assets/dependencies were added and this feature did
not regenerate Cargo.lock. No laptop builds, real-desktop capture, reboot,
deployment, infrastructure or spending is part of this change.
