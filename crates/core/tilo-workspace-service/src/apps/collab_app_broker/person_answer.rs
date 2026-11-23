//! The answer a server made for its person, taken for the page action it was
//! asked about — and, when it is not, why.
//!
//! A page action a key may operate without asking its person reaches the window
//! carrying the answer the server that certified the device made for them
//! (`Confirmation::Approved`). Only the key that issued this device's
//! certificate answers for it, and only for the exact invocation, provider,
//! resource, action and argument it was made for: another server's word, or a
//! changed action, is no answer at all, and the window holds the action for the
//! person as it does any other it may not take.

use super::ApprovalBinding;

/// Whether `token` is the answer of the host that certified this device, for
/// exactly `binding`. What it could not take is said in the log, since an
/// answer not taken is the person being asked again about an action they
/// already answered.
pub(super) fn taken(token: &str, binding: &ApprovalBinding) -> bool {
    let Some(approval) = tilo_user_identity::HostApproval::decode(token) else {
        tracing::warn!(
            action = %binding.action,
            "an answer the person gave for a page action is not one this build reads"
        );
        return false;
    };
    let Ok((_, cert)) = tilo_identity::trusted_device::current(&binding.workspace) else {
        tracing::warn!(
            action = %binding.action,
            workspace = %binding.workspace,
            "an answer the person gave for a page action names a server this device has no identity of its own to check it against"
        );
        return false;
    };
    let stands = approval.stands_for(&cert, tilo_clock::unix_now_ns());
    let same_invocation = approval.invocation == *binding.invocation.0.as_bytes();
    let same_provider = approval.provider == binding.provider;
    let same_resource = approval.resource == binding.resource;
    let same_action = approval.action == binding.action;
    let same_argument = approval.argument == binding.argument_digest;
    let approved =
        stands && same_invocation && same_provider && same_resource && same_action && same_argument;
    if !approved {
        tracing::warn!(
            action = %binding.action,
            stands,
            same_invocation,
            same_provider,
            same_resource,
            same_action,
            same_argument,
            "an answer the person gave for a page action was not taken for this one"
        );
    }
    approved
}
