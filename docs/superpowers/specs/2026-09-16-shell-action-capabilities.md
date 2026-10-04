# Shell capabilities through the shared Action model

Status: approved in conversation on 2026-09-16. The user approved the phased
design, then explicitly requested implementation by GPT-5.6 Sol at high
reasoning, using only this specification. The implementer owns implementation
design and may discover and resolve related issues; a prescriptive plan is not
required.

## Outcome

Improve the shell's useful capabilities by making its existing commands and
newly exposed operations use the shared Action model consistently. People,
scripts and agents must be able to discover an operation, supply typed
arguments, invoke the intended target with current authority, and receive a
truthful outcome. Maximize useful complete workflows rather than action count.

The approved scope has three parts: correct shell invocation and contracts;
explicit document targeting and typed Quick Settings operations; and edited
image content handed from Photos to Writer through the existing runtime.
All three remain in scope even if implemented and reviewed incrementally.

## Shared architecture

Keep the same application/session/dispatch core across the shell, standalone
applications and headless execution. Reuse canonical ActionSpec, ActionContract,
typed arguments, bound dispatch, broker authority and invocation lifecycle.
Do not introduce a second workflow engine, per-app automation server, or an
OS-specific action interpreter.

Document operations deterministically mutate shared CRDT state. View operations
change local presentation. Effects perform host operations. External I/O must
not enter deterministic document replay. Platform providers supply mechanisms;
applications own domain operations; the runtime owns routing and authority.

## 1. Truthful shell invocation and contracts

Every invocation entry point must honor asynchronous admission. Starting an
operation and waiting for its admission are distinct from successful completion.
JSON/socket callers must not receive synthetic success merely because a legacy
fallback callback returned without performing the requested operation.

Pending admission must retain its ticket without blocking the event loop or
starting the operation again on each poll. Preserve request/response ordering,
propagate rejection and infrastructure failure, and retain the broker's actual
invocation identity and lifecycle when admission succeeds. Disconnects,
timeouts and delayed responses must not fabricate completion or automatically
repeat an operation with an ambiguous outcome.

Shell-native commands must expose complete and accurate operational contracts,
using the same definitions as app actions. Contracts must be enforced, not just
displayed. Declare appropriate permission, availability, operation kind, risk,
confirmation, retry, execution and result semantics. In particular, permanent
device revocation requires explicit confirmation and administrative authority.
Pure overlay/navigation operations remain local View operations. A queued
request must not claim the downstream effect has already completed.

Actual failures must reach the caller. In particular, failure to duplicate a
tab must not become a successful invocation accompanied only by a notification.
Preserve useful user-facing error feedback as well as machine-readable failure.

Discovery and execution must use current permission/availability, and existing
capability restrictions must not be weakened. Derive policy from the canonical
contract wherever feasible instead of maintaining divergent identifier lists.

## 2. Addressed edits and callable Quick Settings

An external text edit must be bound to an explicit document/resource target.
Changing focus after discovery must not redirect it to another document.
Reject stale, inaccessible, non-text or otherwise invalid targets without
mutating a different record. Use canonical app/record dispatch and attribution
where the target is owned by that model; do not create a new parallel storage
or undo route. Preserve user-facing focused editing as appropriate without
leaving externally targeted edits dependent on ambient focus.

Expose the existing brightness, volume, Wi-Fi enabled state, power profile and
named VPN enabled state through typed Effect operations. UI controls and
external invocation must share the same operation behavior and provider.
Arguments must enforce ranges and choices before dispatch. Provider errors and
unavailable services must be returned honestly; unsupported hosts must not
silently use ambient fallback authority. These are host/device effects, not
shared CRDT edits. Declare retry and reversibility only when the actual
operation justifies them. Secret-bearing Wi-Fi join is not required in this
slice; do not expand scope just to add it.

## 3. Edited image content handoff

Keep Photos rotation/crop as deterministic Document operations. Add a typed
Effect that produces the currently addressed image's edited content. Render
from a well-defined snapshot of its source and edit state; reject missing,
stale or inaccessible inputs. Return exactly one canonical tilo.content:<hash>
resource only after the corresponding immutable encoded bytes have been
successfully stored and can be retrieved through the configured content
provider. Do not return the original image hash as evidence of transformed
content. Declare the content resource result using the existing ResultSpec.

Use the existing immediate-next-step content handoff to feed Writer's
place-image operation. The producer's actual result replaces model-invented
content arguments. Missing, ambiguous, wrong-kind or unavailable content must
stop the operation; an intervening step must not retain an older handoff.
A content address is not a grant: producer and consumer invocation authority
and content-provider access remain separate checks.

Preserve the distinction between live invocation output and durable effect
receipts, which deliberately redact returned content. Do not claim exactly-once
external effects, unsupported invocation-ID retry, power-loss durability,
cross-sandbox isolation or Windows portability based on a same-user Unix proof.

## Acceptance evidence

Use meaningful regressions against actual dispatch paths, not tests that only
duplicate metadata construction. At minimum establish:

- JSON asynchronous invocation begins once, stays pending until admission,
  returns the genuine receipt, and exposes rejected/failed admission.
- Socket handling continues to make progress while admission is pending and
  preserves framing/order without fabricated success.
- Shell commands expose valid contracts; consequential operations refuse before
  effects without required authority/confirmation; duplicate failure propagates.
- Two text documents with changed focus: only the explicitly addressed record
  changes. Invalid/stale/denied targets change neither record.
- Quick Settings UI and external paths reach the same dispatcher with exactly
  the validated arguments; invalid values never dispatch and service failures
  reach callers. Use a controlled provider, not the user's actual hardware.
- An asymmetric image fixture proves transformed pixels and stored hash/bytes;
  Writer receives that exact produced content through typed invocation.
- Denied producer/consumer, missing/ambiguous output, unavailable bytes and
  intervening-step handoff invalidation are covered.
- Production broker validation exercises native and headless paths as
  applicable, including instance targeting and current grants. Document
  signatures and resulting records must be independently checked where the
  existing shared-runtime proof provides that facility.

Run focused tests while developing and the required affected build/test checks
before claiming completion. Linux-only compositor changes require Linux
qualification; a macOS check cannot validate cfg-excluded code. Report exact
commands, results and remaining limitations. Do not weaken assertions or
introduce silent test skips to obtain a pass.

## Coordination and workspace boundaries

Do not change Cargo target directories or broadly format unrelated
files. Record owned files, commits, discoveries and test results in a report.

The task "Plan cross-platform framework" owns ongoing renderer/performance work and
has agreed to runtime-side validation for the content-handoff slice once its
contract is established. Coordinate via the controller before editing its
active UI/Writer/performance files or shared runtime proof files. Photos files
were not owned there at the time of agreement. The controller owns coordination
and independent review; implementation subagents must not spawn other agents.

Resolve related issues found during implementation when necessary to make these
outcomes true; explain changes in the report. Escalate genuinely incompatible
architecture or ownership conflicts, not routine implementation choices.

## Investigation entry points (not an implementation prescription)

Relevant current sources include shell/src/shell/search/action_api.rs,
shell/src/shell/search/providers/actions.rs, its text_insert module,
shell/src/shell/search/providers/live_app_actions.rs,
shell/src/compositor/native.rs, shell/src/shell/overlay_host.rs,
shell/src/shell/qs_actions.rs, apps/photos/src/actions.rs and app.rs,
crates/collab/actions/src/{contract,outcome}.rs,
apps/agent/src/session.rs, and docs/shared-runtime-proof.md.

The prior read-only audit found JSON invocation bypassing on_begin_invoke,
contract-less shell rows, tab duplication swallowing errors, focus-dependent
text insertion, and empty Photos effect outputs. Revalidate these against the
current tree; the implementation design is yours to determine.
