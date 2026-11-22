//! The invocation a change a host made for a caller keeps, as a peer reading
//! the change checks it.
//!
//! A device signs each invocation it sends a host with its own key, and a host
//! that runs one for a caller on another device keeps it in the change's
//! history entry (`tilo_actions::ActionContext::signed`). Every device that
//! takes the change in checks it here: the signature, that it is this
//! workspace's and the change's own invocation, the argument against the
//! signed hash, the certificate's chain and validity when it signed, its
//! revocation, a grant's reach, and that the person who signed is the one the
//! change names as its invoker. What the envelope proves, and what it does
//! not, is in docs/security/architecture.md ("Signed invocations").

use tilo_actions::remote::signed_invocation::StatementMismatch;
use tilo_server_protocol::invocation::{InvocationError, check_signature, signer_of};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Check the invocation `context` keeps, as a peer reading its change in
/// `workspace` does, and return the certificate of the caller who asked.
/// `Ok(None)` for a change that keeps none: the device made it of its own
/// accord, or for someone who asked under no key of theirs.
///
/// Freshness is the host's to check when it runs the invocation: a change is
/// read long after.
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
