//! Private native call actor. The governor of a room grants each
//! participation over the room's own relay session; the grant alone admits
//! this device to the relay's voice carrier. Workers hold opaque local
//! tickets; only the actor talks to the relay.
mod authority;
mod devices;
mod observer;
mod session;
pub(crate) use session::Factory;
#[cfg(test)]
mod audio_tests;
mod registry;
#[cfg(test)]
mod tests;

use crate::shell::{native_media::CallTarget, native_workspace_owner::NativeWorkspaceOwner};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_joined_room::voice::Participation as Ticket;
use tilo_moq::client::Client;

pub(crate) use registry::Registry;

/// The fields and operation variants stay inside this module. Apps cannot
/// address the relay or substitute an owner on an existing ticket.
pub struct Command {
    operation: Operation,
    reply: tokio::sync::oneshot::Sender<Result<Reply, String>>,
}
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeMediaCommand")
    }
}
enum Operation {
    Open {
        owner: NativeWorkspaceOwner,
        target: CallTarget,
    },
    Heartbeat {
        ticket: Arc<Ticket>,
    },
    End {
        ticket: Arc<Ticket>,
    },
}
enum Reply {
    Opened(Arc<Ticket>),
    Sent,
    Left,
}
/// How long the governor may take to grant a fresh participation.
const OPEN_DEADLINE: Duration = Duration::from_secs(10);
