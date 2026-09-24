//! Private native control actor. Only the actor's actual HostIdentity signs;
//! asynchronous workers carry exact prepared bytes and opaque local tickets.
mod authority;
mod devices;
mod observer;
mod session;
pub(crate) use session::Factory;
#[cfg(test)]
mod audio_tests;
mod http;
mod protocol;
mod registry;
#[cfg(test)]
mod tests;

use crate::shell::{
    native_media::CallTarget,
    native_workspace_owner::{NativeBootstrap, NativeFenceSubscription, NativeWorkspaceOwner},
    state::RoomId,
};
use parking_lot::Mutex;
use qos_moq::client::{Client, Connection, ConnectionScope};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tilo_relay::delivery_v3::media_wire;

pub(crate) use registry::Registry;

/// The fields and operation variants stay inside this module. Apps cannot
/// construct signing requests or substitute an owner on an existing ticket.
pub(crate) struct Command {
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
    Advance {
        ticket: Arc<Ticket>,
        serial: u64,
        body: Vec<u8>,
    },
    Act {
        ticket: Arc<Ticket>,
        action: Action,
    },
    End {
        ticket: Arc<Ticket>,
    },
}
#[derive(Clone, Copy)]
enum Action {
    Heartbeat,
    Renew,
    Relay,
    Refresh,
    CloseRelay,
    Leave,
    Abandon,
    RetireCarrier,
    Bootstrap,
}
enum Reply {
    Opened(Arc<Ticket>, Work),
    Work(Work),
    Grant,
    Carrier(Connection),
    Presence,
    Refreshed,
    Closed,
    Left,
}
struct Work {
    serial: u64,
    request: protocol::PreparedRequest,
}
enum Phase {
    Discover { origin: String },
    Bootstrap(NativeBootstrap),
    Control(protocol::NativeControl),
    Transition,
}
struct TimedGrant {
    view: protocol::GrantView,
    expires: Instant,
    presence: Instant,
}
struct Carrier {
    protocol_id: u32,
    scope: ConnectionScope,
    expires: Instant,
}
struct State {
    phase: Phase,
    serial: u64,
    pending: Option<(u64, u32)>,
    call: String,
    grant: Option<TimedGrant>,
    carrier: Option<Carrier>,
    generation: u64,
    wall_high_water: u64,
    bootstrap_at: Instant,
    descriptor_expires: Instant,
    custody_expires: Option<Instant>,
}
/// Stop callbacks use only this independent slot, never the protocol or service
/// mutex. The weak client avoids a Client -> Authority -> Ticket ownership cycle.
#[derive(Default)]
struct Stops {
    client: Weak<Client>,
    capture: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}
struct Ticket {
    owner: NativeWorkspaceOwner,
    room: RoomId,
    target: CallTarget,
    alive: AtomicBool,
    leaving: AtomicBool,
    // Set only around the synchronous finish of this ticket's opaque bootstrap.
    own_bootstrap: AtomicBool,
    own_continuity: AtomicU64,
    fences: AtomicU64,
    base: Instant,
    capture_until: AtomicU64,
    state: Mutex<State>,
    stops: Mutex<Stops>,
    subscription: Mutex<Option<NativeFenceSubscription>>,
}
impl Ticket {
    fn stop_resources(&self, terminal: bool) {
        let stops = self.stops.lock();
        if let Some(client) = stops.client.upgrade() {
            client.close();
        }
        if let Some(stop) = &stops.capture {
            stop(terminal);
        }
    }
    fn pause(&self) {
        self.stop_resources(false);
    }
    fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.stop_resources(true);
    }
    fn fence(&self, continuity: u64) {
        self.fences.fetch_add(1, Ordering::SeqCst);
        let own = self.own_bootstrap.load(Ordering::SeqCst)
            && continuity == self.own_continuity.load(Ordering::SeqCst);
        if !own {
            self.alive.store(false, Ordering::SeqCst);
        }
        self.stop_resources(!own);
    }
    fn live(&self) -> Result<(), String> {
        self.alive
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or("native media intent ended".into())
    }
    fn completion_live(&self) -> Result<(), String> {
        if self.leaving.load(Ordering::SeqCst) {
            Ok(())
        } else {
            self.live()
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.stop();
    }
}

fn now_ns() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "native clock precedes epoch".to_string())?
        .as_nanos()
        .try_into()
        .map_err(|_| "native clock overflow".into())
}
fn namespace(value: media_wire::MediaNamespace) -> String {
    format!(
        "{}/{}",
        hex::encode(value.call),
        hex::encode(value.publisher)
    )
}
fn deadline(now: Instant, ns: u64) -> Result<Instant, String> {
    now.checked_add(Duration::from_nanos(ns))
        .ok_or("native media deadline overflow".into())
}
