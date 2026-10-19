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
recovery after the argument-bearing request has been discarded. New private
openings also bind the journal's exact retry request digest, including whether
a request was present. Recovery compares this signed private binding with the
stored digest before accepting the row; changing only the stored digest cannot
substitute the accepted retry. The digest is not added to the public receipt.

Journal format 7 stores evidence in the original admission append, using its
existing data barrier. There is no second evidence file or additional fsync.
Older records stay readable and do not acquire synthetic attestations. Openings
written before retry-digest binding keep their original commitment encoding;
they cannot authenticate the separately stored request digest retroactively.
Removing the binding from a new opening invalidates its signed commitment. Host
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

## Speculative SDK-local framework actions

The framework captures local typed executions at `ActionHost` and retained
resource dispatch, including immediate, queued and grouped edits, registered
View callbacks and registered semantic Read projections. A route-local,
bounded queue preserves the original published invocation, canonical encoded
input, original heads, exact resulting change hashes and context. Invocation
identity is separate from undo coalescing. Reads and no-ops retain empty change
sets. Background model work, presence and broker-delivered continuations do not
become fresh local-user actions.

Before execution, the application pins its published directory snapshot and
component aliases. `resolveLocalDocumentAction` resolves that retained snapshot
only for the local-report admission path; ordinary invocation resolution still
rejects stale revisions. Snapshot leases and queue capacity survive asynchronous
report submission, including uncertain responses. Oversized reports and queue
saturation refuse visibly rather than blocking on the broker. There is no
synchronous signing, key loading, RPC or journal barrier in input dispatch.

Losing the signing transport retains the last authenticated published snapshot,
not a new identity. Exact pending reports and their retained directory rows use
existing private checkpoint metadata for reconnect and restart; retries never
execute callbacks again. Pending, refused and unsigned history are visible.
A device without internet can still use its connected local broker. A fresh
unmanaged development session can edit, but has explicitly unsigned history.
History is unsigned only once an edit is actually recorded without an
authenticated publication: input that changes no document (scrolling,
selecting, hovering, moving focus) touches no history state, shows nothing and
asks for no frame. The status appears once, with that first edit, in the
existing History bar.

`admitLocalDocumentAction` authenticates the existing SDK channel, revalidates
its original worker and route, resolves the exact published action/revision,
and uses the central policy and evidence journal without delivering the action
again. Its private commitment includes the complete typed report and bound
arguments. Encoded input, context and mutation hashes are explicitly
**application-reported**; they are not `verify_change` execution proofs. The
receipt says the host admitted the report after speculative application, not
that it observed consent, replayed the action or certified successful execution.
Confirmation-required actions fail closed rather than synthesizing consent.

The application retains a bounded private association between captures and the
authenticated admission response; eviction counts are exposed. These values are
not placed in public progress or presence streams. Receipt consumers must still
verify the host certificate and commitment using the evidence verifier.

This admission does **not** authorize certified document publication.
`send_document_mutation` continues to refuse `DocumentAuthorization` until the
actual final owner validates the parsed causal range, original contributors and
surface. No local receipt is converted into that authority. Thus local reports
have admission and app-reported mutation binding, but the existing SDK shovel
still cannot publish scoped mutations as certified execution evidence. Generic
legacy dispatch without a retained published invocation refuses observation
rather than guessing an action identity.

## Coverage and remaining entrypoints

Covered by published application admission or the native host admission barrier:

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
- Native host operations, including shortcut runs, host steps, own-device
  actions, application launch/close and workspace-open operations. These use
  `admit_host_action` (or its retained-start variant) before invoking services;
  observed-launch completions retain the original certified owner and lifecycle.
  Native workspace services and task launches use the same admission foundation.
  This coverage depends on original certificate custody, not a displayed actor
  label.
- The shell Jobs projection opens the maintained certified session and retains
  its owner-aware lifecycle handle for listing, status and cancellation. Every
  operation revalidates the original certificate and execution. A row must
  belong to the session's room as well as its exact root/device/agent owner;
  another device with the same displayed Person cannot observe or cancel it.
  Existing actor-only rows remain reachable through an explicitly uncertified
  compatibility lifecycle while their host dispatch paths are migrated. That
  lifecycle cannot observe or cancel certificate-owned jobs and does not turn
  a legacy actor label into signing authority.

The following are **not** claimed to have this coverage:

- `InvocationOwner::Local`: actor-string-only legacy/system dispatch. These rows
  remain explicitly uncertified. `action_invoke` without original custody,
  `start_invocation_confirmed_for`, `start_invocation_from_offered`, and legacy
  bridge starts cannot manufacture a certified user from an actor label.
- Explicitly identity-free, pre-identity connections may retain an uncertified
  local owner when neither the native-person service nor certificate authority
  is configured. A missing or revoked certificate on an identity-bearing host
  fails closed; it does not become a bootstrap exception.
- Portable web hosts do not provide the native broker's signing and durable
  admission journal. Without a certified Tilo session, typed web execution cannot
  claim a Tilo user; it must not mint a substitute web key. Local execution there
  is not native host admission evidence.
- UI edits without a retained published invocation, direct shell commands, and
  installer/greeter console actions. They need their semantic admission doors;
  input-event logging is not a substitute. Typed framework actions with retained
  publication are covered by the speculative local-report path above.
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
