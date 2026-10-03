//! The desktop command vocabulary, as the owner commit path speaks it.
//!
//! The commands themselves belong to the desktop model
//! ([`tilo_desktop_model::commands`]); this module adds only what needs the
//! owner commit request.
pub use tilo_desktop_model::commands::*;

/// The id of an owner desktop request, from its authenticated caller.
pub trait DesktopCommandIdOf: Sized {
    fn of(request: &crate::owner_commit::OwnerCommitRequest) -> Option<Self>;
}

impl DesktopCommandIdOf for DesktopCommandId {
    fn of(request: &crate::owner_commit::OwnerCommitRequest) -> Option<Self> {
        match &request.operation {
            crate::owner_commit::CommitOperation::DesktopCommand {
                invocation, caller, ..
            } => Some(Self {
                device: caller.device_public_key,
                invocation: *invocation,
            }),
            _ => None,
        }
    }
}
