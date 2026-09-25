//! Closed media operation projection; authorization remains Store::action.
use super::NativeOperation;
use qos_action_broker::collaboration_admin::CollaborationAdminOperation as Operation;

pub(super) fn operation(value: &NativeOperation) -> Option<Operation> {
    Some(match value {
        NativeOperation::MediaCreate(v) => Operation::MediaCreate(v.clone()),
        NativeOperation::MediaEnd(v) => Operation::MediaEnd(v.clone()),
        NativeOperation::MediaRestart(v) => Operation::MediaRestart(v.clone()),
        NativeOperation::MediaSetDirections(v) => Operation::MediaSetDirections(v.clone()),
        NativeOperation::MediaSetParticipantDenial(v) => {
            Operation::MediaSetParticipantDenial(v.clone())
        }
        _ => return None,
    })
}
