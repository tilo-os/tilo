//! What an application-reported update must satisfy before it advances a
//! pending invocation, checked against the contract the invocation was
//! published under.

/// Checks one application-reported update against its pending invocation's
/// published contract, returning the sequence it advances to.
pub(super) fn validate_invocation_update(
    pending: &super::PendingInvocation,
    update: &tilo_actions::InvocationUpdate,
) -> Result<u64, capnp::Error> {
    if update.invocation() != pending.update.invocation() {
        return Err(capnp::Error::failed(
            "invocation update identity mismatch".into(),
        ));
    }
    let mut next_sequence = pending.last_sequence;
    if matches!(update, tilo_actions::InvocationUpdate::Accepted { .. }) {
        return Err(capnp::Error::failed(
            "invocation acceptance is owned by the broker".into(),
        ));
    }
    if let tilo_actions::InvocationUpdate::Progress {
        sequence,
        phase,
        completed,
        total,
        ..
    } = update
    {
        if matches!(pending.execution, tilo_actions::ExecutionMode::Immediate) {
            return Err(capnp::Error::failed(
                "immediate action reported background progress".into(),
            ));
        }
        if *sequence <= pending.last_sequence {
            return Err(capnp::Error::failed(
                "invocation progress must be monotonic".into(),
            ));
        }
        if phase.trim().is_empty() {
            return Err(capnp::Error::failed(
                "invocation progress phase must be non-empty".into(),
            ));
        }
        if completed
            .zip(*total)
            .is_some_and(|(completed, total)| completed > total)
        {
            return Err(capnp::Error::failed(
                "invocation progress cannot exceed its total".into(),
            ));
        }
        next_sequence = *sequence;
    }
    if let tilo_actions::InvocationUpdate::Item {
        sequence, value, ..
    } = update
    {
        let Some(item_schema) = pending.execution.item_schema() else {
            return Err(capnp::Error::failed(
                "non-streaming action reported a streamed item".into(),
            ));
        };
        if *sequence <= pending.last_sequence {
            return Err(capnp::Error::failed(
                "invocation stream sequence must be monotonic".into(),
            ));
        }
        tilo_actions::validate_json_schema(value, item_schema).map_err(|error| {
            capnp::Error::failed(format!(
                "streamed item violated its published contract: {error}"
            ))
        })?;
        next_sequence = *sequence;
    }
    if let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update {
        pending.result.validate(output).map_err(|error| {
            capnp::Error::failed(format!(
                "invocation result violated its published contract: {error}"
            ))
        })?;
    }
    if let tilo_actions::InvocationUpdate::Failed { error, .. } = update {
        if error.code.trim().is_empty() || error.message.trim().is_empty() {
            return Err(capnp::Error::failed(
                "invocation failure requires a code and message".into(),
            ));
        }
        if error.retryable && !pending.retryable_on_disconnect {
            return Err(capnp::Error::failed(
                "at-most-once action cannot advertise a retryable failure".into(),
            ));
        }
    }
    if matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }) && !pending.cancellable {
        return Err(capnp::Error::failed(
            "non-cancellable action reported cancellation".into(),
        ));
    }
    Ok(next_sequence)
}
