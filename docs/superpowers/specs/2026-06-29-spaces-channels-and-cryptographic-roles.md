# Spaces, channels, and cryptographic roles

**Date:** 2026-06-29
**Status:** Design accepted; build sequenced over ~5 months
**Replaces:** the flat `Workspace { collaborators }` model and the advisory `Role` enum

---

## Problem

The current `Workspace` does four jobs at once: container for artifacts, collaborator roster, permission boundary, sandbox scope. In a real organisation these four don't coincide. The people who can see `report.pdf` aren't the people you usually work with. Your clearance is a property you carry across rooms, not something each room re-grants. People work in **stable formations** day-to-day (a team, a project) and occasionally do **one-off shares** with strangers; the model has to support both without making either feel bolted on.

Compounding that: the current `Role` enum is *advisory only*. The local shell paints a "Viewer" badge but does not enforce write restrictions. Any peer holding the doc can splice anything; the broker fans without validation. Today's role system is decoration over a permissive P2P fabric.

This document specifies the model and the build sequence to fix both.

---

## Model

### Five primitives

| Name | One-liner |
|---|---|
| **Identity** | Threshold-shared Ed25519 keypair representing a human. Mirrored across the user's devices. |
| **Space** | A social formation (a company, a household, a personal box). Has a member roster, a default identity to use when entering, and a list of channels. |
| **Channel** | A working area within a space. Has a member-subset, a per-member role, a default role for new members, and a set of artifacts. Cheap to create. |
| **Role** | Enum: `Owner | Editor | Commenter | Viewer | Denied`. Each role permits a fixed set of op classes per artifact kind. |
| **Artifact** | A note, file, cluster session, kanban board. Lives inside a channel. Inherits permissions from the channel; can carry per-artifact overrides (rare). |

### Identity

```rust
pub struct Identity {
    pub public_key: Ed25519PublicKey,
    pub label: String,           // human-meaningful: "ada — Personal"
    pub avatar: Option<ImageRef>,
    pub attestations: Vec<Attestation>,   // signed by trust roots
}

pub struct Attestation {
    pub issued_by: Ed25519PublicKey,   // a trust root (org's key)
    pub subject: Ed25519PublicKey,     // this identity
    pub claims: Vec<Claim>,            // EmployeeOf("ACME"), ClearedAt(Confidential)
    pub valid_from: SystemTime,
    pub valid_until: SystemTime,
    pub signature: Signature,
}
```

The private key is **threshold-shared**: a 2-of-N FROST scheme across the user's devices. Adding a new device requires two existing devices to co-sign the enrolment. No device holds the full key; every signing operation is a threshold sig. Recovery: pre-mint a single offline recovery share, stored on a USB stick or printed on paper.

One human, one identity, multiple devices. (Multiple identities per human are technically possible but the UX should not encourage it — one identity per person is the simple case we optimise for. A user with multiple identities — e.g. work + personal — can have them but they live as separate accounts in the identity switcher.)

### Space

```rust
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub trust_root: Ed25519PublicKey,     // who can sign attestations for this space's members
    pub default_identity_label: Option<String>,
    pub channels: Vec<ChannelRef>,
    pub member_directory: Vec<Identity>,  // everyone the space knows about, cached
}
```

A space is the social container. Joining a space means: receiving an invite signed by the space's trust root, accepting it, and your identity gaining a `Member-of(space)` claim. Leaving means: the space's trust root issues a revocation. Spaces are plural — one user belongs to many.

### Channel

```rust
pub struct Channel {
    pub id: ChannelId,
    pub space: SpaceId,
    pub name: String,
    pub owner: Ed25519PublicKey,          // can issue/revoke attestations
    pub default_role: Role,
    pub members: Vec<MemberRef>,          // explicit grants; subset of space members
    pub observe_default: bool,            // space owners can drop in as Viewer
    pub min_clearance: Option<Clearance>, // gating on identity attestations
    pub artifacts: Vec<ArtifactRef>,
    pub presence_visible_for_viewers: bool,   // default false — Viewer is a lurker
}

pub struct MemberRef {
    pub identity: Ed25519PublicKey,
    pub role: Role,
    pub attestation: AttestationId,       // proof this member has this role
}
```

A channel is the working area. Membership is a subset of the space's roster, with explicit per-member role attestations. Default role applies to space members not explicitly attested (commonly `Editor` for trust-rich teams, `Commenter` for larger groups, `Viewer` for read-only-by-default).

### Role and op classes

```rust
pub enum Role {
    Owner,        // governance: issue/revoke attestations, transfer ownership
    Editor,       // mutate any artifact
    Commenter,    // append comment-class ops only
    Viewer,       // read-only; no signed ops accepted from this role
    Denied,       // explicit block; overrides inherited grants
}

pub enum OpClass {
    Splice,       // text edits
    Move,         // kanban card moves, file reordering
    EditMeta,     // rename, retag
    Comment,      // append a comment / reaction
    Tombstone,    // soft delete
    Restore,      // undo a soft delete
    Govern,       // attestation issue/revoke; only Owner
}

// Static table per (Role, ArtifactKind, OpClass) → bool.
// Editor on Note: Splice/EditMeta/Comment/Tombstone/Restore = true. Govern = false.
// Commenter on Note: Comment = true. Everything else = false.
// Viewer: nothing = true.
```

This table is the source of truth for "can this op be applied?". Both the local shell (paint affordances) and the receiver's validator (reject invalid ops) consult it.

### Artifact

```rust
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,        // Note | File | Cluster | Kanban | ChatLog
    pub channel: ChannelId,        // home channel
    pub classification: Option<Clearance>,   // can be stricter than channel.min_clearance
    pub crdt_doc: Option<DocumentHandle>,
}
```

Artifacts live in their home channel and inherit its role+clearance rules. An artifact can carry a stricter clearance than its channel (a Confidential doc inside an Internal channel) but can never *loosen* — only the channel owner can raise an artifact's permission floor, never lower it below the channel's.

---

## Wire protocol

### Signed op envelope

```rust
pub struct SignedOp {
    pub payload: Vec<u8>,                  // automerge change bytes
    pub artifact: ArtifactId,
    pub author: Ed25519PublicKey,
    pub attestation_id: AttestationId,     // proof author is authorised for this op
    pub op_class: OpClass,                 // declares the semantic class
    pub timestamp: SystemTime,
    pub seq: u64,                          // monotonic per (author, channel) for replay defence
    pub signature: Signature,              // over canonical bytes of all above fields
}
```

Every CRDT mutation crossing any boundary (local broker → other apps; broker → libp2p peers; one device → another of the same user) is wrapped in this envelope. Unsigned ops are accepted only for legacy artifacts marked as unsigned, with a visible "legacy" badge.

### Validation pipeline (per inbound op)

1. Resolve `attestation_id` in the channel's local role table. If unknown, lazy-fetch from peers; defer the op meanwhile.
2. Verify `attestation.signature` traces to a key authorised as channel owner. Channel owner is set at the channel's genesis event (the first signed event by its creator).
3. Verify `op.signature` against `author`'s public key.
4. Verify `author == attestation.subject`.
5. Look up `(attestation.role, artifact.kind, op_class)` in the role table. Must be `true`.
6. Verify `timestamp ∈ [attestation.valid_from, attestation.valid_until]`.
7. Verify `seq > last_seq_from(author, channel)`. (Replay defence.)
8. If `artifact.classification` is set: verify `author`'s identity carries an `Attestation` with `Claim::ClearedAt(c)` where `c ≥ classification` and the attestation's issuer is in the channel's `accepted_trust_roots`.

If any check fails: drop the op, append a structured event to the channel's audit log (`OpRejected { op_hash, reason, observer }`), surface a toast to the local user "rejected edit from <author>: <reason>".

If all pass: apply to the local automerge doc, fan to other connected peers (broker forwards only validated ops).

### Genesis and ownership

```rust
pub enum ChannelEvent {
    Genesis {
        channel_id: ChannelId,
        space: SpaceId,
        name: String,
        owner: Ed25519PublicKey,
        created_at: SystemTime,
        signature: Signature,           // by owner over the above
    },
    AttestationIssued(RoleAttestation),
    AttestationRevoked { attestation: AttestationId, signed_by: Ed25519PublicKey, signature: Signature },
    OwnershipTransfer { new_owner: Ed25519PublicKey, signed_by: Ed25519PublicKey, signature: Signature },
    Rename { new_name: String, signed_by: Ed25519PublicKey, signature: Signature },
    SettingsUpdate { delta: ChannelSettingsDelta, signed_by: Ed25519PublicKey, signature: Signature },
}
```

These are append-only governance events. They form the channel's authoritative history; every peer replays them to arrive at the current role table. They themselves are CRDT data — appended via the same `SignedOp` envelope — and validated the same way.

---

## Encryption

The wire format above is in plaintext until we layer encryption. Plan:

- Each channel is an **MLS group** (via `openmls`). Member set = identities with active role attestations.
- Group key rotates on every membership change (add, remove, role change). MLS handles the rotation epoch internally.
- The `SignedOp.payload` field is encrypted under the current group key before broadcast. Recipients decrypt before validation.
- The `SignedOp` envelope's *metadata* (author, artifact, op_class, timestamp, seq) stays cleartext — needed for routing without leaking content.
- At rest: the local automerge doc cache is encrypted with a device-local symmetric key derived from the user's threshold-shared identity.
- Cross-org direct shares (Bob from outside sends a file): per-recipient envelope — encrypt the artifact's content key with the recipient's identity public key, send.

Implementation lands *after* signing, layered on top.

---

## UX surfaces

### Top bar

- **Left**: current space + channel breadcrumb. "Company / #engineering". Click → quick channel switcher.
- **Center**: identity badge with avatar + label. Click → identity picker. Subtle chrome tint shifts by ~2% hue per identity so you can feel which "you" is active without reading.
- **Right**: personal dock. A strip of always-on apps (Signal, Spotify, Browser, personal terminal) that live outside any space. Clicking expands them as a floating window over whichever channel is in focus.

### Wheel disc

The wheel disc shows **channels**, not spaces. Each spoke is one channel; spokes from the same space share a coloured rim band. Auto-promoted: the channels you've touched in the last week + manually pinned ones, capped at 12 spokes. Overflow: a `+` spoke opens a typeahead channel browser (reuses the command-palette widget) listing every channel in every space you belong to, with last-activity timestamps and unread counts.

### Channel chrome

- **Roster pill**: shows "active" members (Editor+ tier with `presence_visible: true`), plus a count of viewers (un-named, since viewers may be lurking).
- **Roles tab in channel settings**: lists members, their roles, expiry. Owner can edit each row. Adding a member mints an attestation (governance event).
- **Artifact tiles**: each tile carries a small classification chip in the corner if the artifact has a non-default clearance.

### Activity pane per channel

A chronological timeline of governance events + ops. Per-row:
- Author + identity badge
- Op class (Splice / Move / Comment / Tombstone / etc.)
- Affected artifact
- Timestamp
- "Revert" button for Editors+ (restores the artifact to its pre-op state via a `Restore` op)

This is the audit surface. Replaces the current per-workspace "Permissions" tab.

### Direct shares inbox

A system surface in the personal dock. Receives `quickdesktop://share/...` links from outside any space. Items can be:
- Read in place (decrypt + render, no membership created)
- Dragged onto a channel to pin (becomes a channel artifact owned by the recipient, with sender attribution)
- Discarded

### Invite flow

- **Channel owner mints**: WS settings → Members → "+ Invite". Generates a QR code + `quickdesktop://join/<channel_id>?invite=<signed_token>` URL. Token encodes role, expiry, single-use vs multi-use.
- **Recipient accepts**: scans QR or clicks URL. Their shell parses, verifies the signature against the channel's trust root, shows a confirmation modal: "Join #engineering as Editor? (Signed by Alice @ Company)". Accept → device signs a `Joined` event back → channel admits.

### Device enrolment (threshold identity)

- First device boot: generate identity keypair, single-device mode (no threshold yet).
- Adding a second device: Device A scans QR from Device B → both perform FROST setup → key is now split 1-of-2. From this point any signing operation requires both devices (or one device with the offline recovery share).
- Adding third+: pick any two existing devices, both confirm a 6-digit code shown on the new device's screen → FROST adds a share. Now 2-of-3.

---

## Build sequence

Nine phases, sequenced for value delivery and risk management. Each phase ships independently and improves the system without blocking on later phases.

### Phase 1: Audit timeline + soft-delete + restore-from-history

**Target:** two weeks. Smallest, lowest-risk, highest immediate utility.

- Every CRDT op is already in automerge history; expose it via a structured iterator on the artifact's `DocumentHandle`.
- Add a `Tombstone { artifact, deleted_at }` op class. Deletion = appending a tombstone op. Artifacts with a tombstone are hidden from the UI but their data + history remain.
- `Restore` op class undoes a tombstone. Available to Editor+.
- Per-channel **Activity pane** widget: timeline of recent ops, grouped by author, with `[Revert]` per row.
- Per-artifact "view history" affordance: scrubber over historical states, click any state → preview + "Restore to this version" button.

No cryptography. No new wire format. Just history+UI work.

### Phase 2: Identity + signing infrastructure

**Target:** three weeks. The cryptographic foundation.

- Add a stable `Identity` keypair, persisted under `~/.config/qos/identity/`. Single-device (no threshold yet).
- Wire identity into the libp2p layer (replaces the per-shell-instance keypair as the *display* identity; libp2p's transport key stays underneath as a device key).
- Generate identity at first boot. Label defaults to system username.
- Define `SignedOp`, `RoleAttestation`, `ChannelEvent` wire types in a new `qos-collab-types` crate.
- Implement signing + verification primitives. Canonical-bytes serialisation (postcard or similar deterministic format).
- Unit tests for envelope round-trip, signature verification, tamper detection.

No enforcement yet — types exist, signatures get made on outbound ops, validators are stubs.

### Phase 3: Enforce signed roles on the wire

**Target:** three weeks. Where roles start meaning something.

- Extend `CollabRequest::SyncIncremental` + `NeighbourMsg::AppCrdtIncremental` payloads to carry `SignedOp` envelopes.
- Broker validates inbound envelopes against the local channel role table before fan-out.
- Recipients validate inbound ops before applying.
- Drop + log + toast on validation failure.
- Migration: existing unsigned channels keep working with an "unsigned legacy" badge. Owner can run a one-shot upgrade that mints attestations for current members and starts requiring signed ops from that point. Legacy history is grandfathered.

After this phase, role enforcement is real.

### Phase 4: Conflict toolkit + reconciliation UI

**Target:** two weeks, partly overlapping with Phase 3.

- Every CRDT-bound widget declares a `MergeStrategy` at construction (`Text`, `Set`, `LastWriterWins { surface_on_conflict }`, `Custom(...)`).
- Compile-time lint: widget bound to a CRDT field without a declared merge strategy → warning.
- Toolkit-provided `ConflictBanner` + `ConflictPicker` widgets.
- App-level `ConflictResolver` trait for `Custom` strategies (kanban-card-move, file-rename-collision, etc.).
- Reconciliation pane that appears in channel chrome after offline-sync: "N edits to reconcile."
- Quarantine drafts for offline edits that fail post-reconnect role validation.

### Phase 5: Channel browser + wheel-disc overflow

**Target:** one week. Small but unblocks larger spaces.

- Wheel disc caps at 12 spokes; auto-promote recent + pinned.
- Overflow `+` spoke opens a typeahead channel browser (reuses command-palette widget).
- Channel list grouped by space (colour band), shows last-activity + unread count.
- Keyboard-driven, fuzzy match.

### Phase 6: Compaction + lazy scrollback

**Target:** two weeks.

- Host-driven checkpointing: every 200 ops or 2 MB, host (initial: channel owner's primary device) emits a `Checkpoint` signed event.
- Peers snapshot to checkpoint, move older ops to cold storage (`history/<artifact_id>/<base_version>.bin`).
- Scrolling past checkpoint horizon → `RequestHistoryRange` over the broker, any peer with the range responds.
- UI: "Loading older edits…" spinner when fetching.
- Document host failure mode + fallback (v1: owner-as-host only; v2: quorum-elected backup host).

### Phase 7: Group encryption with MLS

**Target:** four to six weeks. Highest-stakes piece.

- Integrate `openmls` for group key management.
- Each channel = one MLS group; group key rotates on membership change.
- Encrypt `SignedOp.payload` under group key; metadata stays cleartext.
- Encrypt local doc cache at rest with device-local key derived from identity.
- Direct-share path: per-recipient envelope using recipient's identity pubkey.
- Migration for existing unencrypted channels: opt-in upgrade per channel, owner triggers.

### Phase 8: Threshold-shared identity + invite ceremony

**Target:** three weeks. Can land alongside Phase 7.

- FROST 2-of-N threshold for identity keypair.
- Device enrolment UI: QR + 6-digit confirmation across two devices.
- Recovery share generation (offline, one-shot, displayed during first device setup).
- Invite-generation UI for channel owners: mint QR + URL.
- Invite-acceptance UI: validate signature, confirm role + clearance, accept.
- Mirror channel membership across the user's devices (per-channel `mirror_to_devices: bool` flag).

### Phase 9: Relay infrastructure + redaction (enterprise)

**Target:** ongoing.

- Standalone `qos-relay` binary: long-lived libp2p node, public address, forwards gossipsub + DHT, sees only ciphertext + metadata.
- Deployment artifacts: systemd unit, container image.
- Service discovery: shell finds relay via DNS SRV or static config.
- Cryptographic redaction (`RedactRequest` event): channel owner can request a range of ops be replaced with sealed markers; cooperative peers ACK and apply. Audit trail preserved.
- Marked as "enterprise mode" feature behind a flag; not for the default path.

---

## Out of scope (explicitly)

- **Revocation of locally-cached data.** The user's device is theirs. We add an opt-in "self-delete on revocation" honour-system behaviour. Adversarial revocation requires hardware enclaves we're not building.
- **Mobile / cross-OS clients.** The signing+encryption wire format is designed to be portable, but we don't build clients for Mac/Windows/iOS/Android in this scope. After the ecosystem foundation is solid we revisit.
- **eDiscovery / GDPR right-to-be-forgotten beyond cooperative redaction.** Documented as a known limitation. Enterprise-mode redaction is best-effort cooperation, not a guarantee.

---

## Defaults

- New channel in a space ≤10 people: default role `Editor`. Default `presence_visible_for_viewers: false`.
- New channel in a space 11–50: default role `Editor` for own channels, `Commenter` for general-purpose.
- New channel in a space 51+: default role `Commenter`. Viewers can read but cannot write without a per-member role escalation.
- Every artifact's default `classification: None` (no clearance check).
- New space: trust root is the creator's identity; creator is sole admin until they delegate.

---

## Tradeoffs and known costs

- **Threshold identity adds enrolment friction.** Mitigated by: very polished QR-pair flow, recovery share for solo bootstrap. Worth the cost vs centralised key escrow.
- **Signing every op costs CPU.** Ed25519 sigs are fast (~70µs); negligible for human-paced editing. Verifying replication storms (a peer rejoining with thousands of queued ops) may need batching.
- **MLS group rekey on every membership change.** Worst case: a channel with 100 members and frequent turnover spends time rekeying. Acceptable for human-scale orgs; we don't aim to scale this to Slack-public-channel size.
- **Lazy history fetch can stall when no peer has the range.** Document: a channel with all peers offline cannot serve scroll-back. UI: degraded mode banner. Self-hosted relay solves it; community-mode users live with the limitation.
- **Op-class validation table is fixed at compile time.** New artifact kinds need a schema bump. Acceptable: artifact kinds are added rarely.

---

## Migration from current code

The existing `WorkspaceTree` becomes the first space + a single channel per existing workspace:

```
old Workspace { collaborators, ... }
  → new Space { name: "Personal", members: [me], channels: [legacy_channel] }
    new Channel {
      name: "Workspace",
      owner: me.identity,
      members: collaborators.map(|c| MemberRef { c, role: Editor }),
      observe_default: false,
      // ...
    }
```

Migration script runs once on first boot of the upgraded shell. Pre-existing CRDT docs are grandfathered as unsigned; the channel is marked "legacy / unsigned" with a banner offering to upgrade.

---

## What this document does NOT specify

- The notification / signal-to-noise model (per-channel mute, batching, badge logic). Will be its own design.
- The kanban primitive (mentioned as an artifact kind but not specified). Lives in a separate doc once we build it.
- The chat primitive. Same.
- The deployment / ops story for self-hosted relays. Lives in `os/` docs.
- Pricing / commercial details for hosted relay-as-a-service. Lives in business docs.

---

## Open design questions still warm

1. **Channel nesting depth.** Specified as one level (Space → Channel). The user's life has 2-3 levels in places (Company → Customer-X → integration-work). We've punted to "use a flat list with naming conventions." Revisit if usability suffers.

2. **Member directory privacy.** Listing every space-member to every other space-member may leak data the owner doesn't want shared. Could be opt-in per member.

3. **Recovery share UX.** Where does the user store it? Paper? Hardware token? We need to make this not-scary while not-insecure. The default for a non-expert user is going to lose this; how do we recover them?

4. **Cross-space artifact references.** Pinning a Note from Channel A into Channel B (read-only). Conceptually allowed (artifact has only one home channel; other channels can hold references) but the UX is unclear — how do you make it visually obvious "this is from elsewhere, don't try to edit"?

5. **Audit retention.** Audit events accumulate indefinitely. Probably want a per-space retention policy. Default forever; enterprise can configure.
