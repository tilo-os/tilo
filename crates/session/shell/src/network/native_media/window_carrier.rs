//! A room's window carrier on this device: its participation in the room's
//! `windows_call` and the `moq-lite` carrier the governor's grant admits.
//!
//! The governor of a room that shares its layout grants every member device
//! this carrier ([`tilo_relay_protocol::voice::windows_call`]). On it a
//! device publishes the windows of host programs it runs, one window track
//! each, only while some member's device asks for that track, and views the
//! windows of others it shows. Its participation is kept like a call's: the
//! same heartbeat, the same reconnects, the same carrier replacement when
//! the grant's scope changes ([`super::session::maintain`]).

use super::session::{Running, command, maintain};
use super::{
    Arc, AtomicBool, Client, Mutex, NativeWorkspaceOwner, OPEN_DEADLINE, Operation, Ordering, Reply,
};
use crate::network::NetCmd;
use crate::shell::state::RoomId;
use runtime_collab::native_media::{Issue, State as CallState};
use tokio::sync::mpsc;

/// This device on one room's window carrier, while it holds it. Dropping it
/// leaves the carrier.
pub(crate) struct WindowCarrier {
    running: Arc<Running>,
    room: RoomId,
}

impl WindowCarrier {
    /// Join `room`'s window carrier and keep it on the current runtime.
    pub(crate) async fn open(
        commands: mpsc::Sender<NetCmd>,
        owner: NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<Self, String> {
        let Reply::Opened(ticket) =
            command(&commands, Operation::OpenWindows { owner, room }).await?
        else {
            return Err("the window carrier did not open".into());
        };
        if let Err(error) = ticket.granted(OPEN_DEADLINE).await {
            ticket.stop();
            return Err(error);
        }
        let running = Arc::new(Running {
            ticket,
            client: Mutex::new(None),
            state: Mutex::new(Ok(CallState::Joining)),
            cancelled: AtomicBool::new(false),
            task: Mutex::new(None),
            commands: commands.clone(),
            executor: tokio::runtime::Handle::current(),
        });
        let controller = running.clone();
        let task = tokio::spawn(async move {
            let result = maintain(&commands, &controller).await;
            if !controller.cancelled.load(Ordering::Acquire) {
                if let Err(error) = &result {
                    tracing::warn!(%error, "window carrier maintenance ended");
                }
                controller.ticket.stop();
                *controller.state.lock() = result
                    .map(|()| CallState::Ended)
                    .map_err(|_| Issue::TransportFailed);
            }
        });
        *running.task.lock() = Some(task.abort_handle());
        Ok(Self { running, room })
    }

    /// The room whose carrier this is.
    pub(crate) const fn room(&self) -> RoomId {
        self.room
    }

    /// The carrier, while it is installed.
    pub(crate) fn client(&self) -> Option<Arc<Client>> {
        self.running.client.lock().clone()
    }

    /// Whether the participation ended: refused, lapsed or out of retries.
    /// An ended carrier is opened afresh.
    pub(crate) fn ended(&self) -> bool {
        self.running.ticket.live().is_err()
    }

    /// The namespace the device whose key is `device` publishes its windows
    /// under, as the current grant names it.
    pub(crate) fn namespace_of(&self, device: [u8; 32]) -> Option<String> {
        let grant = self.running.ticket.grant()?;
        grant
            .participants
            .iter()
            .find(|participant| participant.device_public_key == device && participant.publishes)
            .map(|participant| participant.namespace.clone())
    }
}

impl Drop for WindowCarrier {
    fn drop(&mut self) {
        let first = !self.running.cancelled.swap(true, Ordering::SeqCst);
        self.running.ticket.stop();
        let task = self.running.task.lock().take();
        if let Some(task) = task {
            task.abort();
        }
        let client = self.running.client.lock().take();
        if let Some(client) = client {
            client.close();
        }
        if first {
            let ticket = self.running.ticket.clone();
            let commands = self.running.commands.clone();
            // Best-effort leave; the governor ends a silent participation
            // anyway.
            self.running.executor.spawn(async move {
                let _ = command(&commands, Operation::End { ticket }).await;
            });
        }
    }
}
