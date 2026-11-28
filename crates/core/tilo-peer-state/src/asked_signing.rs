//! A governance change a device makes in a workspace another device keeps: that host answers the
//! first ask with what the device signs it on ([`ActionCallError::SignatureRequired`]), the
//! device signs the change it asked for itself and nothing else, and asks again with it.

use collab_types::signer::DeviceSigner;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, SIGNED_ARGUMENT, SignFacts,
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use crate::own_admission::NOT_ASKED;
use crate::workspace_membership::WorkspaceMembershipCommand;

/// How long a device has to sign a change its host asks it for: a signer that answers outside it
/// (a passkey, a hardware key) has this long. A call its person said yes to waits as long for the
/// device that asked it, and ends in [`NOT_SIGNED_IN_TIME`] after.
pub const SIGN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// What a person reads when the device did not sign in time.
pub const NOT_SIGNED_IN_TIME: &str =
    "This device didn't sign the change in time, so nothing changed. Try again.";

/// What `receipt`, a host's answer, asks its device to sign, when it asks it.
#[must_use]
pub fn sign_asked(receipt: &InvocationReceipt) -> Option<&SignFacts> {
    match &receipt.status {
        ReceiptStatus::Rejected {
            error: ActionCallError::SignatureRequired(facts),
        } => Some(facts),
        _ => None,
    }
}

/// [`sign_asked`] of a host's answer as it travels.
#[must_use]
pub fn sign_asked_in(frame: &[u8]) -> Option<(InvocationId, SignFacts)> {
    let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(frame).ok()?.message
    else {
        return None;
    };
    sign_asked(&receipt)
        .cloned()
        .map(|facts| (receipt.invocation, facts))
}

/// Whether `frame`, a host's answer to `request`, accepted a change its device signs without
/// answering it yet: one that waits for its person's yes, after which the host asks the device to
/// sign it ([`SignFacts::awaited`](tilo_actions::remote::SignFacts::awaited)).
#[must_use]
pub fn accepted_change(request: &InvokeRequest, frame: &[u8]) -> bool {
    let accepted = matches!(
        RemoteActionFrame::decode(frame).map(|frame| frame.message),
        Ok(RemoteActionMessage::Receipt(InvocationReceipt {
            status: ReceiptStatus::Accepted,
            ..
        }))
    );
    accepted
        && (tilo_actions::remote::workspace_governance_operation(&request.target.address).is_some()
            || request.target.address.action
                == tilo_server_protocol::workspaces::ADMINISTER_WORKSPACES)
}

/// `request` again, as a new invocation carrying the change it asks for, which `signer` signs on
/// `facts`. The change the host names must be the one `request` asks for: a device signs nothing
/// it did not ask for. A request that carried the device's signature carries it again, for the
/// same scope.
///
/// # Errors
/// `request` asks no change a device makes, the host named another ([`NOT_ASKED`]), or the
/// device could not sign it.
pub async fn signed_again(
    request: &InvokeRequest,
    facts: &SignFacts,
    (cert, signer): (&DeviceCert, &dyn DeviceSigner),
    now: SystemTime,
) -> Result<InvokeRequest, String> {
    let mut arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => serde_json::Map::new(),
    };
    let answered: WorkspaceMembershipCommand =
        serde_json::from_value(facts.change.clone()).map_err(|_| NOT_ASKED.to_owned())?;
    if !asked_for(request, &arguments, &answered)? {
        return Err(NOT_ASKED.to_owned());
    }
    let changes = signed_on(facts, &answered, (cert, signer), now).await?;
    let mut again = request.clone();
    again.invocation = InvocationId::new();
    arguments.insert(
        SIGNED_ARGUMENT.to_owned(),
        serde_json::Value::Array(
            changes
                .iter()
                .map(|change| serde_json::Value::String(hex::encode(change)))
                .collect(),
        ),
    );
    again.argument = InvokeArgument::Json(arguments);
    // It names a new invocation and carries new arguments: the signature the first ask carried
    // covers neither.
    if let Some(scope) = again
        .signed
        .take()
        .map(|signed| signed.statement().workspace.clone())
    {
        again.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation(
                &scope,
                &again,
                cert,
                signer,
                tilo_clock::unix_now_ns(),
            )
            .await
            .map_err(|error| format!("this device could not sign the request: {error}"))?,
        ));
    }
    Ok(again)
}

/// Whether `answered` is the change `request`, with `arguments`, asks for: a workspace's
/// governance action ([`WorkspaceMembershipCommand::asked_by`]), or a page's `/workspaces`
/// request to its host ([`WorkspaceMembershipCommand::of_page_request`]).
fn asked_for(
    request: &InvokeRequest,
    arguments: &serde_json::Map<String, serde_json::Value>,
    answered: &WorkspaceMembershipCommand,
) -> Result<bool, String> {
    if let Some((_, verb)) =
        tilo_actions::remote::workspace_governance_operation(&request.target.address)
    {
        // What the action's address names (`members/<person>#remove`) is one of its arguments.
        let mut arguments = arguments.clone();
        if let Some((name, value)) =
            tilo_actions::remote::workspace_named_argument(&request.target.address)
        {
            arguments.insert(name.to_owned(), value.into());
        }
        return answered.asked_by(verb, &arguments);
    }
    if request.target.address.action == tilo_server_protocol::workspaces::ADMINISTER_WORKSPACES {
        let asked = tilo_server_protocol::workspaces::administered_request(arguments)?;
        return Ok(WorkspaceMembershipCommand::of_page_request(&asked).as_ref() == Some(answered));
    }
    Ok(false)
}

/// `command`, checked to be the change its device asked for, signed by that device on `facts`,
/// the member list the host answered with ([`crate::own_admission::sign_command`]).
///
/// # Errors
/// The member list does not read, or the device could not sign the change.
pub async fn signed_on(
    facts: &SignFacts,
    command: &WorkspaceMembershipCommand,
    device: (&DeviceCert, &dyn DeviceSigner),
    now: SystemTime,
) -> Result<Vec<Vec<u8>>, String> {
    let governance = facts
        .governance
        .iter()
        .map(hex::decode)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "the host's member list does not read".to_owned())?;
    crate::own_admission::sign_command(&facts.workspace, &governance, command, device, now).await
}

/// `frame`, the host's answer to the invocation asked again ([`signed_again`]), as its answer to
/// `invocation`, the one first asked: the one its asker waits on.
#[must_use]
pub fn answered_as(frame: Vec<u8>, invocation: InvocationId) -> Vec<u8> {
    let Ok(mut decoded) = RemoteActionFrame::decode(&frame) else {
        return frame;
    };
    match &mut decoded.message {
        RemoteActionMessage::Receipt(receipt) => receipt.invocation = invocation,
        _ => return frame,
    }
    decoded.encode().unwrap_or(frame)
}

/// The host's answer to `invocation` refused in `reason`'s words: what its asker reads when the
/// device did not sign the change it was asked to.
#[must_use]
pub fn refused_frame(invocation: InvocationId, reason: String) -> Option<Vec<u8>> {
    RemoteActionFrame::new(RemoteActionMessage::Receipt(InvocationReceipt {
        invocation,
        status: ReceiptStatus::Rejected {
            error: ActionCallError::refused(reason),
        },
    }))
    .encode()
    .ok()
}
