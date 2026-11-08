//! The agent door toward the person's other devices.
//!
//! This device's endpoint answers [`tilo_runtime::remote_door::ALPN`]: an
//! agent on another of the person's own devices connects to this session's
//! agent door over iroh, and is asked about and served exactly as a program of
//! this login is ([`crate::agent_door`]), as an agent the person delegates to,
//! named after the device it came from. Which device dials is the transport's
//! to say, and whether it is one of the person's is their own store's
//! ([`tilo_runtime::remote_door::PersonalDevices`]); a connection from anyone
//! else is refused before the person is asked.
//!
//! The person may let such agents in without being asked here — on a box
//! nobody sits at — with this device's `agent_door` setting
//! ([`settings_store::agent_door`]), which is off unless they turn it on.
//!
//! The other way round, a `--connect --device NAME` client of this login asks
//! this session to carry it to another device's door ([`Remote::forward`]),
//! and an agent lists the person's devices with
//! [`tilo_runtime::remote_door::DEVICES_TOOL`].

use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_runtime::remote_door::{
    Arrival, Arrivals, DeviceDirectory, Doors, Forward, PersonsDevices, Reach, RemoteDoor,
};
use tilo_service::Wake;

/// What the session's agent door hears from the person's other devices, and how it reaches theirs.
pub(crate) struct Remote {
    /// Connections the door opened, waiting for the agent door's pass.
    arrivals: Mutex<Vec<Arrival>>,
    /// Wakes the session's loop when one arrives.
    wake: Wake,
    /// Whether the session's agent door takes what arrives: a session that serves no door refuses
    /// them at once.
    serving: AtomicBool,
    /// The person's other devices' doors, once this device's endpoint has bound and its store is
    /// open.
    doors: OnceLock<Arc<Doors>>,
}

impl Remote {
    pub(crate) fn new(wake: Wake) -> Arc<Self> {
        Arc::new(Self {
            arrivals: Mutex::default(),
            wake,
            serving: AtomicBool::new(false),
            doors: OnceLock::new(),
        })
    }

    /// The door this device answers the person's other devices' agents at,
    /// admitting those `devices` says are theirs.
    pub(crate) fn door(self: &Arc<Self>, devices: Arc<dyn PersonsDevices>) -> RemoteDoor {
        RemoteDoor::new(devices, Arc::clone(self) as Arc<dyn Arrivals>)
    }

    /// The protocol this device's endpoint answers the door on.
    pub(crate) fn protocol(
        self: &Arc<Self>,
        devices: Arc<dyn PersonsDevices>,
    ) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        (
            tilo_runtime::remote_door::ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(self.door(devices)),
        )
    }

    /// Reach the person's other devices through `doors` from now on.
    pub(crate) fn attach(&self, doors: Doors) {
        let _ = self.doors.set(Arc::new(doors));
    }

    /// The agent door serves what arrives from now on.
    pub(crate) fn serve(&self) {
        self.serving.store(true, Ordering::Relaxed);
    }

    /// Every connection that arrived since the last pass.
    pub(crate) fn take(&self) -> Vec<Arrival> {
        std::mem::take(&mut *self.arrivals.lock_recover())
    }

    /// Carry `stream` , whose client asked for `forward` , to that device's door. Blocks the
    /// calling thread for as long as the connection lasts.
    pub(crate) fn forward(&self, stream: UnixStream, forward: &Forward) {
        match self.doors.get() {
            Some(doors) => doors.serve_forward(stream, forward),
            None => tilo_runtime::remote_door::refuse_forward(
                stream,
                "this desktop reaches none of your other devices: it takes no part in the \
                 collaboration plane"
                    .into(),
            ),
        }
    }

    /// Whether the person lets an agent from another of their devices in without asking here, as
    /// this device's settings say now.
    pub(crate) fn accepts_own_devices() -> bool {
        settings_store::agent_door::accepts_own_devices(&settings_store::load())
    }
}

impl Arrivals for Remote {
    fn arrive(&self, arrival: Arrival) -> Result<(), String> {
        if !self.serving.load(Ordering::Relaxed) {
            return Err("this desktop serves no agents now".into());
        }
        self.arrivals.lock_recover().push(arrival);
        (self.wake)();
        Ok(())
    }
}

impl DeviceDirectory for Remote {
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>> {
        let doors = self.doors.get().cloned();
        Box::pin(async move {
            match doors {
                Some(doors) => doors.reach_now().await,
                None => Vec::new(),
            }
        })
    }
}
