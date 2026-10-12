//! Attenuable capability invite tokens.
//!
//! An invite is a **signed, delegable capability**: it grants `{subject
//! identity, workspace, role, optionally one sub-resource, optionally
//! time-boxed}`, handed to a person to join a workspace with that role
//! preset — the "ad-hoc / limited invite" tier.
//!
//! The defining property is **monotone attenuation**: a holder may mint a
//! *child* token that lowers the role, shortens validity, or
//! pins to a sub-resource — but **never** one that adds authority the
//! parent lacked. Widening is rejected at mint time
//! ([`InviteError::Widens`]) and, redundantly, at verification (a forged
//! wider child fails the chain check) — object-capability discipline.
//!
//! ## Chain shape
//!
//! The **root** token is signed by a workspace root authority and has
//! `parent: None`. Each **derived** token is signed by the previous
//! token's `subject` and carries `parent: Some(previous signature)`, so
//! the link cannot be spliced onto a different parent.
//!
//! [`verify_chain`] walks root-first, checking each link's signature,
//! that every step narrows, and that no token has expired.

use web_time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::CanonicalError;
use crate::channel::ArtifactId;
use crate::crypto::{Ed25519PublicKey, Signature, VerifyError, sign_event, verify_event};
use crate::role::Role;

/// Signing-domain tag mixed into every invite's signed bytes so a
/// signature is unforgeable across structure types. Frozen once invites
/// ship on the wire.
pub const INVITE_DOMAIN_TAG: &str = "tilo-invite-v1";

/// A signed capability grant into a workspace.
///
/// `role` is the role preset the token admits its subject with; attenuation
/// only ever lowers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteToken {
    /// The identity this invite grants authority to.
    pub subject: Ed25519PublicKey,
    /// The workspace the grant is scoped to. Every token in a chain must name
    /// the same workspace.
    pub workspace: u64,
    /// The role preset, narrowed monotonically down the chain
    /// (`Owner ≥ Editor ≥ Commenter ≥ Viewer`).
    pub role: Role,
    /// Optional single sub-resource pin. `None` means the whole workspace. A
    /// child may pin an unpinned parent or keep the same pin, but may not
    /// unpin or repoint to a different artifact.
    pub sub_resource: Option<ArtifactId>,
    /// Optional expiry; `None` means "until revoked". A child's expiry may
    /// not be later than its parent's.
    pub not_after: Option<SystemTime>,
    /// The identity that signed this token — a root authority for the root
    /// token, the parent's `subject` for a derived one.
    pub issuer: Ed25519PublicKey,
    /// Link to the parent token's signature (`None` for a root token),
    /// covered by this token's own signature so a link cannot be re-parented.
    pub parent: Option<Signature>,
    /// Detached signature by `issuer` over the domain-tagged canonical view
    /// of every other field.
    pub signature: Signature,
}

/// The desired grant for a fresh or derived token — everything except
/// issuer/parent/signature, supplied by the mint call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteSpec {
    /// See [`InviteToken::subject`].
    pub subject: Ed25519PublicKey,
    /// See [`InviteToken::workspace`].
    pub workspace: u64,
    /// See [`InviteToken::role`].
    pub role: Role,
    /// See [`InviteToken::sub_resource`].
    pub sub_resource: Option<ArtifactId>,
    /// See [`InviteToken::not_after`].
    pub not_after: Option<SystemTime>,
}

/// Failure modes for minting / attenuating / verifying invites.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum InviteError {
    /// A derived token tried to grant authority the parent lacked (higher
    /// role, widened resource, later expiry) — attenuation must only narrow.
    #[error("derived invite widens the parent grant: {0}")]
    Widens(&'static str),
    /// The delegator's key doesn't match the parent token's subject — only
    /// the holder of a token may attenuate it.
    #[error("delegator is not the parent token's subject")]
    NotHolder,
    /// A token in the chain is scoped to a different workspace than the root.
    #[error("workspace mismatch within the invite chain")]
    WorkspaceMismatch,
    /// The chain was empty, mis-linked, or a link's issuer didn't match the
    /// previous subject.
    #[error("broken invite chain: {0}")]
    BrokenChain(&'static str),
    /// The root token was signed by an identity not in the verifier's
    /// accepted root authorities.
    #[error("root invite issuer is not an accepted authority")]
    UntrustedRoot,
    /// A token's signature did not verify under its declared issuer.
    #[error("invite signature did not verify: {0}")]
    BadSignature(#[source] VerifyError),
    /// A token in the chain has expired at the verification time.
    #[error("invite has expired")]
    Expired,
    /// Canonical encoding failed. Effectively unreachable.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// The domain-tagged canonical view an invite signs over (everything but
/// the signature).
#[derive(Serialize)]
struct InviteView<'a> {
    tag: &'a str,
    subject: &'a Ed25519PublicKey,
    workspace: u64,
    role: &'a Role,
    sub_resource: &'a Option<ArtifactId>,
    not_after: &'a Option<SystemTime>,
    issuer: &'a Ed25519PublicKey,
    parent: &'a Option<Signature>,
}

impl InviteToken {
    fn view(&self) -> InviteView<'_> {
        InviteView {
            tag: INVITE_DOMAIN_TAG,
            subject: &self.subject,
            workspace: self.workspace,
            role: &self.role,
            sub_resource: &self.sub_resource,
            not_after: &self.not_after,
            issuer: &self.issuer,
            parent: &self.parent,
        }
    }

    /// Verify this token's own signature against its declared `issuer`. Does
    /// not check the chain, narrowing, or expiry — see [`verify_chain`].
    ///
    /// # Errors
    ///
    /// Returns [`InviteError::BadSignature`] if `signature` does not verify
    /// against `issuer` over this token's canonical view.
    pub fn verify_signature(&self) -> Result<(), InviteError> {
        verify_event(&self.view(), &self.signature, &self.issuer).map_err(InviteError::BadSignature)
    }

    /// (Re)sign this token in place with `sk`. Used by [`mint_root`] /
    /// [`attenuate`]; exposed so tests can prove [`verify_chain`] rejects a
    /// *validly-signed* widening, not merely a broken signature.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if canonicalising this token's view fails.
    pub fn sign_with(&mut self, sk: &ed25519_dalek::SigningKey) -> Result<(), CanonicalError> {
        self.signature = sign_event(&self.view(), sk)?;
        Ok(())
    }

    /// Is this token expired at `now`? `not_after: None` never expires.
    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        matches!(self.not_after, Some(t) if now >= t)
    }
}

/// Does `child_expiry` end no later than `parent_expiry`? (`None` =
/// "never", so a child may only tighten.)
fn expiry_le(child: Option<SystemTime>, parent: Option<SystemTime>) -> bool {
    match (child, parent) {
        (_, None) => true,        // parent never expires — any child ok.
        (None, Some(_)) => false, // parent expires; child claims never.
        (Some(c), Some(p)) => c <= p,
    }
}

/// Does `child`'s sub-resource pin narrow (or equal) `parent`'s? An
/// unpinned parent may be pinned by a child; a pinned parent may only
/// keep the same pin.
fn subresource_narrows(child: Option<ArtifactId>, parent: Option<ArtifactId>) -> bool {
    match (parent, child) {
        (None, _) => true,            // parent whole-workspace — child may pin.
        (Some(p), Some(c)) => p == c, // parent pinned — child must match.
        (Some(_), None) => false,     // parent pinned — child may not unpin.
    }
}

/// Check that `spec` is an attenuation (narrowing) of `parent`. Returns
/// the specific widening on failure.
fn check_narrows(spec: &InviteSpec, parent: &InviteToken) -> Result<(), InviteError> {
    if spec.workspace != parent.workspace {
        return Err(InviteError::WorkspaceMismatch);
    }
    if !parent.role.includes(spec.role) {
        return Err(InviteError::Widens("role higher than parent"));
    }
    if !expiry_le(spec.not_after, parent.not_after) {
        return Err(InviteError::Widens("expiry later than parent"));
    }
    if !subresource_narrows(spec.sub_resource, parent.sub_resource) {
        return Err(InviteError::Widens("sub-resource broader than parent"));
    }
    Ok(())
}

/// Mint a **root** invite, signed by a workspace root authority. The
/// signer must be a key the eventual verifier lists in its accepted root
/// authorities. `parent` is `None`.
///
/// # Errors
///
/// Returns [`InviteError::Canonical`] if canonicalising the token's signed
/// view fails.
pub fn mint_root(
    spec: InviteSpec,
    authority_sk: &ed25519_dalek::SigningKey,
) -> Result<InviteToken, InviteError> {
    let issuer = Ed25519PublicKey::from_bytes(authority_sk.verifying_key().to_bytes());
    let mut token = InviteToken {
        subject: spec.subject,
        workspace: spec.workspace,
        role: spec.role,
        sub_resource: spec.sub_resource,
        not_after: spec.not_after,
        issuer,
        parent: None,
        signature: Signature::from_bytes([0u8; 64]),
    };
    token.signature = sign_event(&token.view(), authority_sk)?;
    Ok(token)
}

/// Attenuate a `parent` token into a narrower child, signed by the
/// parent's holder (`delegator_sk` must derive `parent.subject`). Rejects
/// widening ([`InviteError::Widens`]) or a non-holder signer
/// ([`InviteError::NotHolder`]). The child links via the parent's
/// signature so the chain is tamper-evident.
///
/// # Errors
///
/// Returns [`InviteError::NotHolder`] if `delegator_sk` doesn't derive
/// `parent.subject`, [`InviteError::Widens`] if `spec` grants more than
/// `parent`, or [`InviteError::Canonical`] if signing fails.
pub fn attenuate(
    parent: &InviteToken,
    spec: InviteSpec,
    delegator_sk: &ed25519_dalek::SigningKey,
) -> Result<InviteToken, InviteError> {
    let issuer = Ed25519PublicKey::from_bytes(delegator_sk.verifying_key().to_bytes());
    if issuer != parent.subject {
        return Err(InviteError::NotHolder);
    }
    check_narrows(&spec, parent)?;
    let mut token = InviteToken {
        subject: spec.subject,
        workspace: spec.workspace,
        role: spec.role,
        sub_resource: spec.sub_resource,
        not_after: spec.not_after,
        issuer,
        parent: Some(parent.signature),
        signature: Signature::from_bytes([0u8; 64]),
    };
    token.signature = sign_event(&token.view(), delegator_sk)?;
    Ok(token)
}

/// Verify a full invite chain at time `now`. `chain[0]` is the root;
/// `chain[i]` is a child of `chain[i-1]`. Checks every token's signature,
/// non-expiry, and that each derived token genuinely narrows and links to
/// its parent. The root must be signed by an identity in `root_authorities`.
///
/// Returns the effective granted [`InviteToken`] (the last, tightest
/// link) on success.
///
/// # Errors
///
/// Returns [`InviteError::BrokenChain`] if `chain` is empty or mis-linked,
/// [`InviteError::UntrustedRoot`] if the root's issuer isn't in
/// `root_authorities`, [`InviteError::BadSignature`] if any link's
/// signature fails to verify, [`InviteError::Widens`] if a child grants
/// more than its parent, or [`InviteError::Expired`] if any link has
/// expired by `now`.
pub fn verify_chain<'a>(
    chain: &'a [InviteToken],
    now: SystemTime,
    root_authorities: &[Ed25519PublicKey],
) -> Result<&'a InviteToken, InviteError> {
    let root = chain
        .first()
        .ok_or(InviteError::BrokenChain("empty chain"))?;

    // Root: trusted authority, no parent link, valid signature.
    if root.parent.is_some() {
        return Err(InviteError::BrokenChain("root token carries a parent link"));
    }
    if !root_authorities.contains(&root.issuer) {
        return Err(InviteError::UntrustedRoot);
    }
    root.verify_signature()?;
    if root.is_expired_at(now) {
        return Err(InviteError::Expired);
    }

    for (parent, token) in chain.iter().zip(chain.iter().skip(1)) {
        // The delegator (this token's issuer) must be the parent's holder.
        if token.issuer != parent.subject {
            return Err(InviteError::BrokenChain(
                "issuer is not the parent's subject",
            ));
        }
        // The parent link must point at the parent's signature.
        if token.parent != Some(parent.signature) {
            return Err(InviteError::BrokenChain("parent link mismatch"));
        }
        // Signature first, so the narrowing check below is proven to
        // reject *validly-signed* widenings, not merely tampered bytes.
        token.verify_signature()?;
        // Each step must genuinely narrow: a child that was correctly
        // signed but grants more than its parent is caught here.
        check_narrows(
            &InviteSpec {
                subject: token.subject,
                workspace: token.workspace,
                role: token.role,
                sub_resource: token.sub_resource,
                not_after: token.not_after,
            },
            parent,
        )?;
        if token.is_expired_at(now) {
            return Err(InviteError::Expired);
        }
    }

    // `chain` was already confirmed non-empty above, so this always succeeds.
    chain.last().ok_or(InviteError::BrokenChain("empty chain"))
}
