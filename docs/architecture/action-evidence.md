# Semantic admission evidence

The native action broker signs semantic admission automatically in
`CapnpBrokerState::dispatch_invocation_from_offered_owned`. This includes reads,
no-op executions and calls made through the ordinary, non-durable invocation
RPC. It does not require an application to request evidence or write state.

An admission receipt is not an actor's signature, proof of human consent, proof
of execution, or proof that an external side effect happened. It is the host's
attestation of the authenticated original caller and the resolved invocation it
admitted. Verifiers must independently establish trust in the host certificate
for the named host and workspace. Certificate signature verification alone does
not establish that trust or historical revocation policy.

## Bound facts and private intent

`tilo_action_broker::action_evidence` defines the public receipts and verifier.
The domain-separated signature binds the host, workspace, invocation UUID,
original authenticated ownership, effective actor, application, record, resource,
resolved action and directory revision. A randomized commitment binds the whole
canonical admission intent, including exact arguments, original request where
available, contract and causal/action context. Distinct invocations cannot reuse
a receipt; exact retries still require the existing journal's replay checks.

Only the public receipt is shareable. Its private opening contains random nonce
material and intent digests, not plaintext arguments. The opening permits offline
intent guesses and must therefore remain private. The host-private journal
retains it; the existing authenticated, exact-owner observation RPC returns it
in `admissionJson`. Neither progress streams nor application invocation envelopes
receive it. Runtime observation debug output redacts evidence and result JSON.
A separate commitment component checks retained authorization/context fields on
recovery after the argument-bearing request has been discarded.

Journal format 7 stores evidence in the original admission append, using its
existing data barrier. There is no second evidence file or additional fsync.
Older records stay readable and do not acquire synthetic attestations. Host
signer custody is shared and cached under `current_device_stamp`; identity
selection, key, certificate, enrollment and revocation changes invalidate it.
Certificate time validity is checked on every signature. Authenticated eager
facade invocations are queued onto the existing bounded broker bridge rather
than signing or appending on the UI thread. Directory resolution already runs
on the broker LocalSet.

Terminal receipts reference the admission and classify only an observed success,
failure, cancellation or uncertainty. They share the existing terminal append.
If signing identity becomes unavailable, the outcome remains durable without a
fabricated terminal receipt. Recovery uncertainty is not retroactively certified
as success. Guarded-read private result reconstruction is not certified by this
terminal mechanism.

## Coverage and remaining entrypoints

Covered by central admission when they reach the published application dispatcher:

- Authenticated eager and resolved local RPCs, including ordinary non-durable
  invocations. Status and cancellation on the local RPC retain exact certificate
  ownership rather than recovering a person from an actor label.
- Durable peer and authenticated local invocations.
- Native task invocations, binding original caller root/device as well as the
  task, agent and run epoch.
- Certified local person/agent action hosts used by maintained native action
  sessions, and native-person origins retaining the original certificate.
- Eager application actions through those authenticated hosts; their admission
  runs on the broker bridge.
- The shell Jobs projection opens the maintained certified session and retains
  its owner-aware lifecycle handle for listing, status and cancellation. Every
  operation revalidates the original certificate and execution. A row must
  belong to the session's room as well as its exact root/device/agent owner;
  another device with the same displayed Person cannot observe or cancel it.

The following are **not** claimed to have this coverage:

- `InvocationOwner::Local`: actor-string-only legacy/system dispatch. These rows
  remain explicitly uncertified. `action_invoke` without original custody,
  `start_invocation_confirmed_for`, `start_invocation_from_offered`, and legacy
  bridge starts cannot manufacture a certified user from an actor label.
- Host operations handled before the published application dispatcher:
  `invoke_shortcut_run`, `invoke_host_step`, `invoke_own_device`, direct
  `launch_application`/`close_application`, workspace-open operations and their
  observed-launch completions. Their own admission/lifecycle needs original
  certificate custody before it can use this evidence foundation.
- UI widget edits that never invoke a published resource action, direct shell
  commands, and installer/greeter console actions. They need migration through
  their semantic admission doors; input-event logging is not a substitute.
- The legacy shell `BrokerActionHost::invoke` facade and compatibility native
  hosts without a local-person service, when they supply only a `Caller` label.
  Maintained certified action sessions are distinct from this compatibility path.
- Remote action-host `InvocationState` and public progress streams do not carry
  private openings. The retained local observation RPC exposes evidence; peer
  transports and higher-level SDK adapters need equivalent authorized evidence
  retrieval if they require offline verification.
- Typed external-signing and external-moderation services are separate consumers
  of original invocation custody, not implemented by this admission mechanism.

This is a semantic admission foundation, not a claim that every human
interaction on the desktop has signed evidence.
