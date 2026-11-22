//! Signing an invocation as its caller, and checking one as a host.
//!
//! [`SignedInvocation`] is the caller's statement of who asks for which action
//! on which resource of which workspace, with which arguments. A device signs
//! each invocation it sends a host with its own key ([`sign_invocation`]);
//! the host runs it as the verified signer ([`verify_invocation`]) and never
//! signs one in a caller's place. A change the host makes for it keeps it, and
//! a peer reading the change checks it there ([`verify_recorded`]). What the envelope proves, and what it does
//! not, is in docs/security/README.md ("Signed invocations").

// A page signs through its own key, a JavaScript object bound to its one thread, so the futures
// that await a signature never move to another thread.
#![cfg_attr(
    target_family = "wasm",
    allow(
        clippy::future_not_send,
        reason = "a page's signer is a JavaScript object and its signing futures run on the page's one thread"
    )
)]

use collab_types::signer::DeviceSigner;
use tilo_actions::remote::signed_invocation::{
    INVOCATION_SIGNATURE_TOLERANCE_NS, InvocationStatement, MAX_SIGNER_CERT_BYTES,
    StatementMismatch,
};
use tilo_actions::remote::{InvokeRequest, RemoteActionMessage};
use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

pub use tilo_actions::remote::SignedInvocation;

/// Why an invocation was not signed, or its signature not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvocationError {
    /// The request carries no signature, where one is required.
    #[error("the invocation is not signed by its caller")]
    Unsigned,
    /// The carried certificate does not decode, or is too large.
    #[error("the signed invocation's certificate does not decode")]
    Encoding,
    /// The certificate certifies another key than the one that signed.
    #[error("the signed invocation's certificate is another key's")]
    SignerMismatch,
    /// The signature does not verify under the signer's key.
    #[error("the invocation's signature does not verify")]
    Signature,
    /// The signature covers another request than the one sent with it.
    #[error("{0}")]
    Mismatch(#[from] StatementMismatch),
    /// The issue time is outside the freshness window.
    #[error("the signed invocation is outside its freshness window")]
    Stale,
    /// The signer's certificate does not verify, or is expired or not yet
    /// valid.
    #[error("the signer's certificate is not valid: {0}")]
    Certificate(DeviceCertError),
    /// The signer's person revoked its key.
    #[error("the signer's key is revoked")]
    Revoked,
    /// The signer is a grant that does not reach the workspace.
    #[error("the signer's grant does not reach this workspace")]
    OutOfScope,
    /// The same invocation id was signed for another request.
    #[error("the invocation id was already used for another request")]
    Replayed,
    /// The device's signer failed.
    #[error("the device signer failed: {0}")]
    Signer(String),
    /// A change keeps an invocation another person signed than the one it
    /// names as its invoker.
    #[error("the invocation a change keeps is signed by someone it does not name")]
    NotInvoker,
}

impl InvocationError {
    /// Stable code a host refuses with.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsigned => "invocation_unsigned",
            Self::Encoding
            | Self::SignerMismatch
            | Self::Signature
            | Self::Mismatch(_)
            | Self::Certificate(_) => "invocation_signature_invalid",
            Self::Stale => "invocation_stale",
            Self::Revoked => "invocation_signer_revoked",
            Self::OutOfScope => "invocation_out_of_scope",
            Self::Replayed => "invocation_replayed",
            Self::Signer(_) => "invocation_signer_failed",
            Self::NotInvoker => "invocation_not_invoker",
        }
    }
}

/// `request` in `workspace`, signed at `now_ns` by the key `cert` certifies,
/// which `signer` holds. Awaits the signer: a browser's key signs through a
/// promise.
///
/// # Errors
///
/// [`InvocationError::SignerMismatch`] when `signer` holds another key than
/// `cert`'s, [`InvocationError::Encoding`] for a certificate that does not
/// encode within its bound, [`InvocationError::Signer`] when the signer fails.
pub async fn sign_invocation(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<SignedInvocation, InvocationError> {
    let (statement, encoded_cert) = statement_for(workspace, request, cert, signer, now_ns)?;
    let signature = signer
        .sign(statement.signing_message())
        .await
        .map_err(|error| InvocationError::Signer(error.0))?;
    Ok(SignedInvocation::new(statement, signature, encoded_cert))
}

/// [`sign_invocation`] in this turn, for a signer that answers at once (a key
/// kept in memory). A signer that answers only later fails with
/// [`InvocationError::Signer`].
///
/// # Errors
///
/// As [`sign_invocation`].
pub fn sign_invocation_now(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<SignedInvocation, InvocationError> {
    let (statement, encoded_cert) = statement_for(workspace, request, cert, signer, now_ns)?;
    let signature = collab_types::signer::sign_now(signer, statement.signing_message())
        .map_err(|error| InvocationError::Signer(error.0))?;
    Ok(SignedInvocation::new(statement, signature, encoded_cert))
}

/// Sign the invocation `message` carries, if it carries one, as
/// [`sign_invocation`] does, replacing any signature it carried: a device signs
/// what it sends under its own key, never one an app handed it. Every other
/// message goes as it is.
///
/// # Errors
///
/// As [`sign_invocation`].
pub async fn sign_message(
    workspace: &str,
    message: &mut RemoteActionMessage,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(), InvocationError> {
    if let RemoteActionMessage::Invoke(request) = message {
        request.signed = None;
        request.signed = Some(Box::new(
            sign_invocation(workspace, request, cert, signer, now_ns).await?,
        ));
    }
    Ok(())
}

/// [`sign_message`] in this turn, for a signer that answers at once.
///
/// # Errors
///
/// As [`sign_invocation_now`].
pub fn sign_message_now(
    workspace: &str,
    message: &mut RemoteActionMessage,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(), InvocationError> {
    if let RemoteActionMessage::Invoke(request) = message {
        request.signed = None;
        request.signed = Some(Box::new(sign_invocation_now(
            workspace, request, cert, signer, now_ns,
        )?));
    }
    Ok(())
}

fn statement_for(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(InvocationStatement, Vec<u8>), InvocationError> {
    let key = signer.device_public_key();
    if key != cert.device_public_key {
        return Err(InvocationError::SignerMismatch);
    }
    let encoded = postcard::to_allocvec(cert).map_err(|_| InvocationError::Encoding)?;
    if encoded.len() > MAX_SIGNER_CERT_BYTES {
        return Err(InvocationError::Encoding);
    }
    Ok((
        InvocationStatement::of(workspace, request, now_ns, key),
        encoded,
    ))
}

/// Check that `request` in `workspace` is signed by its caller, and return
/// the caller's verified certificate. Checks, in order: the signature under
/// the signing key, that it covers this very request (workspace, invocation,
/// target, arguments, the invocation it continues), its freshness at
/// `now_ns`, the certificate (its chain to the person's root, an issuer's
/// included, and its validity at `now_ns`), its revocation in `revocations`,
/// and that a grant reaches the workspace. Whether the signer is a member
/// with the grants the action needs is the host's admission and policy, which
/// it decides for the certificate this returns.
///
/// # Errors
///
/// The [`InvocationError`] naming the first check that fails.
pub fn verify_invocation(
    workspace: &str,
    request: &InvokeRequest,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    let signed = request.signed.as_ref().ok_or(InvocationError::Unsigned)?;
    check_signature(signed)?;
    signed.statement().covers(workspace, request)?;
    if !signed.statement().fresh_at(now_ns) {
        return Err(InvocationError::Stale);
    }
    signer_of(signed, workspace, revocations, now_ns)
}

/// Check the invocation a change made for it keeps (`context`'s
/// [`tilo_actions::ActionContext::signed`]), as a peer reading the change in
/// `workspace` does, and return the certificate of the caller who asked.
/// `Ok(None)` for a change that keeps none: the host made it of its own
/// accord, or for someone who asked under no key of theirs.
///
/// Checks the signature, that it is this workspace's and the change's own
/// invocation ([`tilo_actions::ActionContext::source_invocation`]), the
/// argument kept beside it against the signed hash, the certificate's chain
/// and validity when it signed, its revocation in `revocations`, a grant's
/// reach, and that the person who signed is the one the change names as its
/// invoker. Freshness is the host's to check when it runs the invocation: a
/// change is read long after.
///
/// # Errors
///
/// The first check that fails, as [`InvocationError`]:
/// [`InvocationError::NotInvoker`] when another person signed than the one
/// the change names.
pub fn verify_recorded(
    context: &tilo_actions::ActionContext,
    workspace: &str,
    revocations: &RevocationList,
) -> Result<Option<DeviceCert>, InvocationError> {
    let Some(call) = context.signed.as_deref() else {
        return Ok(None);
    };
    let signed = &call.signed;
    check_signature(signed)?;
    let statement = signed.statement();
    if statement.workspace != workspace {
        return Err(StatementMismatch::Workspace.into());
    }
    if context.source_invocation != Some(statement.invocation) {
        return Err(StatementMismatch::Invocation.into());
    }
    if call.argument_signed() == Some(false) {
        return Err(StatementMismatch::Arguments.into());
    }
    let cert = signer_of(signed, workspace, revocations, statement.issued_at_ns)?;
    let person = hex::encode(cert.user_public_key);
    let names_signer = context.invoker.as_ref().is_some_and(|invoker| {
        [invoker.acting.person_id(), invoker.acting.acted_for_person()].contains(&Some(&person))
    });
    if !names_signer {
        return Err(InvocationError::NotInvoker);
    }
    Ok(Some(cert))
}

/// That `signed`'s signature verifies under the key it names.
fn check_signature(signed: &SignedInvocation) -> Result<(), InvocationError> {
    let statement = signed.statement();
    let key = ed25519_dalek::VerifyingKey::from_bytes(&statement.signer)
        .map_err(|_| InvocationError::Signature)?;
    key.verify_strict(
        &statement.signing_message(),
        &ed25519_dalek::Signature::from_bytes(signed.signature()),
    )
    .map_err(|_| InvocationError::Signature)
}

/// The certificate `signed` carries, checked: it certifies the signing key,
/// chains to its person's root and is valid at `at_ns`, is not revoked in
/// `revocations`, and a grant reaches `workspace`.
fn signer_of(
    signed: &SignedInvocation,
    workspace: &str,
    revocations: &RevocationList,
    at_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    let cert = decode_cert(signed.signer_cert())?;
    if cert.device_public_key != signed.statement().signer {
        return Err(InvocationError::SignerMismatch);
    }
    verify_device_cert_at(&cert, at_ns).map_err(InvocationError::Certificate)?;
    if revocations.refuses(&cert) {
        return Err(InvocationError::Revoked);
    }
    if cert
        .capabilities
        .agent
        .is_some_and(|scope| !scope.reaches(workspace))
    {
        return Err(InvocationError::OutOfScope);
    }
    Ok(cert)
}

fn decode_cert(bytes: &[u8]) -> Result<DeviceCert, InvocationError> {
    if bytes.len() > MAX_SIGNER_CERT_BYTES {
        return Err(InvocationError::Encoding);
    }
    match postcard::take_from_bytes::<DeviceCert>(bytes) {
        Ok((cert, [])) => Ok(cert),
        _ => Err(InvocationError::Encoding),
    }
}

/// The invocation ids a host admitted within the freshness window, so one id
/// names one request: the same id signed again for the same request by the
/// same key is a retry, which the host's invocation log answers with what the
/// first did, and the same id for anything else is a conflict.
#[derive(Debug, Default)]
pub struct InvocationReplay {
    seen: std::collections::HashMap<tilo_actions::InvocationId, Seen>,
}

#[derive(Debug)]
struct Seen {
    /// The request as signed, without its issue time.
    request: [u8; 32],
    /// When the last statement for it was issued.
    issued_at_ns: u64,
}

/// At most this many invocations are remembered; past it, the oldest go
/// first. The freshness window bounds what a forgotten one could replay.
const MAX_REMEMBERED: usize = 65_536;

impl InvocationReplay {
    /// Admit the verified `signed` at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`InvocationError::Replayed`] when its id was admitted within the
    /// window for another request or another signer.
    pub fn admit(&mut self, signed: &SignedInvocation, now_ns: u64) -> Result<(), InvocationError> {
        let statement = signed.statement();
        let floor = now_ns.saturating_sub(INVOCATION_SIGNATURE_TOLERANCE_NS);
        if self.seen.len() >= MAX_REMEMBERED {
            self.seen.retain(|_, seen| seen.issued_at_ns >= floor);
            if self.seen.len() >= MAX_REMEMBERED
                && let Some(oldest) = self
                    .seen
                    .iter()
                    .min_by_key(|(_, seen)| seen.issued_at_ns)
                    .map(|(id, _)| *id)
            {
                self.seen.remove(&oldest);
            }
        }
        let request = request_digest(statement);
        match self.seen.get_mut(&statement.invocation) {
            Some(seen) if seen.issued_at_ns >= floor && seen.request != request => {
                Err(InvocationError::Replayed)
            }
            Some(seen) => {
                seen.request = request;
                seen.issued_at_ns = seen.issued_at_ns.max(statement.issued_at_ns);
                Ok(())
            }
            None => {
                self.seen.insert(
                    statement.invocation,
                    Seen {
                        request,
                        issued_at_ns: statement.issued_at_ns,
                    },
                );
                Ok(())
            }
        }
    }
}

/// What a statement asks, whenever it was issued.
fn request_digest(statement: &InvocationStatement) -> [u8; 32] {
    let timeless = InvocationStatement {
        issued_at_ns: 0,
        ..statement.clone()
    };
    *blake3::hash(&timeless.signing_message()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
    use tilo_user_identity::{
        AgentScope, DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity,
        prepare_minted_grant, sign_device_cert, sign_revocation_entry,
    };

    const WORKSPACE: &str = "6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b";
    const NOW: u64 = 10_000_000_000_000;

    fn cert_of(root: &SigningKey, key: &SigningKey, expires_at_ns: Option<u64>) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: ActionAddress::new("note:1", "rename"),
            },
            argument: InvokeArgument::Text("Plan".into()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn signed(
        request: &InvokeRequest,
        cert: &DeviceCert,
        key: &SigningKey,
        at: u64,
    ) -> InvokeRequest {
        let mut request = request.clone();
        request.signed = Some(Box::new(
            sign_invocation_now(WORKSPACE, &request, cert, key, at).unwrap(),
        ));
        request
    }

    tilo_test_env::scenario! {
        fn a_host_takes_an_invocation_only_as_its_signer_signed_it;

        phase a_signed_invocation_verifies_as_its_signer;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let asked = signed(&request(), &cert, &key, NOW);
            let verified = verify_invocation(WORKSPACE, &asked, &RevocationList::new(), NOW).unwrap();
            assert_eq!(verified, cert);
        }

        phase a_changed_workspace_target_or_argument_is_refused;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let asked = signed(&request(), &cert, &key, NOW);
            let none = RevocationList::new();
            assert_eq!(
                verify_invocation("another", &asked, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Workspace))
            );
            let mut retargeted = asked.clone();
            retargeted.target.address.action = "delete".into();
            assert_eq!(
                verify_invocation(WORKSPACE, &retargeted, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Target))
            );
            let mut reargued = asked;
            reargued.argument = InvokeArgument::Text("Other".into());
            assert_eq!(
                verify_invocation(WORKSPACE, &reargued, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Arguments))
            );
            // A statement rewritten to match is no longer the one signed.
            let mut forged = reargued;
            let original = forged.signed.take().unwrap();
            let rewritten = InvocationStatement::of(WORKSPACE, &forged, NOW, original.statement().signer);
            forged.signed = Some(Box::new(SignedInvocation::new(
                rewritten,
                *original.signature(),
                original.signer_cert().to_vec(),
            )));
            assert_eq!(
                verify_invocation(WORKSPACE, &forged, &none, NOW),
                Err(InvocationError::Signature)
            );
            assert_eq!(
                verify_invocation(WORKSPACE, &request(), &none, NOW),
                Err(InvocationError::Unsigned)
            );
        }

        phase a_stale_expired_or_revoked_signer_is_refused;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let none = RevocationList::new();
            let cert = cert_of(&root, &key, None);
            let old = signed(&request(), &cert, &key, NOW - INVOCATION_SIGNATURE_TOLERANCE_NS - 1);
            assert_eq!(verify_invocation(WORKSPACE, &old, &none, NOW), Err(InvocationError::Stale));
            let expiring = cert_of(&root, &key, Some(NOW - 1));
            let expired = signed(&request(), &expiring, &key, NOW);
            assert_eq!(
                verify_invocation(WORKSPACE, &expired, &none, NOW),
                Err(InvocationError::Certificate(DeviceCertError::Expired))
            );
            let mut entry = RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: cert.device_public_key,
                revoked_at_ns: 500,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &root).unwrap();
            let mut revoked = RevocationList::new();
            revoked.insert(entry).unwrap();
            let asked = signed(&request(), &cert, &key, NOW);
            assert_eq!(
                verify_invocation(WORKSPACE, &asked, &revoked, NOW),
                Err(InvocationError::Revoked)
            );
            // Another key cannot sign under this certificate.
            let other = SigningKey::from_bytes(&[8; 32]);
            assert_eq!(
                sign_invocation_now(WORKSPACE, &request(), &cert, &other, NOW),
                Err(InvocationError::SignerMismatch)
            );
        }

        phase a_minted_key_carries_its_issuer_and_reaches_only_its_workspace;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let account = SigningKey::from_bytes(&[9; 32]);
            let mut issuer = cert_of(&root, &account, Some(NOW + 1_000_000_000));
            issuer.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
            sign_device_cert(&mut issuer, &root).unwrap();
            let minted_key = SigningKey::from_bytes(&[10; 32]);
            let workspace = *uuid::Uuid::parse_str(WORKSPACE).unwrap().as_bytes();
            let mut minted = prepare_minted_grant(
                &issuer,
                minted_key.verifying_key().to_bytes(),
                workspace,
                "script",
                2,
                NOW + 1_000_000_000,
            );
            sign_device_cert(&mut minted, &account).unwrap();
            let asked = signed(&request(), &minted, &minted_key, NOW);
            let none = RevocationList::new();
            let verified = verify_invocation(WORKSPACE, &asked, &none, NOW).unwrap();
            assert_eq!(verified.issuer.as_deref(), Some(&issuer));
            let mut elsewhere = request();
            let other = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
            elsewhere.signed = Some(Box::new(
                sign_invocation_now(other, &elsewhere, &minted, &minted_key, NOW).unwrap(),
            ));
            assert_eq!(
                verify_invocation(other, &elsewhere, &none, NOW),
                Err(InvocationError::OutOfScope)
            );
        }

        phase one_invocation_id_names_one_request;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let first = signed(&request(), &cert, &key, NOW);
            let mut replay = InvocationReplay::default();
            replay.admit(first.signed.as_ref().unwrap(), NOW).unwrap();
            // Signed again for the same request: a retry.
            let retry = signed(&first, &cert, &key, NOW + 1);
            replay.admit(retry.signed.as_ref().unwrap(), NOW + 1).unwrap();
            // The same id for another action.
            let mut other = first;
            other.target.address.action = "delete".into();
            let other = signed(&other, &cert, &key, NOW + 2);
            assert_eq!(
                replay.admit(other.signed.as_ref().unwrap(), NOW + 2),
                Err(InvocationError::Replayed)
            );
        }
    }
}
