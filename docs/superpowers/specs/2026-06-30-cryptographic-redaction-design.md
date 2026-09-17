# Cooperative cryptographic redaction

**Date:** 2026-06-30
**Record type:** Historical security design rationale. Protocol definitions and enforcement behavior live in the collaboration code and tests.

## Security boundary

Redaction is a cooperative compliance protocol for a channel's authorized peers.
It can suppress a targeted operation from future local rendering and normal
replication, retain an auditable governance record, and expose divergence. It
cannot make a recipient forget content already delivered, undo screenshots or
copies, or force deletion by an adversarial peer. Encryption does not change
those facts.

This distinction is non-negotiable: describe redaction as cooperative removal,
never as a cryptographic erasure guarantee.

## Authority and target identity

A redaction is a signed, owner-authorized governance operation that names one
artifact and one deterministic operation identity plus the hash of that
operation's canonical signed bytes. Its signer and payload identity must agree.
The target hash prevents a request from being silently retargeted; the stable
operation identity lets peers recognise a redaction regardless of arrival order.

Only a governance authority may redact. Ordinary deletion remains separate: it
hides an artifact or edit semantically, whereas redaction records a compliance
decision and constrains future rendering and propagation.

## Apply and replication semantics

Peers persist the redacted-operation set alongside channel state. When a valid
redaction arrives, a peer suppresses the target from derived content and emits an
audit record containing the redactor, target, reason, time, and available original
reference. It uses a typed redacted marker, rather than a missing sequence item,
so replicas retain ordering and history can explain what happened without showing
the removed payload.

An operation that arrives after its identity has been redacted is not applied to
derived state. It is represented as a redacted marker and audited. Recursive
redaction is rejected: the governance event recording removal must remain visible.

## Compaction and audit trade-off

Redaction records survive compaction. Checkpoint integrity is derived from the
original signed operation bytes, not from a locally substituted marker, so peers
can produce the same commitment despite differing delivery order. This requires
an explicit retention decision for original evidence; it is the intentional trade
off between auditable compliance history and stronger local forgetting.

When the target was already compacted, the governance operation is still valid.
The peer records the target identity and audit decision even when it cannot
rewrite an in-memory entry.

## Encrypted content

For encrypted operations, redaction blocks later decryption and display through
the same redacted-operation set. The target hash covers the encrypted envelope so
that it can be verified without decrypting. Retaining ciphertext supports audit
verification but is not content deletion; a product that requires physical local
scrubbing must specify a separate retention and storage-erasure policy.

## Policy and presentation

Redaction policy determines how non-compliance is presented, not whether a peer
can be compelled to comply. Channel history and timeline surfaces show a neutral
redacted marker, keep non-sensitive ordering and attribution context, and do not
offer restore paths that would reintroduce redacted content.

Wire evolution must be versioned: peers that do not understand a redaction
operation cannot safely participate in that feature. Legacy state needs explicit
defaults for any new policy fields, never implicit assumptions about historic
content.
