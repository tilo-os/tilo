# Cryptographic redaction (Phase 9, second half)

**Date:** 2026-06-30
**Status:** Design accepted; implementation lands alongside this doc
**Companion to:** `2026-06-29-spaces-channels-and-cryptographic-roles.md` §"Phase 9"
**Replaces:** the half-sentence mention of `RedactRequest` in the parent spec

---

## Problem and frame

The parent spec already lands signed roles (Phase 3), compaction (Phase 6), MLS
encryption (Phase 7), and the standalone `qos-relay` (Phase 9 first half). What
remains is the awkward question every CRDT system eventually hits: **once a
piece of content is in the gossip mesh, how do you take it back?**

Concrete triggers:

- A chat message in `#engineering` contains a customer's home address pasted
  by mistake. The poster wants it scrubbed.
- A kanban card title on the `Hiring` board records a candidate's medical
  status. Legal flags it under GDPR; the org must produce a "right to be
  forgotten" record.
- A comment under `report.docx` accidentally includes a paragraph from an
  unrelated NDA'd document.

We must let a channel owner say "this op is now gone" and have peers honour
it, **without** pretending we can claw bytes back from an adversary. The
design's overarching frame is:

> Redaction is a **cooperative compliance protocol**, not a security
> mechanism. Cooperating peers honour it, uncooperating peers cannot be
> stopped, and we accept that.

---

## 1. Threat model and non-goals

### In scope

- An **org-mandated removal** of a specific op on a channel the org operates.
  Triggered by:
  - GDPR / DSA / national-data-protection right-to-erasure requests.
  - Accidental PII paste (customer data, NDA'd quotes, medical records).
  - Court-ordered takedowns inside an enterprise tenant.
- Peers in the channel are **still in the trust circle** at the moment of
  redaction (they are not adversaries; they want to honour the policy).
- Cooperating peers redact in-memory state, filter the op out of subsequent
  reads, and write a redaction record to the channel's audit log.

### Out of scope

- **Forcing redaction onto a peer who has gone rogue.** Once an op has left
  the broker, a peer with a copy on disk is *physically* in possession of it.
  Cryptographic protocols cannot un-deliver bytes — anyone who claims
  otherwise is selling theatre.
- **Off-band leaks.** Screenshots, OCR, paste-into-other-tools, accessibility
  tree scraping, photos of monitors. Outside our threat model.
- **Pre-redaction reads.** Anyone who already read the bytes before the
  redaction reached their peer has seen the content. We don't promise
  forgetting in their head.
- **Forensic-grade erasure** (DoD-style multi-pass wipes of underlying
  storage). v1 redacts in process memory and the persisted op log; deep
  storage scrubbing is a separate concern (Phase 9.5).

### **Decision (chosen):** redaction is cooperative

A peer who refuses to redact is detectable (their audit log will diverge from
the channel's authoritative event log; see §4). We surface that fact via the
channel's audit timeline but do not — and cannot — prevent it.

---

## 2. Op shape

### Wire type

```rust
// qos-collab-types/src/redaction.rs
pub struct RedactRequest {
    pub artifact: ArtifactId,
    pub target_op_id: OpId,        // deterministic id of the op being redacted
    pub target_op_hash: [u8; 32],  // sha256 of the target's canonical bytes
    pub reason: String,            // free-text, capped at 512 bytes
    pub redacted_by: Ed25519PublicKey,
    pub redacted_at: SystemTime,
}
```

Carried as the payload of a `SignedOp` with `op_class == OpClass::Redact`.

### `OpId` derivation

```rust
pub struct OpId(pub [u8; 16]);

impl OpId {
    /// Derive a stable id from (author, seq, canonical-hash-prefix).
    ///
    /// The first 8 bytes are the BLAKE3 prefix of the author's pubkey
    /// XOR seq, the next 8 are the prefix of the op's canonical bytes
    /// (excluding the signature). Two peers running the same code
    /// derive the same id for the same op.
    pub fn from_op(op: &SignedOp) -> Self { ... }
}
```

We pick a derived id rather than carrying an explicit `op_id` field on every
op for two reasons:

1. **Wire stability.** Adding an explicit field to `SignedOp` is a breaking
   wire change; deriving it post-hoc isn't.
2. **Forward compat for ops minted before Redact existed.** Phase 3 and Phase
   6 ops in the wild don't carry an op_id; redaction must still be able to
   address them.

### `reason` bound

The 512-byte cap is enough for "GDPR request from data subject Alice
<alice@acme.example> — case ACME-2026-04-17" and a sentence of context.
Larger justifications belong in the compliance system, not the audit log.

### Role permission

```rust
// qos-collab-types/src/role.rs (added)
OpClass::Redact => match role {
    Role::Owner => true,
    _           => false,
}
```

**Decision (chosen):** Owner-only. Editors can already tombstone (soft-
delete), which is the right primitive for "I changed my mind about this
edit." Redaction is a governance act and stays with governance authority.
A non-owner who wants something redacted asks an owner; that conversation
itself becomes part of the org's compliance record.

### Signer ↔ payload binding

The validator must enforce `op.author == redact_request.redacted_by`, so a
peer cannot mint an op whose envelope says "signed by Bob" while the payload
claims "redacted_by Alice". This is a separate check from the existing
step-4 (author == attestation.subject) because attestations don't carry
identities — the `redacted_by` field lives inside the payload.

---

## 3. Application semantics

### What "applying" a Redact means

On a valid `Redact` op `r`:

1. Compute `target = r.payload.target_op_id`.
2. Insert `target` into `ChannelState.redacted_ops: HashSet<OpId>`.
3. Walk `ChannelState.op_log` and, for every entry `e` with
   `OpId::from_op(&e) == target`:
   - Replace `e.payload` with the canonical-bytes encoding of a
     `<redacted>` marker (a fixed sentinel struct that round-trips through
     postcard and is itself decodable).
   - Recompute `e.signature` is **not** attempted — the original signature
     no longer verifies because we've changed the payload. The op log
     entry is now a "redacted-marker" stand-in. **The original envelope is
     preserved in the audit log** so the unredacted signature can be
     inspected for compliance reconstruction (see §8).
4. Insert a `AuditEntry::Redaction { … }` entry into the channel's audit
   log, keyed by `r.artifact`.

### `ChannelState::is_redacted` and `apply_redaction`

```rust
impl ChannelState {
    pub fn is_redacted(&self, op_id: OpId) -> bool {
        self.redacted_ops.contains(&op_id)
    }

    /// Returns the number of op-log entries rewritten. A redaction
    /// that targets an op the local log doesn't currently hold is
    /// still recorded — the op may have been compacted away, or may
    /// arrive later (see §7).
    pub fn apply_redaction(&mut self, redact: &SignedOp) -> Result<usize, RedactionError>;
}
```

### Three concrete artifact examples

- **Chat message** (artifact kind `ChatLog`): the redacted op's payload was
  an `automerge` change splicing characters into the log. After redaction,
  scrollback shows a struck-through "[redacted by alice@acme on 2026-06-30
  — GDPR request]" row in place of the original message body. The row's
  timestamp and author display name remain visible (so the chat reads
  coherently). The redacted text never re-renders on this peer.

- **Kanban card title** (artifact kind `Kanban`): the redacted op was a
  `Splice` on the card's title text. The card still exists; its title
  renders as `[redacted]` plus the redactor tooltip. The card's history,
  comments, and column position are untouched.

- **Comment under a document** (artifact kind `Note`, op class `Comment`):
  the comment's body is replaced by the marker; the side-anchor stays
  pinned to its text range so the visual layout doesn't shift. The
  commenter's avatar dot renders normally so the audit pane can still show
  "Bob said `[redacted]`".

### **Decision (chosen):** filter vs replace

`FetchScrollback` and the broker's fan-out path **return the redacted-marker
payload, not nothing**, when an op has been redacted. The op envelope stays
in the log (with its rewritten payload + invalidated signature flag set);
peers downstream see the same redacted-marker. The reason:

- Filtering it out entirely would create a hole in the seq stream. A late-
  joining peer would see seq 4 and seq 6 with no seq 5 and would chase
  cooperative scrollback for ever.
- Replacing the payload keeps the seq + author + timestamp + op_class
  metadata available for the UI's audit timeline (so a reviewer can see
  "alice removed this at 14:32" without seeing the content).
- The redacted-marker is itself a typed payload (`RedactedMarker`) decodable
  by `qos-collab-types`, so widget code can render the strike-through
  without special-casing "missing payload".

---

## 4. Interaction with compaction

### Two cases

**Case A: op already compacted.** The target op is older than the most
recent `Compaction` checkpoint; it is no longer in any peer's in-memory
log. The redaction is still valid:

- The Redact op envelope itself is gossiped, validated, and applied (the
  redacted_ops set gains `target_op_id`).
- `apply_redaction` returns `Ok(0)` — zero log entries rewritten.
- The audit log records the redaction. Compliance reconstruction can
  cross-reference the Redact's `target_op_hash` against any peer's cold
  history storage (if it still exists).

**Case B: redaction precedes compaction.** A Redact lands at seq 80; later
the owner emits a Compaction with `up_to_seq=100`. The compaction's
prefix-hash now covers ops including the redacted (now marker-payload) op.

### **Decision (chosen):** compaction hashes the **original** op bytes

The prefix hash inside a `CompactionCheckpoint` is computed over the
**pre-redaction** canonical bytes of each op. Reasons:

- Determinism across peers. A peer that received the Redact before the
  pre-Redact op would compute the marker bytes; a peer that received the
  op first and the Redact afterwards would have the original bytes
  (transient — until it applies the Redact). Compaction-time would race
  against redaction-time and the prefix hash would be ambiguous.
- Audit reconstruction. The original bytes are recorded inside the audit
  log's `Redaction` entry (cap'd by the existing audit-log ring; in v1 the
  full envelope is preserved per `AuditEntry::envelope: Box<SignedOp>`),
  so the compaction hash remains computable forever.
- A redacted-marker is **derived state**, not log-of-record state. The log
  of record is "what the author actually signed"; we redact the derived
  copy for compliance but keep the signed log entry's bytes addressable
  for the prefix hash.

In practice this means hosts emitting a Compaction checkpoint compute the
prefix hash from a `original_bytes_of(op)` accessor on `ChannelState`,
which the validator populates with the `pre_redaction_bytes` field of each
redacted op (kept in the audit log + a tiny `original_payloads:
HashMap<OpId, Vec<u8>>` cache on the state — bounded by the redaction set,
which is small by construction).

### Redaction record outlives compaction

The `redacted_ops` set survives any number of compaction checkpoints. The
Redact op itself is **never evicted** by a Compaction (it has its own seq
> the targeted op's seq, and the audit log keeps a structured copy too).
The compaction's `apply_compaction` filter retains any op with
`op_class == Redact` regardless of seq — explicit carve-out documented in
the code.

---

## 5. Interaction with MLS encryption

A Phase-7 op carries `encryption: Some(EncryptionMetadata { epoch, channel })`
and a ciphertext payload. The validator decrypts before applying. **What
does redaction mean here?**

Three options were considered:

| Option | Mechanism | Pro | Con |
|---|---|---|---|
| (a) Drop ciphertext | local storage replaces `payload` with `&[]`; audit log keeps `target_op_hash` only | most "actually deleted" | breaks signature reverification for any future audit replay; need a separate "ciphertext absent" flag in the envelope |
| (b) Re-encrypt to marker | encrypt a `RedactedMarker` cleartext under the current epoch key | every member sees the marker; non-members can't tell new from old | theatre — anyone with a prior decryption already saw the original |
| (c) Mark redacted, keep ciphertext | flip a flag; downstream rendering pipelines refuse to decrypt + display | simplest; uniform with the cleartext case | the ciphertext stays on disk |

### **Decision (chosen): option (c) with a cleartext-twin record**

Implementation:

- The validator's apply path sets `ChannelState.redacted_ops.insert(id)` the
  same way as for cleartext.
- The `EncryptionMetadata` on the redacted op is left intact in the log; the
  payload bytes are kept *and never decrypted again* by the rendering
  pipeline. The pipeline consults `is_redacted(id)` before issuing a
  `decrypt` call.
- The `RedactRequest.target_op_hash` is the SHA-256 of the **canonical
  bytes of the SignedOp envelope including the ciphertext payload**, not
  of the cleartext. This lets a peer verify "yes, this redaction targets
  the op I have a copy of" without ever decrypting.

Why not (a): purging ciphertext from local storage breaks the audit-log
reconstruction story. The audit log's `target_op_hash` must remain
verifiable, and verification needs the original bytes. We pick "keep the
bytes, refuse to decrypt" as the cooperative-compliance posture; (a) is a
future v2 option for tenants who want stronger forgetting and accept the
audit-trail cost.

Why not (b): re-encrypting to a marker is pure theatre — every current member
already had the original cleartext during the original epoch. It also burns
an MLS epoch transition per redaction, which is expensive.

---

## 6. Replay defence interaction

Each Redact op has a `seq` counter per-author, same as every other op. The
existing step-7 of the validator (`seq > last_seq_per_author[author]`)
covers it — a peer cannot re-broadcast a stale Redact and have it apply
twice.

### **Decision (chosen):** recursive redaction is rejected

A `Redact` op whose `target_op_id` resolves to another `Redact` op is
rejected at step 5b (a new sub-case of the role-table check). Reasons:

- Semantic muddiness — "un-redact" is not a concept; we already keep the
  redaction record in the audit log forever.
- A bug or attacker chaining redacts could craft a graph that loops
  through the redacted-ops set and hide the very fact of a redaction.

The validator surfaces this as `ValidationError::RecursiveRedaction`.

---

## 7. Late-arriving op with already-redacted id

Scenario: alice mints op `o` at seq 5; her network drops; she goes offline.
While offline, bob (an owner) redacts op `o` based on a copy he received
from carol. Then alice comes back online and the broker fans her op `o` to
everyone.

What each peer does on receiving `o`:

1. Step 1-8 of the existing validator run as normal — `o` itself is a
   well-signed op that the role table permits.
2. **New step 9 (redaction-aware filter):** before applying, the validator
   checks `ChannelState.is_redacted(OpId::from_op(&o))`. If true:
   - The op is **not** applied to the derived state.
   - The op log entry is inserted with the redacted-marker payload (so seq
     gaps don't form).
   - An audit entry records `OpArrivedRedacted { op_envelope }` so the
     operator can see the late arrival.

This is the load-bearing piece for "a buggy peer can't re-broadcast around
the redaction." Even if every connected broker forgot the redaction (which
shouldn't happen — the redacted_ops set is persisted with the channel state
in v2; for v1 the in-memory record is enough because new peers fetch the
governance event log on join), the **fact** that the Redact op exists in
the event log is sufficient to re-derive the set on any cold start.

---

## 8. Audit log entry shape

```rust
// shell/src/shell/collab_broker.rs
pub enum AuditEntryRecord {
    /// Phase 3: rejected envelope (the existing variant, lifted from
    /// the previous `AuditEntry` struct shape).
    Rejected {
        envelope: Box<SignedOp>,
        reason: ValidationError,
    },
    /// Phase 9: cryptographic redaction event.
    Redaction {
        redact_envelope: Box<SignedOp>,    // the Redact op itself
        target_op_id: OpId,
        target_op_hash: [u8; 32],
        redactor_pk: Ed25519PublicKey,
        reason: String,
        redacted_at: SystemTime,
        /// The pre-redaction canonical bytes of the targeted op, if
        /// the broker held a copy at redact-time. `None` if the op
        /// had already been compacted away (Case A in §4).
        original_envelope: Option<Box<SignedOp>>,
    },
    /// Phase 9: a peer's broker received an op whose id was already
    /// in the redacted_ops set. Surfaced to operators because it
    /// usually indicates either an offline-clock peer re-joining
    /// (benign) or a misbehaving peer trying to re-introduce
    /// content (worth a look).
    OpArrivedRedacted {
        envelope: Box<SignedOp>,
        op_id: OpId,
    },
}

pub struct AuditEntry {
    pub record: AuditEntryRecord,
    pub at: SystemTime,
}
```

Backwards compat: the existing `AuditEntry.envelope` + `AuditEntry.rejection`
fields are replaced by the `AuditEntryRecord::Rejected` variant. Tests using
the old shape need migrating; the call sites are listed in §10.

### Accessor

```rust
impl BrokerHandle {
    pub fn audit_for(&self, artifact: ArtifactId) -> Vec<AuditEntry>;  // unchanged
    pub fn redactions_for(&self, artifact: ArtifactId) -> Vec<RedactionRecord>;
}

pub struct RedactionRecord {
    pub target_op_id: OpId,
    pub target_op_hash: [u8; 32],
    pub redactor_pk: Ed25519PublicKey,
    pub reason: String,
    pub redacted_at: SystemTime,
}
```

`redactions_for` is the structured projection of every
`AuditEntryRecord::Redaction` for the given artifact; consumed by chrome
widgets that don't care about rejections or late-arrivals.

---

## 9. What the chrome shows

### `ActivityTimeline`

- A redacted row renders the author's avatar dot, name, and op-class glyph as
  normal.
- The `summary` field is replaced with a struck-through label like
  `[redacted by alice@acme on 2026-06-30 — "GDPR request"]`. The strike-
  through is drawn via a thin horizontal line through the summary text
  baseline (vello stroke).
- A small lock glyph (`\u{1F512}` is an emoji and forbidden by the project
  convention — we use `\u{1f50f}` analog or the ASCII fallback `[!]` or the
  Unicode `\u{2622}`-style symbol; v1 picks `\u{229A}` "circled ring
  operator" as a neutral compliance indicator).
- The `[Revert]` chip is suppressed; redacted rows are non-revertible (see
  below).

### `HistoryScrubber`

- A tick whose `HistoryEntry` is marked redacted renders with a thin red
  vertical bar at that tick position.
- "Restore to this version" is **disabled** for redacted ticks. The
  `ConflictPicker`'s "Revert past" similarly refuses to roll back through a
  redacted tick — the chrome surfaces "this version contains redacted
  content; restore is unavailable" instead.

The widget changes are confined to:

```
ui/src/widget/history.rs        — add `redacted: bool` to HistoryEntry +
                                  `op_class_is_revertible` honours it
ui/src/widget/activity_timeline — render the struck-through label
```

`history_scrubber.rs` and `conflict_picker.rs` get away with reading the
new `redacted` flag and gating their existing affordances — no new public
API in those widgets (kept inside the file list constraint).

---

## 10. Migration / opt-out

### **Decision (chosen):** ship Cosmetic default, Mandatory toggle

```rust
pub enum RedactionPolicy {
    Cosmetic,    // default. peers MAY opt out of honouring; non-honouring
                 // is logged in the channel's authoritative event log so
                 // it's visible to the owner.
    Mandatory,   // owner-set. non-honouring is treated as a protocol
                 // violation: the violator's audit log will diverge and
                 // the chrome surfaces a banner. (Still cannot force
                 // compliance — see §1.)
}

// On Channel:
pub redaction_policy: RedactionPolicy,
```

Channels default to `Cosmetic`. Owners can flip a channel to `Mandatory` via
`ChannelEvent::SettingsUpdate { delta: ChannelSettingsDelta::Redaction(Mandatory) }`.
The validator does not behave differently in the two modes — both apply the
Redact. The difference is policy on what to surface in the audit timeline.

Why Cosmetic by default:

- The default qOS install is for personal + small-team use where redaction
  is a courtesy not a compliance requirement. Surfacing "Bob's peer hasn't
  honoured your redaction" as an alarm in those contexts is wrong.
- Enterprise tenants who want the harder posture flip the toggle once at
  channel creation.

### Upgrade path

Existing channels (pre-Phase-9) have no `redaction_policy` field. The
`Channel` struct gains `#[serde(default = "default_cosmetic")]` so wire
decoding of a legacy `Channel` blob populates `Cosmetic` and the tests don't
see a wire-break.

---

## Wire-stability summary

| Change | Wire impact |
|---|---|
| New `OpClass::Redact` variant | Postcard discriminant byte — adding a variant at the end of an enum is wire-safe for `#[serde(deny_unknown_fields)]` *if* no peer has yet shipped a `match` over the enum that errors on unknown variants. Existing peers DO error: an unknown discriminant is a hard decode error. **Mitigation**: Phase 9 is feature-gated by version; production deploys roll out shell + qos-collab-types together. |
| New `RedactRequest` payload type | Inside `SignedOp::payload`; payload is opaque from the envelope's perspective, so no wire impact at envelope level. |
| New `redaction_policy` field on `Channel` | Already discussed; `#[serde(default)]` keeps legacy blobs decoding. |
| Audit log shape change | In-memory only — audit log is not persisted in v1 — no wire concern. |

---

## Limitations called out explicitly

- **The original bytes live in the audit log.** A peer who wants to be
  truly forgetful must also redact their audit log; the system does not
  enforce that. (A future v2 adds a "forget completely after N days"
  retention policy per channel.)
- **A peer with disk access can read the ciphertext.** §5 keeps the
  ciphertext on disk. Cooperative deletion can be added later (the wire
  shape supports it — a peer can voluntarily blow away its local copy and
  the audit log records "I no longer have the original bytes").
- **Mandatory mode is honour-system.** We can detect divergence but cannot
  prevent it. This is documented in `## Out of scope` of the parent spec
  too.
- **No multi-target redactions.** v1 redacts one op per Redact envelope.
  Bulk-redact ("every op alice ever sent") is an operator workflow on top
  of v1, not a wire primitive. Reasons: keeping the envelope shape simple,
  and making each redaction independently auditable.

---

## Implementation plan (commits)

1. **doc commit (this file)** — `docs(spec): cryptographic redaction design`.
2. **qos-collab-types** — `feat(qos-collab-types): Phase 9 cryptographic
   redaction primitives`. Adds `redaction.rs`, `OpClass::Redact`,
   `role_permits` row, validator extensions, `redacted_ops` field,
   `apply_redaction`, `is_redacted`. Updates `lib.rs` re-exports.
3. **shell broker** — `feat(shell): wire redaction through the collab
   broker + audit log`. Migrates `AuditEntry` shape, adds
   `redactions_for(artifact)` to `BrokerHandle`, plumbs the apply path.
4. **shell tests** — `test(shell): redaction_pipeline 8-test integration
   suite`. Validates every §1-§7 scenario end-to-end.
5. **ui widgets** — `feat(ui): render redaction-aware activity timeline +
   history scrubber`. Adds `redacted: bool` to `HistoryEntry`, paints the
   struck-through label, gates `[Revert]` and "Restore past" through
   redaction boundaries.
6. **superproject** — `chore(shell, ui): bump for Phase 9 redaction`.

Each commit compiles standalone (the doc commit is comment-only and never
changes Rust files). The integration test suite at step 4 exercises the
full §3 + §4 + §5 + §7 surface; widget tests are added inline in step 5.
