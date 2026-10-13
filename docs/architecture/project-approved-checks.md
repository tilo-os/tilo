# Approved project checks

A Git repository exports `run-approved-check` as a cancellable effect job. Its
only argument is `check_id`. It returns bounded execution metadata and untrusted
UTF-8 diagnostics, not a grading verdict. Commands, environment, images, source
paths and limits are absent from the public action vocabulary.

The native compositor can load trusted definitions with
`--approved-checks /absolute/private/checks.json` before admitting applications or
tasks. The default is an empty registry. The configuration must be an owned
private regular file; links, duplicate flags, duplicate fields, unsupported
versions and input above 8 MiB fail startup. Definitions remain immutable for the
session. This first registration interface is a trusted host startup option;
owner GUI registration is not implemented.

The closed JSON vocabulary is `version: 1` and `projects`, with at most 32 project
entries. Each entry has `app_id`, a typed `resource_identity` (`kind` and `key`),
`artifacts` and `checks`. Both identity fields must be valid; the fixed check
binding resource equals the semantic key. Each check uses the
existing `ush::approved_checks::ApprovedCheck` registration: a fixed `id`, full
`binding` (`workspace`, host device public key, canonical `record`, semantic
resource key), exact `checkout`, dedicated `build`, private `scratch`,
`runtime_binary`, resolved immutable `images`, fixed `command`, fixed allowed
`environment`, and aggregate `limits`. A project's checks must share its full
binding. The host supplies existing verified image materializations; this loader
does not fetch images or authenticate arbitrary directory contents by merely
accepting an image digest label.

Source, writable build output and scratch are separate. The source has an empty
`target` mount point; ush mounts the dedicated writable build directory there.
Never set `CARGO_TARGET_DIR` or share a target directory between worktrees.
Private receipt artifacts must be an exact owned private directory outside both
source and build. Grader source must remain outside the evaluated agent's
accessible resources. Loading a private configuration does not by itself create
that source/access boundary.

The SDK advertises its public operation surface and directory only after the
actual bound document route and every live owner route have applied their
initial committed history. A joined channel alone does not satisfy this gate.
Directory query and resolution use the same check; an already admitted delivery
whose original history was retired runs no handler and disconnects as uncertain.
Unrelated records outside the live owner map do not participate in this gate.
Ordinary authenticated transports without committed reads retain their existing
explicit no-read semantics; a native broker with no acknowledged route stays
closed.
The terminal still requires queue admission and durable publication of all
changed live owners; startup readiness does not replace that later barrier.

The SDK uses the existing authenticated record channel. A bounded lane admits
at most two queued requests per record; duplicate live route registrations
refuse ambiguity. Route retirement drops its registration and aborts that lane.
`EffectCx::approved_check` preserves required private-worker original custody.
For ordinary native effects the existing bound dispatcher captures optional inert
route and action-address facts from the selected record owner. These facts select
that exact acknowledged SDK channel; they grant no authority. The existing
broker effect-dispatch path supplies the retained invocation. A detached local
effect, unavailable named project route, foreign record or retired registration
refuses. The helper returns a Send future for the existing effect worker, opens
no second connection and accepts no caller-supplied identity. App-primary routes
with no canonical named record do not satisfy a named project registration.

At the broker, a request's invocation UUID and check ID are correlation data.
The broker must recover exactly one retained nonterminal target invocation,
match the accepted action, typed argument and cancellable Write contract, and
retain the actual registered worker or native launcher/process connection.
Target custody is distinct from the original caller's authority. Both remain
subject to current certificate, membership, policy and cancellation checks.
A bounded per-invocation ledger refuses concurrent retries and retains completed
results or uncertain failures; an adapter panic cannot make execution retryable.
Host work runs outside the Cap'n Proto LocalSet.

The broker retains the authenticated published semantic kind and key alongside
its exact opaque action address. Native bound addresses contain app-lifetime
UUIDs and change when an app restarts. The stable identity selects the fixed
app/workspace/record registration; current live publication must still match the
retained semantic identity and exact target peer. Missing or changed identities,
a different key under the same kind, and retired original invocations refuse.
The public reply retains the original opaque address, rather than replacing it
with a semantic key. The private artifact binds both identities to the actual
supervision receipt.

The native adapter resolves that exact stable registration.
Before and throughout execution it checks original custody, the current enrolled
local device, and the committed room's exact host device. `ush` must observe the
existing FullyEnforced sandbox boundary and aggregate cgroup limits before guest
code starts. Missing enforcement or supervision observation fails. Existing
owned-unit cancellation and cleanup rules still apply.

Full supervision receipts and streams are written exclusively to private
content-addressed files and synced before a portable reply is emitted. Public
metadata includes their SHA-256, configuration/runtime/source hashes, observed
outcome, guest exit or signal, elapsed time and cleanup observation. Each public
stream preview is at most 2048 bytes with valid UTF-8 and identifies its full
captured stream hash. A nonzero guest exit remains a nonzero exit; this layer
does not translate it into a model score or infer test success.

Signed registered-worker and real kernel native-socket tests exercise accepted
effect custody and foreign-target, changed-ID, cancellation and retirement
refusals. Startup tests exercise actual private-file loading, cross-record
registrations and guest-visible artifact rejection. These checks do not establish
that an LLM caused a patch or that a complete evaluator run has succeeded.

## Reported phases on the original job

`EffectCx::approved_check_reported` attaches command reports to the original
background effect's `Progress::report`. Git uses this helper; the app remains the
only publisher of that action's `InvocationUpdate` sequences. Humans,
accessibility and agents observe the same existing job resource. Reports describe
what the command says it is doing; they never establish success, cleanup or a
grading result. Execution Activity keeps its lifecycle-only metadata.

A host may configure `reported_phases` on a fixed check, with at most 32 unique
`id`/`label` pairs. Labels are nonempty, control-free and bounded to 128 characters
and 512 UTF-8 bytes. Empty or absent configuration disables reports. The guest
receives `TILO_CHECK_REPORT_ID` and a separate informational
`TILO_CHECK_REPORT_TOKEN`; this nonce is never the private supervisor permit or
completion token. The fixed command may emit newline-framed stderr records:

```text
TILO_CHECK_REPORT_V1 {"token":"<informational nonce>","check_id":"<approved ID>","sequence":1,"phase_id":"synthesis","completed":1,"total":2}
```

Only the host's allowlisted label reaches job progress. Unknown fields or phases,
wrong correlation, nonconsecutive sequences, invalid counts, oversized frames
and capture-observation backpressure disable only the informational lane with a
closed reason. The same owned stderr capture remains byte-for-byte unchanged;
the same run still produces its independently observed terminal receipt.

The native channel appends `approvedCheckReportedProgress @60`, returning a
client-pulled capability with a bounded latest report and the existing terminal
receipt. Every report and read rechecks original custody; completion freezes
late reports. The capability shares the existing execution/replay registry and
creates no runner, scheduler, job resource or history store. The existing
`approvedCheck @58` is unchanged. New clients require a broker supporting @60;
unsupported brokers refuse without falling back to a second execution. Deliver
the broker, runtime client, SDK, Git application and registered ush supervisor
runtime together when enabling this path. Checks without reporting retain the
legacy private supervisor configuration format.
