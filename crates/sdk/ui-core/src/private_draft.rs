//! Private unsent editor adoption over the existing certified native channel.
//! Only the opaque token belongs in published action arguments and receipts.

pub use runtime_collab::private_draft::{Draft, Editor, Target};

fn request(
    token: uuid::Uuid,
    invocation: tilo_actions::InvocationId,
    acknowledge: bool,
) -> Result<Vec<u8>, String> {
    let command = serde_json::to_vec(&runtime_collab::private_draft::Request {
        private_draft: token,
        acknowledge,
    })
    .map_err(|_| "invalid private draft token")?;
    #[cfg(feature = "broker-transport")]
    {
        crate::collaboration::person_chat_command(command, invocation.0.as_bytes().to_vec())
    }
    #[cfg(not(feature = "broker-transport"))]
    {
        let _ = (command, invocation);
        Err("private drafts require an authenticated native host".into())
    }
}

/// Fetch without consuming. Failures leave both the source draft and token intact.
/// # Errors
/// Refuses unavailable transport, invalid original custody, expiry or malformed replies.
pub fn fetch(token: uuid::Uuid, invocation: tilo_actions::InvocationId) -> Result<Draft, String> {
    let bytes = request(token, invocation, false)?;
    if bytes.len() > 128 * 1024 {
        return Err("private editor reply exceeds bound".into());
    }
    let draft: Draft =
        serde_json::from_slice(&bytes).map_err(|_| "invalid private editor reply")?;
    draft.editor.validate()?;
    if let Target::Task {
        agent,
        task,
        run_epoch,
    } = &draft.target
    {
        tilo_actions::task_messaging::TaskRunAddress {
            agent_id: agent.clone(),
            task_id: task.clone(),
            run_epoch: *run_epoch,
        }
        .validate()?;
    }
    Ok(draft)
}

/// Call only after the retained editor and standard surface have adopted the draft.
/// An uncertain acknowledgement is not permission to discard the source draft.
/// # Errors
/// Refuses unavailable transport, invalid original custody, expiry or malformed replies.
pub fn acknowledge(
    token: uuid::Uuid,
    invocation: tilo_actions::InvocationId,
) -> Result<(), String> {
    let bytes = request(token, invocation, true)?;
    if bytes != b"null" {
        return Err("invalid private editor acknowledgement".into());
    }
    Ok(())
}
