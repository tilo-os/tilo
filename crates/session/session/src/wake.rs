//! Waking one of the person's sleeping devices from this one.
//!
//! A device that went to sleep off the network still answers a magic packet,
//! and whichever of the person's devices shares its LAN sends it: this one
//! when it does, one of its siblings otherwise ([`crate::platform::fabric::wake`]).
//! This mount is the session's side of that — which interfaces this machine
//! offers to be woken on, the packets it sends, the probes it answers, and
//! what the person is told once the device comes back or does not.
//!
//! The routes this machine offers travel with what it says of itself while it
//! is there, so its devices know whether a packet from here could reach a
//! sleeping one: they are read from `/sys/class/net` at most every
//! [`ROUTE_SCAN_INTERVAL`], and an interface that was last seen on a network
//! keeps that subnet while it is off it, or the machine would stop being
//! wakeable the moment it slept.
//!
//! Everything the fabric decides is in the crate's own module; here is only
//! what touches the machine: the socket, the interfaces, the clock.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::platform::fabric::SharedLink;
use crate::platform::fabric::wake::{
    self, MagicPackets, ROUTE_SCAN_INTERVAL, UdpMagicPackets, WakeEvent, WakePlan, Waker, WatchStep,
};
use crate::platform::fabric::{
    DeviceId, PersonalFabricAction, PersonalFabricActionEnvelope, WakeOutcome, WakeRoute,
};
use tilo_fabric::interfaces::{SysInterfaces, WakeInterfaces};

use crate::{Shared, notifications::SessionNotifications};

/// Where a session's wakes send their packets and what its interfaces offer,
/// so a test keeps both off the machine it runs on.
#[derive(Clone, Default)]
pub struct WakeIo {
    pub packets: Option<Arc<dyn MagicPackets>>,
    pub interfaces: Option<Arc<dyn WakeInterfaces>>,
}

/// This session's wakes: what it sends, what it answers and what it waits for. Called once per pass
/// of the session's loop.
pub(crate) struct Wake {
    /// The socket the packets leave through; a test's own, so no test broadcasts.
    packets: Arc<dyn MagicPackets>,
    interfaces: Arc<dyn WakeInterfaces>,
    waker: RefCell<Waker>,
    /// This machine's own wake routes, as its interfaces last showed them.
    routes: RefCell<Vec<WakeRoute>>,
    /// When the interfaces were last read.
    scanned: RefCell<Option<Instant>>,
}

impl Wake {
    /// The session's wakes over `io`: the real socket and this machine's interfaces unless a
    /// test brought its own.
    pub(crate) fn new(io: &WakeIo) -> Rc<Self> {
        Rc::new(Self {
            packets: io
                .packets
                .clone()
                .unwrap_or_else(|| Arc::new(UdpMagicPackets)),
            interfaces: io
                .interfaces
                .clone()
                .unwrap_or_else(|| Arc::new(SysInterfaces)),
            waker: RefCell::new(Waker::default()),
            routes: RefCell::new(Vec::new()),
            scanned: RefCell::new(None),
        })
    }

    /// Offer this machine's interfaces to the person's devices, probe the devices it woke, give up
    /// on the ones that do not come back, and tell the person how each of their requests ended.
    /// Called every pass.
    pub(crate) fn follow(&self, shared: &Shared) {
        let Some(fabric) = &shared.fabric else {
            return;
        };
        self.publish(fabric);
        // A device back on the person's fabric is awake: its session coming up is the answer a
        // probe would have asked for.
        let online = fabric.online_device_ids();
        let back = self.waker.borrow_mut().rejoins(&online);
        for step in back {
            self.carry_out(shared, fabric, step);
        }
        let now = Instant::now();
        let steps = self.waker.borrow_mut().poll(now);
        for step in steps {
            self.carry_out(shared, fabric, step);
        }
        self.waker
            .borrow_mut()
            .abandoned(now, |device| label_of(fabric, device));
        self.tell(shared);
    }

    /// When this device next reads its interfaces again, or a wake it is watching needs a probe or
    /// runs out of time.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let scan = self
            .scanned
            .borrow()
            .and_then(|at| at.checked_add(ROUTE_SCAN_INTERVAL));
        [scan, self.waker.borrow().next_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Answer the wake actions one of the person's other devices sent this one. Whether `envelope`
    /// was one of them.
    pub(crate) fn handle_action(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::WakeDevice { target } => {
                let outcome = self.wake_from_here(fabric, target, Some(envelope.source.clone()));
                answer(
                    fabric,
                    &envelope.source,
                    PersonalFabricAction::WakeReport {
                        target: target.clone(),
                        outcome,
                    },
                );
            }
            PersonalFabricAction::WakeReport { target, outcome } => {
                let label = label_of(fabric, target);
                self.waker
                    .borrow_mut()
                    .reported(&envelope.source, target, *outcome, label);
                self.tell(shared);
            }
            PersonalFabricAction::WakeProbe { nonce } => {
                answer(
                    fabric,
                    &envelope.source,
                    PersonalFabricAction::WakeProbeReply { nonce: *nonce },
                );
            }
            PersonalFabricAction::WakeProbeReply { nonce } => {
                let step = self.waker.borrow_mut().answered(&envelope.source, *nonce);
                if let Some(step) = step {
                    self.carry_out(shared, fabric, step);
                }
            }
            _ => return false,
        }
        true
    }

    /// Wake the device the person named, by its id or its label: this device
    /// sends the packets when it shares the sleeping device's network, else
    /// asks the device that does.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be woken, in the person's words: no device by
    /// that name, none of them awake on its network, or a signal that did not
    /// leave.
    pub(crate) fn request(&self, shared: &Shared, name: &str) -> Result<WakePlan, String> {
        let Some(fabric) = &shared.fabric else {
            return Err("this session has no device fabric".to_owned());
        };
        let (local, _) = fabric
            .local_device_fabric_identity()
            .ok_or_else(|| "this device has no identity in the person's devices".to_owned())?;
        let converged = fabric.devices().listed();
        let target =
            wake::resolve_device(&converged, name).map_err(|refusal| refusal.to_string())?;
        let local_routes = self.publish(fabric);
        let plan = wake::plan_wake(&converged, &local, &local_routes, &target, |device| {
            fabric.online_device_ids().contains(device)
        })
        .map_err(|refusal| refusal.to_string())?;
        if plan.waker != local {
            fabric
                .try_send_personal_fabric_action(
                    &plan.waker,
                    PersonalFabricAction::WakeDevice {
                        target: target.clone(),
                    },
                )
                .map_err(|error| {
                    format!(
                        "{} could not be asked to wake {}: {error}",
                        label_of(fabric, &plan.waker),
                        label_of(fabric, &target)
                    )
                })?;
            self.waker
                .borrow_mut()
                .asked(target, plan.waker.clone(), Instant::now());
            return Ok(plan);
        }
        match self.wake_from_here(fabric, &target, None) {
            WakeOutcome::Sent { .. } => Ok(plan),
            _ => Err(format!(
                "could not send a wake-up signal to {} on {}",
                label_of(fabric, &target),
                plan.subnet
            )),
        }
    }

    /// Send this machine's magic packets for `target` and start watching for it on behalf of
    /// `requester` .
    fn wake_from_here(
        &self,
        fabric: &SharedLink,
        target: &DeviceId,
        requester: Option<DeviceId>,
    ) -> WakeOutcome {
        let converged = fabric.devices().listed();
        let routes = self.publish(fabric);
        let packets = wake::magic_packets_for(&converged, &routes, target);
        let Some((_, subnet)) = packets.first().copied() else {
            return WakeOutcome::NotOnTargetLan;
        };
        let mut sent = false;
        for (mac, subnet) in &packets {
            match self.packets.send(*mac, *subnet) {
                Ok(()) => sent = true,
                Err(error) => {
                    tracing::warn!(%error, %subnet, "a wake-up signal could not be sent");
                }
            }
        }
        if !sent {
            return WakeOutcome::SendFailed;
        }
        self.waker
            .borrow_mut()
            .started(target.clone(), requester, Instant::now());
        WakeOutcome::Sent { subnet }
    }

    /// Do the next thing a watch asks for: ask the woken device whether it is back, or tell whoever
    /// asked how it ended.
    fn carry_out(&self, shared: &Shared, fabric: &SharedLink, step: WatchStep) {
        match step {
            WatchStep::Probe { target, nonce } => {
                // Unanswered until the target's session is up again.
                let _ = fabric.try_send_personal_fabric_action(
                    &target,
                    PersonalFabricAction::WakeProbe { nonce },
                );
            }
            WatchStep::Finished {
                target,
                requester: Some(requester),
                outcome,
            } => {
                answer(
                    fabric,
                    &requester,
                    PersonalFabricAction::WakeReport { target, outcome },
                );
            }
            finished @ WatchStep::Finished { .. } => {
                let label = match &finished {
                    WatchStep::Finished { target, .. } => label_of(fabric, target),
                    WatchStep::Probe { .. } => return,
                };
                self.waker.borrow_mut().tell_step(finished, label);
                self.tell(shared);
            }
        }
    }

    /// This machine's wake routes, re-read at most every [`ROUTE_SCAN_INTERVAL`], offered to the
    /// person's devices.
    fn publish(&self, fabric: &SharedLink) -> Vec<WakeRoute> {
        let now = Instant::now();
        let fresh = self
            .scanned
            .borrow()
            .is_some_and(|at| now.saturating_duration_since(at) < ROUTE_SCAN_INTERVAL);
        if !fresh {
            let routes = wake::remember_subnets(self.interfaces.routes(), &self.routes.borrow());
            *self.routes.borrow_mut() = routes;
            *self.scanned.borrow_mut() = Some(now);
        }
        let routes = self.routes.borrow().clone();
        fabric.share_wake_routes(routes.clone());
        routes
    }

    /// Tell the person how their requests to wake a device ended.
    fn tell(&self, shared: &Shared) {
        let outcomes = self.waker.borrow_mut().outcomes();
        if outcomes.is_empty() {
            return;
        }
        let notifications = shared.notifications.borrow();
        for outcome in &outcomes {
            tell(&notifications, outcome);
        }
    }
}

/// Send a wake answer to the device that asked.
fn answer(fabric: &SharedLink, target: &DeviceId, action: PersonalFabricAction) {
    if let Err(error) = fabric.try_send_personal_fabric_action(target, action) {
        tracing::warn!(%error, device = target.as_str(), "a wake answer did not leave");
    }
}

/// Tell the person how one request ended, if the outcome is one to tell.
fn tell(notifications: &SessionNotifications, event: &WakeEvent) {
    if let Some(notice) = crate::notices::wake_notice(event) {
        notifications.tell(&notice);
    }
}

/// The label the person knows `device` by.
fn label_of(fabric: &SharedLink, device: &DeviceId) -> String {
    fabric.devices().get(device).map_or_else(
        || device.as_str().to_owned(),
        |device| device.about.label.clone(),
    )
}
