# Spaces, channels, and cryptographic roles

**Date:** 2026-06-29
**Record type:** Historical design rationale; implementation details and rollout state live in code and its tests.

## Purpose

This record separates the social container, working area, and authorization
boundary that the former flat workspace model conflated. Its durable rule is:
the UI may explain a permission, but receivers must enforce it before a
replicated mutation becomes state.

## Model

| Concept | Meaning |
|---|---|
| Identity | A human-facing signing identity; a person may use it from multiple devices. |
| Space | A social container with a member directory and trust roots. |
| Channel | A working area within a space, with an explicit member subset and policy. |
| Artifact | A document, file, board, or other object whose home is a channel. |
| Role | `Owner`, `Editor`, `Commenter`, `Viewer`, or `Denied`, mapped to permitted operation classes. |

Artifacts inherit the channel's role and clearance policy. An artifact may raise,
but never lower, its channel's clearance requirement. `Denied` overrides inherited
grants. The permission table is shared by local affordances and inbound-operation
validation, so a cosmetic role badge cannot become the authority boundary.

Spaces and channels intentionally remain distinct: spaces describe a durable
formation, while channels are cheap, focused working areas. Direct shares use a
recipient-owned artifact or reference rather than silently changing the source
channel's membership.

## Signed operations and governance

Every replicated mutation is represented by a signed envelope containing the
artifact, author, authorization reference, semantic operation class, timestamp,
per-author sequence number, payload, and signature. Legacy content may be
identified explicitly; it must not be mistaken for content that passed the signed
protocol.

Before applying or forwarding an inbound operation, a peer verifies:

1. the referenced authorization and its issuing authority;
2. the envelope signature and that its author matches the authorization subject;
3. the role/operation/artifact permission table and applicable clearance;
4. validity time and monotonically increasing sequence number.

Rejected operations are not applied or forwarded. They remain observable through
structured audit data, with enough reason information to diagnose policy and
replication failures without exposing payloads unnecessarily.

Channel governance is append-only signed data: genesis, role issuance and
revocation, ownership transfer, rename, and policy changes. Replaying that data
produces the effective role table. This preserves a verifiable authority history
instead of relying on mutable local configuration.

## Confidentiality and retention boundaries

Signing and authorization protect integrity; group encryption protects content
confidentiality. Where group encryption is used, channel membership drives key
membership and membership changes require a key transition. Routing metadata must
be kept to the minimum needed for validation and delivery. Device-local document
caches require separate protection.

Encryption, revocation, and redaction do not retrieve bytes that another device
has already read or copied. Cooperative redaction is specified separately in
[`cooperative-redaction.md`](cooperative-redaction.md); it is a compliance protocol, not
a guarantee against an adversarial recipient.

## User-facing consequences

The model calls for channel-scoped membership and history views, an identity
indicator, classification cues, and explicit invite confirmation. Viewer presence
is private by default. Activity/history presentation must distinguish normal
edits, rejected operations, soft deletion, restoration, and governed changes.

## Compatibility constraints

Existing flat workspaces can be represented as a personal space with a legacy
channel. Existing content must retain an explicit compatibility state until it is
covered by the signed authorization model; migration must never silently assert
authorization that did not exist in historic data.

## Boundaries

This record does not define notifications, chat or board semantics, relay
operations, commercial policy, or a concrete device-enrolment UX. Those concerns
must preserve the model and validation invariants above rather than creating a
parallel authorization path.
