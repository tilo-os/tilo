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

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, Attestation, AttestationId, ChannelId, Claim, Ed25519PublicKey, EncryptionMetadata,
    OpKind, Role, RoleAttestation, Signature, SignedOp, sign_event, sign_op,
};
use ed25519_dalek::SigningKey;

/// A year back — comfortably before any test's "now".
const WIDE_PAST: Duration = Duration::from_secs(31_536_000);
/// A decade forward — comfortably after it.
const WIDE_FUTURE: Duration = Duration::from_secs(315_360_000);

/// Mints a signed [`RoleAttestation`] with an explicit validity window and
/// claim set — use when the window/claims are what the test is about;
/// otherwise prefer [`role_attestation`], whose window is wide enough that
/// validity never becomes an accidental variable.
///
/// # Panics
///
/// Panics if `att` fails to canonicalise or sign — meaning the fixture
/// itself is broken, which should fail loudly rather than hand a test a
/// silently-invalid attestation.
#[allow(
    clippy::similar_names,
    reason = "issuer_sk/issuer_pk (secret vs public key) is the established key-pair naming \
              convention across this codebase; renaming would make the pairing less obvious, \
              not more"
)]
#[allow(
    clippy::expect_used,
    reason = "this crate exists purely to build valid signed test fixtures (see the module \
              doc); a failure here means the fixture itself is broken and must fail loudly, \
              not be silently propagated into whatever test happens to call this"
)]
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
#[allow(
    clippy::similar_names,
    reason = "issuer_sk/issuer_pk (secret vs public key) is the established key-pair naming \
              convention across this codebase; renaming would make the pairing less obvious, \
              not more"
)]
pub fn role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    let now = SystemTime::now();
    // `now - 1 year` / `now + 10 years` cannot realistically overflow
    // `SystemTime`'s representable range; the fallback just keeps this
    // fixture from panicking if it somehow ever did.
    let valid_from = now.checked_sub(WIDE_PAST).unwrap_or(now);
    let valid_until = now.checked_add(WIDE_FUTURE).unwrap_or(now);
    role_attestation_windowed(
        issuer_sk,
        issuer_pk,
        subject,
        channel,
        role,
        valid_from,
        valid_until,
        vec![],
    )
}

/// Mints a signed [`SignedOp`] carrying `payload`. `seq` is the author's
/// per-artifact sequence number — advance it for tests sending more than
/// one op from the same author, since the validator rejects replays and gaps.
///
/// # Panics
///
/// Panics if signing fails — meaning the fixture itself is broken, which
/// should fail loudly rather than hand a test a silently-unsigned op.
#[allow(
    clippy::similar_names,
    reason = "author_sk/author_pk (secret vs public key) is the established key-pair naming \
              convention across this codebase; renaming would make the pairing less obvious, \
              not more"
)]
#[allow(
    clippy::expect_used,
    reason = "this crate exists purely to build valid signed test fixtures (see the module \
              doc); a failure here means the fixture itself is broken and must fail loudly, \
              not be silently propagated into whatever test happens to call this"
)]
pub fn signed_op(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    kind: OpKind,
    payload: Vec<u8>,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact,
        author_pk,
        attestation_id,
        kind,
        SystemTime::now(),
        seq,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}

/// [`signed_op`] with encryption metadata attached, for tests pushing an op through the MLS-wrapped path.
///
/// # Panics
///
/// Panics if signing fails — meaning the fixture itself is broken, which
/// should fail loudly rather than hand a test a silently-unsigned op.
#[allow(
    clippy::similar_names,
    reason = "author_sk/author_pk (secret vs public key) is the established key-pair naming \
              convention across this codebase; renaming would make the pairing less obvious, \
              not more"
)]
#[allow(
    clippy::expect_used,
    reason = "this crate exists purely to build valid signed test fixtures (see the module \
              doc); a failure here means the fixture itself is broken and must fail loudly, \
              not be silently propagated into whatever test happens to call this"
)]
pub fn signed_op_encrypted(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    kind: OpKind,
    payload: Vec<u8>,
    seq: u64,
    encryption: EncryptionMetadata,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned_with_encryption(
        payload,
        artifact,
        author_pk,
        attestation_id,
        kind,
        SystemTime::now(),
        seq,
        encryption,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}
