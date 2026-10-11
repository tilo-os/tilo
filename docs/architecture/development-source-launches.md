# Joined development launches: proposed authorization

Status: design for review. There is no implemented joined native development
launch grant. Native registration still refuses another workspace. This proposal
does not authorize a real grant, change membership or activate an exception.

The compatibility repair is independent: uninstalled SDK clients keep legacy
collaboration, an explicitly started development host can supply separate
executing-source evidence in its own workspace, and installed packages keep
their original signed declarations. The original joined SDK journey uses legacy
transport. Neither that journey nor certified joined membership establishes an
uninstalled executable's native launch authority.

## What the owner would approve

The developer selects an already joined room and existing records, then asks
the trusted platform host to prepare a development launch. Preparation creates
no grant and releases no document or identity descriptor to the child. It holds
the actual child at the existing launch gate while the platform obtains consent.

The local profile owner approves running this exact development build on this
device. The room's governing owner separately approves exposing the selected
records to it. If the same authenticated person holds both authorities, the
platform can present one combined prompt. A member's Editor role does not make
them an owner. A saved certificate, same-UID socket, environment flag, app label
or caller-supplied confirmation boolean supplies neither consent.

Proposed platform prompt (values come from retained host evidence):

> Allow this development build in **{workspace name}**?
>
> **Build:** {app id}, {canonical executable path}, {full executable digest}
>
> **Device and profile:** {device name}, {profile name}, {selected person}
>
> **Records:** {explicit existing record names and stable IDs}
>
> **Access:** {Read only | Read and edit}, within this person's current room
> permissions. Reading includes all contents and history of those records.
>
> **Duration:** this one launch, at most 30 minutes. Ending the app, restarting
> the host, revoking access or replacing the executable ends this authorization.
>
> This does not approve an installed package, create records, change room
> membership, or grant filesystem, hardware or production service access.
>
> **Allow this launch** / **Cancel**

For a different workspace owner, show the same bound request through the
existing authenticated semantic action surface and platform confirmation. This
is a proposed request/approval action, not an existing callable API. Do not send
a message or create a pending approval in someone's real workspace as part of
design or testing. A command-line or agent consumer can inspect the same exact
scope and invoke the semantic action after action-specific authorization; it
cannot approve itself by adding fields to a launch request.

## Scope retained by the host

| Binding | Required scope |
| --- | --- |
| Host | Explicit development startup; private, owner-validated profile; fresh host-instance identity and held profile lock. A pathname alone is insufficient. |
| Person and device | Exact currently selected principal and device, original host-held certificate and joined enrollment/governor continuity. No keys or certificate custody are transferred to the app. |
| Room approval | Current governing owner approves the complete request digest through authenticated governance authority. Local profile ownership cannot approve a foreign room. |
| Code | One bounded verified executable digest, separate `DevelopmentSource`, app id and immutable launch configuration. An installed package cannot take this path. A path or mutable checkout is not a code identity. |
| Process | Actual launcher and executing PID/start identities, retained pidfds and the existing executing-peer challenge. No wire PID, creator-only handoff or unrelated same-UID task can redeem the binding. |
| Data | One canonical room and app; at most 64 explicitly selected existing record IDs. No wildcard, future record, foreign-room bootstrap or missing-record creation. Child-resource labels do not narrow whole-document reads. |
| Operations | Read or Read-and-edit ceiling, intersected with current governor/device/person/record/action policy. No Owner/Admin, invitation, hardware, filesystem, production service or arbitrary command authority. Effects need their existing authorization independently. |
| Lifetime | Single prepared process; approval request expires after two minutes; approval expires no later than 30 minutes after issuance. Also ends on process death, source replacement, profile/identity/enrollment replacement, host restart, consent revocation or loss of the live authority session. |

The approval is a new, temporary launch authority, not an inferred existing
grant. Its exact signed/request-bound representation needs a separate review.
Keep approval material and the active binding host-held, never in an app's CRDT,
environment or transferable bearer descriptor. It is not replicated as an
Always/Workspace grant or restored after restart. Replaying an approval after a
host restart or against another prepared process must fail.

The governing service retains the active approval only for its lifetime and
provides revocation through the same semantic surface. An opaque local binding
ID is a reference, not authority. Disconnect closes the binding; it cannot run
offline while waiting for a revocation message. Use a monotonic local deadline
as well as the bounded issuance expiry; wall-clock rollback cannot extend it.
Unavailable or uncertain expiry validation fails closed.

## Checks, revocation and races

1. Capture the proposed source/configuration and actual held child in the
   trusted launch supervisor. Validate confinement before requesting consent.
2. Bind both approvals to that entire immutable request, selected profile,
   room and prepared process. Register a revocable execution lease before the
   second custody read, as the existing execution admission does.
3. Recheck original source, process, owner approvals, profile/identity/enrollment
   and current permissions before releasing the launch gate. Cancelled,
   replaced, expired or superseded preparations release no app descriptors.
4. Every native read, history chunk, semantic action and mutation uses that
   binding. Directory discovery filters to its exact app/records. Read capture
   checks before and after, and every subsequent chunk checks again.
5. Durable writes validate the binding and current governor role at the
   canonical owner's commit boundary. A previously discovered action or queued
   request does not retain Write after a downgrade. The source/lease fence and
   commit decision need one defined ordering, not a permission check followed
   by an unguarded asynchronous write.
6. Revocation synchronously closes the owned lease and gates new operations.
   The revocation result reports completion only after retained reads, queued
   effects and owned execution have been fenced. Other app/room processes stay
   unaffected. Already committed history remains attributed; it is not deleted.

An Editor-to-Viewer change withdraws Write immediately; Read may continue only
if current policy and this binding's ceiling still allow it. Removing Read or
membership closes reads, including the next chunk of an in-progress capture.
Restored membership cannot revive a retired binding. A role increase cannot
exceed the originally approved operation ceiling.

An executable change retires the binding even if PID and app id stay the same.
Another process with the same digest cannot inherit it. A rebuild requires a
fresh launch and consent; there is no approval for an executable path or all
future builds. In-process UI reload that keeps the approved executable may
retain the lease, but an exec-based hot reload may not. A smoother future build
approval is a separate product/security decision, not an implicit exception.

Two implementation gates remain unresolved:

- **Confinement:** the existing workspace sandbox must confine the uninstalled
  development child without manufacturing installed-package evidence or
  weakening signature, Landlock, portal or service boundaries. If the current
  launcher cannot do that, the request is refused; owner consent is not a
  sandbox bypass.
- **Exec/commit ordering:** before/after `/proc/PID/exe` stamps detect
  replacement but do not alone prove that a concurrent exec cannot race a
  durable commit. A supervised exec fence or confinement that forbids further
  exec after startup needs concrete kernel-path evidence and review. The
  implementation cannot claim race-free custody from repeated stamp reads.

Do not activate the proposed authority until both gates are demonstrated.

## Disposable acceptance cases

Use two isolated profiles, disposable identities and an existing governed room;
never create or approve a grant against real rooms for these tests.

| Case | Causal outcome required |
| --- | --- |
| Explicit positive | Both actual owners approve the exact held build/process and existing records; native read/edit converges and survives owner restart as ordinary attributed document history. Launch approval itself does not survive. |
| Missing consent | Joined Editor, same-UID impostor, spoofed profile/PID or client `approved=true` is refused without document/registry mutation. Test each owner approval missing independently. |
| Production | Normal host, installed package, forged development environment, and production service requests refuse this path. Original signed package declarations remain narrow. |
| Profile and room confusion | Another host instance/profile/identity, valid unjoined UUID, another app, record, room or process cannot redeem or reuse an approved request. |
| Source replacement | Change the executable while preparing, between approval and gate release, and during an operation; live same-PID exec and a same-digest unrelated process lose or lack authority. |
| Concurrent downgrade | Pause a real write at the canonical commit boundary, downgrade to Viewer, resume: no mutation commits. Read remains only if currently allowed. |
| Revocation and expiry | Revoke during multi-chunk capture or a queued action; the next chunk/action fails. Test host restart, process exit, owner disconnect, clock rollback and actual deadline expiry. |
| Creation and legacy | Missing foreign records/bootstrap are refused; all three original SDK journeys remain unchanged. A legacy positive cannot substitute for the native positive. |

Current compatibility/refusal evidence is separate from these proposed tests.
It proves legacy publication and sealed writes, own-room native durable
creation, signed daemon attachment, actual joined membership refusing native
foreign registration, kernel caller/app/room isolation and source retirement.
It does not prove any new owner-approved joined native launch.

See [security](security.md#capability-grants) for existing owner decisions and
revocation, [collaboration](collaboration.md) for governor authority, and
[`mcp_admission.rs`](../../crates/runtime/tilo-runtime/src/standalone/mcp_admission.rs)
for the existing room-selected custody and execution lease. These are reuse
points; none already implements this new launch authorization.
