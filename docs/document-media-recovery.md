# Legacy document-media recovery authorization

## Decision: explicit import is approval

An explicit import **through an existing authenticated real-person/device-owner
action is approval** to import that person's host legacy store into the selected
authorized destination. It does not need another hardware gesture, click proof,
witness, or consent service. An already witnessed canonical owner invocation of
that exact import, with its exact arguments, can also suffice; do not ask twice.
Reuse the existing identity and action authority, not a new trust root.

Missing historical provenance prevents claiming a verified ownership-preserving
migration. It does **not**, by itself, prevent a device owner from explicitly
authorizing a **new import** of their host legacy store. That decision establishes
new destination references; it need not retroactively prove who owned each old
room reference. Do not impose an additional historical source-ownership test on
this route. This is not permission to import arbitrary foreign stores or probe
resident bytes by guessed hash.

Conversely, an ordinary read, automatic migration, selected account, room
membership, mount ownership, matching hash, or newly written document is not an
explicit owner import decision. An agent's assertion that the person approved is
not that decision either.

## What is actually implemented

The following references describe this checkout, not a proposed implementation.
Line ranges are source locations at the time of this audit.

- `crates/system/fs/src/document_media.rs:52–73` defines room-scoped
  `tilo/media/v1/<room>/<hash>` tags in `reference_tag`, `references`, and
  `record_reference`. `DocumentMedia::read_range` (`165–180`) refuses reads
  without the exact reference. This is not account migration or an import permit.
- There is no typed legacy-media import action or media recovery permit wired
  into this production path. `RecoveryRequired`, `sourceLegacy`,
  `authenticated_with_legacy`, `legacy_media_snapshot`, and a
  `standalone/media_migration.rs` implementation must not be described as shipped
  interfaces here. Existing `Unavailable` behavior is unchanged by this document.
- Searches for `ActionPlan` and `trustFence` in `crates/` find no symbols. They
  are not names of a current authorization or execution layer.

### Admin control admits same-account agents, not only people

`crates/session/shell/src/compositor/admin_control.rs` explicitly says that an
agent over the admin's own login or ssh uses this socket (`1–28`). Its concrete
checks are:

1. `session_decision` (`85–98`) and `bind_at` (`124–141`) require administrator
   group membership and `active_identity_owns_device`. This authenticates which
   session owns the device, not which human caused each request.
2. `authorize` (`202–213`) obtains kernel peer credentials (`SO_PEERCRED` via
   `wayland_peer_credentials`), checks user/mount namespaces, and checks launched
   app ancestry. `peer_decision` (`188–199`) rejects other UIDs, sandboxed peers,
   and session-launched apps, including unconfined apps.
3. An unconfined same-account process outside that app ancestry passes. There is
   no person-versus-agent discriminator in `Peer`. Calling this check alone
   "authenticated real person" would be false if agents must be denied import.

The exact execution path is in
`crates/session/shell/src/compositor/native.rs`: the accept loop (`14013–14035`)
calls `authorize` then `TestControlConn::admin` (`3046` onwards).
`pump_test_control_conn` (`6162` onwards) dispatches the request. Its agent
selection (`6251–6257`) takes the optional request label or defaults only for an
agent connection; an admin process can omit that label. The owner-only special
branches (`6258–6318`) therefore do not prove a real person either. Their
`conn.admin && agent.is_none()` check is a connection/request classification,
not independent evidence that an unconfined agent is absent.

For ordinary actions, `actions_invoke` in
`crates/session/shell/src/shell/test_control.rs:1754–1779` resolves an address and
produces `PlannedAction::InvokeAction`. `execute_invoke_action` in
`native.rs:4878–4964` selects the gated catalogue action and calls
`invoke_json_disposition`, rebinding the supplied arguments. `with_action_gate`
(`native.rs:53–75`) builds `PolicyVisibility` from the active workspace's local
person and policy snapshot. That is a capability gate, not human-presence proof.
Do not use this entire transport as an agent-denying import approval boundary.

### Existing caller custody, envelopes, and confirmation

These are the actual relevant fences; none is a media permit today:

- `crates/session/shell/src/shell/remote_actions.rs:62–85`,
  `BrokerActionHost::invoke_local_person_job`, calls the broker's
  `action_invoke_local_person`. The only shell call site found for that wrapper
  is the test helper `invoke_native` in `shell/local_person.rs:131–142`; this
  audit does not establish that ordinary admin requests traverse it. In
  `crates/collab/tilo-action-broker/src/collab_broker_capnp/local_action_host.rs:20–102`,
  that function rejects delegated/foreign/non-person callers, obtains the host
  certificate, checks room/person/key bindings, and retains `NativePerson`
  invocation custody. `current_native_person` (`105–135`) verifies the current
  certificate and rejects identity replacement. If the native-person service is
  absent, the compatibility path supplies **no** such original certificate
  custody. Native-seat custody must not be interpreted as proof that the admin
  transport distinguished an unconfined agent from the person.
- `collab_broker_capnp/action_host.rs:1444–1522`, `action_invoke` and
  `action_invoke_with_origin`, check execution and route to directory, launchable,
  host-step, own-device, or eager actions. `may_act_in` / `decide_for`
  (`608–666`) consult current workspace authority. None is a registered media
  import action.
- `collab_broker_capnp.rs:1821–1838`, `InvocationEnvelope`, carries the wire id,
  invocation id, resource/action, argument, actor and host context. Original
  authority is retained separately, not supplied by an actor label.
  `collab_broker_capnp/published_invocation.rs:175–277`,
  `current_person_certificate` / `owner_person_certificate`, reject known agent
  admissions, check current authority, and bind original room, wire/invocation
  ids, resource/action and arguments. `check_custody` / `check_execution_frontier`
  (`484–550`) also check execution lifetime, target connection, certificate,
  pending invocation and cancellation. These bindings can inform an import
  implementation; they do not repair upstream caller misclassification.
- `collab_broker_capnp/action_host.rs:2528–2566`, `admit_answer`, accepts an
  authenticated direct person's `Confirmed` for Ask, or spends an exact witness.
  `collab_broker_capnp.rs:6224–6349`, `confirmation_token`,
  `mint_confirmation_grant`, `witness_confirmation`, and `take_witness`, bind
  short-lived one-use confirmations to actor, resource/action and argument digest
  (and dispatch grants to the target peer). `collab_broker_capnp/witness.rs:85–167`,
  `request_witness`, is an existing host-consent route for obtaining the person's
  answer. For a typed owner import, an applicable canonical answer is sufficient
  approval; witnessing is an alternative when needed, not an extra mandatory step.
- `crates/collab/actions/src/envelope.rs:17–49`, `ActionEnvelope`, is replay
  metadata: action, heads, timestamp, locus, actor and context. Human provenance
  in context explicitly does not change mutation authority. Separately,
  `ActionContract::carries_untrusted_content`
  (`crates/collab/actions/src/contract.rs:538–542`) reads `content:untrusted`;
  the broker records that on jobs (`collab_broker_capnp.rs:5937–5943`) and exposes
  `untrusted_job` (`6351–6356`). This is content classification, not an owner
  import capability or a fictional `trustFence` envelope field.

Broker paths above are relative to
`crates/collab/tilo-action-broker/src/` unless otherwise specified.

## Ordinary migration versus deliberate import

**Ordinary migration** preserves an existing ownership claim without asking for
a new one. It needs retained authenticated evidence binding the original account,
room and exact hash. A future migration must freeze that evidence before apps can
write new references; later documents cannot manufacture old provenance. Missing
snapshots, keys or recognized binary-reference models may block automatic
migration. Preserve old references and their GC roots rather than guessing.

**Deliberate explicit import** is a new owner-authorized operation. Missing old
provenance is its motivation, not an automatic refusal. Resolve a bounded
selection from the owner's host legacy store, present/describe it as a new import,
and bind source store/room/references, exact hashes, destination and limits to
that exact action's canonical arguments. Authenticate the approving owner and
check destination authority using existing host authority. No additional proof
of historical ownership of those selected legacy references is required.

## Exact missing capability and proposed integration

The current blockers are (1) no typed media-import action/backend authorization
handoff, and (2) no real-person-versus-unconfined-agent distinction at the admin
peer boundary if import must deny agent self-approval. They are **not** absent
historical provenance, a missing hardware witness, or a mandatory new consent
service. A caller-controlled `Person`, `confirm: true`, missing `agent` label,
or arbitrary signed bytes cannot fill those gaps.

Implement a typed action at the existing canonical owner boundary. It may consume
an already authenticated owner invocation or an already witnessed canonical
owner answer for that exact import. Where caller custody is ambiguous, use an
existing authenticated approval route rather than silently treating admin socket
access as human approval. Do not require a second approval once the exact owner
action has already been authorized.

Only then hand a bounded, private authorization to the media backend: an opaque
host-held handle can suffice in-process. Bind source selection, destination,
approving account/device, exact argument digest, invocation/idempotency key,
nonce and expiry. Capture the applicable current authority revision as the trust
fence, and revalidate ownership, enrollment and revocation at consumption; reject
an invalidated fence rather than trusting the mint-time decision. Enforce item
and byte limits. If transported, use the existing authenticated authority and
invocation binding, not a media self-signing trust root. Reject changed nonces,
expired permits, changed arguments and unauthorized replay. Durable outcome recording and
crash-safe reference publication are implementation work, not guarantees supplied
by a short-lived confirmation token. Keep old tags/bytes GC-rooted on failure and
success. Import must not expand the frozen automatic-migration evidence inventory.

If recovery is exposed later, return a bounded reason and an actually registered
next action to authorized callers. Until an import exists, say it is unavailable;
do not advertise a fictitious action or leak foreign-hash existence, paths,
store inventories or bytes. Immutable source metadata must remain distinct from
the proposed destination, including after refusal or restart. Claim preserved
bytes only when they are actually held durably, not merely referenced.

## Existing production tests to reuse or extend

These tests exercise real existing boundaries, **not** an implemented permit:

- `crates/session/shell/src/compositor/admin_control.rs:278–339`:
  `only_the_device_admins_session_serves_admin_control`,
  `only_the_admins_own_unconfined_processes_connect`,
  `a_connection_from_this_account_is_authorised_by_its_credentials`, and
  `a_refused_client_reads_why_in_the_protocol_shape`.
- `crates/session/shell/src/compositor/native.rs:17965–18252`:
  `the_admin_socket_serves_the_action_model_and_refuses_the_rest`,
  `owner_socket_publishes_and_submits_locked_session_authentication`, and
  `session_diagnostics_require_the_unlocked_authenticated_owner_connection`.
  Their owner-connection checks must not be reported as excluding an unlabeled,
  unconfined same-account agent.
- `crates/collab/tilo-action-broker/src/collab_broker_capnp/action_host/own_device.rs`:
  `own_device_refuses_every_caller_but_the_person_here` and
  `own_device_sends_once_per_invocation_and_follows_the_other_device` test
  classified caller exclusion and invocation replay, not admin peer classification.
- `crates/collab/tilo-action-broker/src/collab_broker_capnp.rs:19751`:
  `invocation_journal_confirmation_uuid_and_lifecycle_remain_authoritative`.
- `crates/system/fs/src/document_media/tests.rs:10–145`:
  `stored_media_is_readable_only_by_reference_bounded_and_durable` and
  `ranged_reads_after_a_restart_read_only_the_range`.

A future import needs production-path coverage for owner-authorized import with
missing historical provenance succeeding; existing exact approval needing no
second witness; wrong-account and agent self-approval failing even without a
request label; changed source/destination/arguments or nonce; expiry and revoked
authority; replay; crash recovery; and old
GC-root preservation. Those are required future tests, not tests present or run
for this documentation-only change.
