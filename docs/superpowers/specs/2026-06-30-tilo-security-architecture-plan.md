# Tilo security architecture rationale

**Date:** 2026-06-30
**Record type:** Historical security rationale. The current threat model, boot chain, and operations guidance live in `docs/security/`.

## Defense in depth

Security boundaries are selected by the principal they constrain: the verified
base system; first-party and third-party apps; untrusted peer-provided code; and
a potentially curious hosted operator. No individual mechanism is sufficient.
Boot verification, sandboxing, capability checks, authenticated identity,
supply-chain verification, and recovery paths must compose without each being
mistaken for the whole trust model.

The security UX follows the same rule: normal, expected operation should be
quiet; an unexpected trust transition must be understandable and recoverable.
Recovery is a security feature, not an exceptional escape hatch.

## Device and account identities

A device key and a person's account identity are distinct objects, even where a
single-user deployment initially binds them together. They may be cross-signed,
but a device is not the owner and a person is not a transport credential.

This separation preserves an additive path to multiple users: device encryption
remains a device capability, user homes and sessions use user credentials, and
workspace authority belongs to an owning identity rather than a singleton device
owner. Authorization lists must be collections, not a special-cased "the owner"
field. Adding or removing a user must not require re-encrypting unrelated user
data or redesigning collaboration authority.

## Durable constraints

- Untrusted application and peer content receives least authority by default;
  rendering a peer's code or arbitrary active content needs a stronger boundary
  than ordinary local application sandboxing.
- Capability grants are scoped, revocable, and checked at the operation and
  resource boundaries, not inferred from a UI control.
- Integrity claims are meaningful only when users can independently verify the
  signed image, provenance, and recovery path.
- A hosted operator must not become an implicit reader of tenant content merely
  by operating relay or storage infrastructure.

This record explains the architectural seam behind the current security
documents; it is not a rollout plan or a statement of deployment status.
