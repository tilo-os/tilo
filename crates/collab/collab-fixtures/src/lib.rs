//! Signed fixtures for tests that exercise the collaboration wire format.
//!
//! Minting a *valid* [`RoleAttestation`] or [`SignedOp`] means the same
//! ceremony production uses: build with a placeholder signature,
//! canonicalise, sign the canonical bytes, write the signature back — get
//! any step wrong and the validator rejects the result for a reason
//! unrelated to what the test was proving. Fixture divergence is worse
//! than production divergence, since it silently changes what a test
//! asserts rather than breaking the build — hence one copy, here,
//! dev-dependency only.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, Attestation, AttestationId, ChannelId, Claim, Ed25519PublicKey, EncryptionMetadata,
    OpClass, Role, RoleAttestation, Signature, SignedOp, sign_event, sign_op,
};
use ed25519_dalek::SigningKey;

/// A year back — comfortably before any test's "now".
const WIDE_PAST: Duration = Duration::from_hours(8760);
/// A decade forward — comfortably after it.
const WIDE_FUTURE: Duration = Duration::from_hours(87600);

/// Mints a signed [`RoleAttestation`] with an explicit validity window and
/// claim set — use when the window/claims are what the test is about;
/// otherwise prefer [`role_attestation`], whose window is wide enough that
/// validity never becomes an accidental variable.
pub fn role_attestation_windowed(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
    valid_from: SystemTime,
    valid_until: SystemTime,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims,
        valid_from,
        valid_until,
        signature: Signature::from_bytes([0u8; 64]),
    };
    // Sign the canonical bytes of attestation-minus-signature — what `validate_inbound_op` re-derives and checks against.
    let sb = att
        .signing_bytes()
        .expect("attestation must canonicalise for signing");
    att.signature = sign_event(&sb, issuer_sk).expect("attestation must sign");
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

/// Mints a signed [`RoleAttestation`] unambiguously in-window (a year ago
/// to a decade out, no claims) — the right default whenever the test's
/// subject isn't attestation validity itself.
pub fn role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    let now = SystemTime::now();
    role_attestation_windowed(
        issuer_sk,
        issuer_pk,
        subject,
        channel,
        role,
        now - WIDE_PAST,
        now + WIDE_FUTURE,
        vec![],
    )
}

/// Mints a signed [`SignedOp`] carrying `payload`. `seq` is the author's
/// per-artifact sequence number — advance it for tests sending more than
/// one op from the same author, since the validator rejects replays and gaps.
#[allow(clippy::too_many_arguments)]
pub fn signed_op(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    op_class: OpClass,
    payload: Vec<u8>,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact,
        author_pk,
        attestation_id,
        op_class,
        SystemTime::now(),
        seq,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}

/// [`signed_op`] with encryption metadata attached, for tests pushing an op through the MLS-wrapped path.
#[allow(clippy::too_many_arguments)]
pub fn signed_op_encrypted(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    op_class: OpClass,
    payload: Vec<u8>,
    seq: u64,
    encryption: EncryptionMetadata,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned_with_encryption(
        payload,
        artifact,
        author_pk,
        attestation_id,
        op_class,
        SystemTime::now(),
        seq,
        encryption,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}
