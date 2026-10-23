//! Policy core for forwarding sanitized gamepad input between verified sessions.
//!
//! This is deliberately not USB, Bluetooth, HID, evdev, or uinput forwarding.
//! No descriptor, serial, credential, security key, storage interface, raw
//! report, keyboard, or pointer event can be represented by these types. A
//! privileged integrator may implement [`VirtualGamepadFactory`] using a fixed
//! virtual-gamepad profile after this broker grants an exact short-lived lease.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AUTHORIZATION_TTL: Duration = Duration::from_secs(30);
pub const LEASE_TTL: Duration = Duration::from_secs(5 * 60);
pub const MAX_EVENTS_PER_SECOND: u16 = 240;
const MAX_SEQUENCE_ADVANCE: u64 = 1024;
const MAX_DEVICES: usize = 32;
const MAX_AUTHORIZATIONS: usize = 128;
const MAX_LEASES: usize = 32;
const MAX_AUDIT_EVENTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForwardedPeripheralIdentity {
    source_device: String,
    target_device: String,
    app_id: String,
    workspace_id: String,
    session_id: String,
}

impl ForwardedPeripheralIdentity {
    /// Construct only after the privileged caller verified both devices, the
    /// installed app, its workspace membership, and the live app session.
    pub fn new_after_verification(
        source_device: impl Into<String>,
        target_device: impl Into<String>,
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self, PeripheralBrokerError> {
        let identity = Self {
            source_device: source_device.into(),
            target_device: target_device.into(),
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            session_id: session_id.into(),
        };
        if identity.source_device == identity.target_device
            || !valid_id(&identity.source_device, 256)
            || !valid_id(&identity.target_device, 256)
            || !valid_id(&identity.app_id, 256)
            || !valid_id(&identity.workspace_id, 128)
            || !valid_id(&identity.session_id, 256)
        {
            return Err(PeripheralBrokerError::InvalidIdentity);
        }
        Ok(identity)
    }

    #[must_use]
    pub fn source_device(&self) -> &str {
        &self.source_device
    }
    #[must_use]
    pub fn target_device(&self) -> &str {
        &self.target_device
    }
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Broker-local random identity; never a hardware serial or descriptor field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeripheralDeviceId([u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeripheralAuthorizationId {
    broker: [u8; 16],
    nonce: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeripheralLeaseId {
    broker: [u8; 16],
    nonce: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadButton {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    LeftStick,
    RightStick,
    Start,
    Select,
    Guide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedGamepadEvent {
    Axis {
        axis: GamepadAxis,
        value: i16,
    },
    Button {
        button: GamepadButton,
        pressed: bool,
    },
    Hat {
        x: i8,
        y: i8,
    },
}

impl ForwardedGamepadEvent {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        match self {
            Self::Hat { x, y } => x >= -1 && x <= 1 && y >= -1 && y <= 1,
            Self::Axis { .. } | Self::Button { .. } => true,
        }
    }
}

/// Fixed, reviewable virtual-device shape. It cannot carry a vendor
/// descriptor, arbitrary key bitmap, raw report size, or device serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualGamepadPlan {
    pub axes: u8,
    pub buttons: u8,
    pub hats: u8,
}

impl Default for VirtualGamepadPlan {
    fn default() -> Self {
        Self {
            axes: 6,
            buttons: 11,
            hats: 1,
        }
    }
}

pub trait VirtualGamepad: Send {
    /// Apply one validated logical gamepad event.
    ///
    /// # Errors
    ///
    /// Returns an error when the constrained virtual device cannot accept it.
    fn apply(&mut self, event: ForwardedGamepadEvent) -> Result<(), String>;

    /// Neutralize every axis/hat and release every button.
    fn release_all(&mut self) -> Result<(), String>;
}

pub trait VirtualGamepadFactory: Send + Sync {
    /// Create exactly the fixed sanitized gamepad plan.
    ///
    /// # Errors
    ///
    /// Returns an error when a constrained virtual device cannot be created.
    fn create(&self, plan: VirtualGamepadPlan) -> Result<Box<dyn VirtualGamepad>, String>;
}

impl<T> VirtualGamepadFactory for Box<T>
where
    T: VirtualGamepadFactory + ?Sized,
{
    fn create(&self, plan: VirtualGamepadPlan) -> Result<Box<dyn VirtualGamepad>, String> {
        (**self).create(plan)
    }
}

struct DeviceRecord {
    label: String,
}

struct Authorization {
    identity: ForwardedPeripheralIdentity,
    device: PeripheralDeviceId,
    expires_at: Instant,
}

struct Lease {
    identity: ForwardedPeripheralIdentity,
    device_id: PeripheralDeviceId,
    expires_at: Instant,
    last_sequence: Option<u64>,
    rate_window_started: Instant,
    rate_count: u16,
    virtual_device: Box<dyn VirtualGamepad>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeripheralRevocation {
    Explicit,
    FocusLost,
    ScreenLocked,
    DeviceLost,
    Expired,
    AppExited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeripheralAuditEvent {
    LeaseStarted {
        lease: PeripheralLeaseId,
        device: PeripheralDeviceId,
        identity: ForwardedPeripheralIdentity,
    },
    InputRejected {
        lease: PeripheralLeaseId,
        reason: PeripheralInputRejection,
    },
    LeaseStopped {
        lease: PeripheralLeaseId,
        device: PeripheralDeviceId,
        identity: ForwardedPeripheralIdentity,
        reason: PeripheralRevocation,
    },
    ReleaseAllFailed {
        lease: PeripheralLeaseId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeripheralInputRejection {
    InvalidEvent,
    Sequence,
    Rate,
    Focus,
}

pub struct ForwardedPeripheralBroker<F> {
    instance: [u8; 16],
    factory: F,
    devices: BTreeMap<PeripheralDeviceId, DeviceRecord>,
    authorizations: BTreeMap<PeripheralAuthorizationId, Authorization>,
    leases: BTreeMap<PeripheralLeaseId, Lease>,
    focused: Option<ForwardedPeripheralIdentity>,
    audit: VecDeque<PeripheralAuditEvent>,
}

impl<F: VirtualGamepadFactory> ForwardedPeripheralBroker<F> {
    #[must_use]
    pub fn new(factory: F) -> Self {
        Self {
            instance: random_nonzero(),
            factory,
            devices: BTreeMap::new(),
            authorizations: BTreeMap::new(),
            leases: BTreeMap::new(),
            focused: None,
            audit: VecDeque::new(),
        }
    }

    /// Register a broker-local gamepad. No other peripheral class is representable.
    pub fn register_gamepad(
        &mut self,
        label: impl Into<String>,
    ) -> Result<PeripheralDeviceId, PeripheralBrokerError> {
        let label = label.into();
        if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
            return Err(PeripheralBrokerError::InvalidDevice);
        }
        if self.devices.len() >= MAX_DEVICES {
            return Err(PeripheralBrokerError::Capacity);
        }
        let id = PeripheralDeviceId(self.new_nonce());
        self.devices.insert(id, DeviceRecord { label });
        Ok(id)
    }

    #[must_use]
    pub fn gamepads(&self) -> Vec<(PeripheralDeviceId, String)> {
        self.devices
            .iter()
            .map(|(id, device)| (*id, device.label.clone()))
            .collect()
    }

    pub fn set_focus(&mut self, identity: Option<ForwardedPeripheralIdentity>) {
        self.focused = identity;
        let focused = self.focused.clone();
        self.authorizations
            .retain(|_, proof| focused.as_ref() == Some(&proof.identity));
        self.revoke_matching(PeripheralRevocation::FocusLost, |lease| {
            focused.as_ref() != Some(&lease.identity)
        });
    }

    /// Mint a fresh one-shot proof for an exact gamepad and identity binding.
    pub fn authorize(
        &mut self,
        identity: &ForwardedPeripheralIdentity,
        device: PeripheralDeviceId,
        now: Instant,
    ) -> Result<PeripheralAuthorizationId, PeripheralBrokerError> {
        self.reap_expired(now);
        if self.focused.as_ref() != Some(identity) {
            return Err(PeripheralBrokerError::NotFocused);
        }
        if !self.devices.contains_key(&device) {
            return Err(PeripheralBrokerError::DeviceUnavailable);
        }
        if self.authorizations.len() >= MAX_AUTHORIZATIONS {
            return Err(PeripheralBrokerError::Capacity);
        }
        let id = PeripheralAuthorizationId {
            broker: self.instance,
            nonce: self.new_nonce(),
        };
        let expires_at = now
            .checked_add(AUTHORIZATION_TTL)
            .ok_or(PeripheralBrokerError::ClockOverflow)?;
        self.authorizations.insert(
            id,
            Authorization {
                identity: identity.clone(),
                device,
                expires_at,
            },
        );
        Ok(id)
    }

    /// Consume fresh authorization and construct one constrained virtual gamepad.
    pub fn acquire(
        &mut self,
        authorization: PeripheralAuthorizationId,
        identity: &ForwardedPeripheralIdentity,
        device: PeripheralDeviceId,
        now: Instant,
    ) -> Result<PeripheralLeaseId, PeripheralBrokerError> {
        self.reap_expired(now);
        if authorization.broker != self.instance {
            return Err(PeripheralBrokerError::InvalidAuthorization);
        }
        let proof = self
            .authorizations
            .get(&authorization)
            .ok_or(PeripheralBrokerError::InvalidAuthorization)?;
        if &proof.identity != identity || proof.device != device {
            return Err(PeripheralBrokerError::BindingMismatch);
        }
        if self.focused.as_ref() != Some(identity) {
            return Err(PeripheralBrokerError::NotFocused);
        }
        if !self.devices.contains_key(&device) {
            return Err(PeripheralBrokerError::DeviceUnavailable);
        }
        if self.leases.len() >= MAX_LEASES {
            return Err(PeripheralBrokerError::Capacity);
        }
        let expires_at = now
            .checked_add(LEASE_TTL)
            .ok_or(PeripheralBrokerError::ClockOverflow)?;
        let virtual_device = self
            .factory
            .create(VirtualGamepadPlan::default())
            .map_err(PeripheralBrokerError::VirtualDevice)?;
        self.authorizations.remove(&authorization);
        let id = PeripheralLeaseId {
            broker: self.instance,
            nonce: self.new_nonce(),
        };
        self.leases.insert(
            id,
            Lease {
                identity: identity.clone(),
                device_id: device,
                expires_at,
                last_sequence: None,
                rate_window_started: now,
                rate_count: 0,
                virtual_device,
            },
        );
        self.queue_audit(PeripheralAuditEvent::LeaseStarted {
            lease: id,
            device,
            identity: identity.clone(),
        });
        Ok(id)
    }

    /// Apply one bounded logical event to the exact focused lease.
    pub fn submit(
        &mut self,
        lease_id: PeripheralLeaseId,
        identity: &ForwardedPeripheralIdentity,
        sequence: u64,
        event: ForwardedGamepadEvent,
        now: Instant,
    ) -> Result<(), PeripheralBrokerError> {
        self.reap_expired(now);
        if lease_id.broker != self.instance {
            return Err(PeripheralBrokerError::InvalidLease);
        }
        let focused = self.focused.as_ref() == Some(identity);
        let Some(lease) = self.leases.get_mut(&lease_id) else {
            return Err(PeripheralBrokerError::InvalidLease);
        };
        if &lease.identity != identity {
            return Err(PeripheralBrokerError::BindingMismatch);
        }
        if !focused {
            self.queue_audit(PeripheralAuditEvent::InputRejected {
                lease: lease_id,
                reason: PeripheralInputRejection::Focus,
            });
            return Err(PeripheralBrokerError::NotFocused);
        }
        if !event.is_valid() {
            self.queue_audit(PeripheralAuditEvent::InputRejected {
                lease: lease_id,
                reason: PeripheralInputRejection::InvalidEvent,
            });
            return Err(PeripheralBrokerError::InvalidEvent);
        }
        if lease.last_sequence.is_some_and(|last| {
            sequence <= last || sequence.saturating_sub(last) > MAX_SEQUENCE_ADVANCE
        }) {
            self.queue_audit(PeripheralAuditEvent::InputRejected {
                lease: lease_id,
                reason: PeripheralInputRejection::Sequence,
            });
            return Err(PeripheralBrokerError::Sequence);
        }
        if now.saturating_duration_since(lease.rate_window_started) >= Duration::from_secs(1) {
            lease.rate_window_started = now;
            lease.rate_count = 0;
        }
        if lease.rate_count >= MAX_EVENTS_PER_SECOND {
            self.queue_audit(PeripheralAuditEvent::InputRejected {
                lease: lease_id,
                reason: PeripheralInputRejection::Rate,
            });
            return Err(PeripheralBrokerError::RateLimited);
        }
        lease
            .virtual_device
            .apply(event)
            .map_err(PeripheralBrokerError::VirtualDevice)?;
        lease.last_sequence = Some(sequence);
        lease.rate_count = lease.rate_count.saturating_add(1);
        Ok(())
    }

    pub fn revoke(&mut self, lease: PeripheralLeaseId) -> bool {
        self.revoke_one(lease, PeripheralRevocation::Explicit)
    }

    #[must_use]
    pub fn has_lease(&self, lease: PeripheralLeaseId) -> bool {
        self.leases.contains_key(&lease)
    }

    pub fn app_exited(&mut self, identity: &ForwardedPeripheralIdentity) {
        self.authorizations
            .retain(|_, proof| &proof.identity != identity);
        self.revoke_matching(PeripheralRevocation::AppExited, |lease| {
            &lease.identity == identity
        });
    }

    pub fn screen_locked(&mut self) {
        self.focused = None;
        self.authorizations.clear();
        self.revoke_matching(PeripheralRevocation::ScreenLocked, |_| true);
    }

    pub fn remove_device(&mut self, device: PeripheralDeviceId) -> bool {
        self.authorizations
            .retain(|_, proof| proof.device != device);
        self.revoke_matching(PeripheralRevocation::DeviceLost, |lease| {
            lease.device_id == device
        });
        self.devices.remove(&device).is_some()
    }

    pub fn reap_expired(&mut self, now: Instant) {
        self.authorizations
            .retain(|_, proof| proof.expires_at >= now);
        self.revoke_matching(PeripheralRevocation::Expired, |lease| {
            lease.expires_at < now
        });
    }

    pub fn take_audit_events(&mut self) -> Vec<PeripheralAuditEvent> {
        self.audit.drain(..).collect()
    }

    fn revoke_one(&mut self, id: PeripheralLeaseId, reason: PeripheralRevocation) -> bool {
        let Some(mut lease) = self.leases.remove(&id) else {
            return false;
        };
        if lease.virtual_device.release_all().is_err() {
            self.queue_audit(PeripheralAuditEvent::ReleaseAllFailed { lease: id });
        }
        self.queue_audit(PeripheralAuditEvent::LeaseStopped {
            lease: id,
            device: lease.device_id,
            identity: lease.identity,
            reason,
        });
        true
    }

    fn revoke_matching(
        &mut self,
        reason: PeripheralRevocation,
        predicate: impl Fn(&Lease) -> bool,
    ) {
        let ids = self
            .leases
            .iter()
            .filter_map(|(id, lease)| predicate(lease).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            self.revoke_one(id, reason);
        }
    }

    fn queue_audit(&mut self, event: PeripheralAuditEvent) {
        if self.audit.len() >= MAX_AUDIT_EVENTS {
            self.audit.pop_front();
        }
        self.audit.push_back(event);
    }

    fn new_nonce(&self) -> [u8; 16] {
        loop {
            let nonce = random_nonzero();
            if !self.devices.keys().any(|id| id.0 == nonce)
                && !self.authorizations.keys().any(|id| id.nonce == nonce)
                && !self.leases.keys().any(|id| id.nonce == nonce)
            {
                return nonce;
            }
        }
    }
}

fn random_nonzero() -> [u8; 16] {
    let mut value = [0; 16];
    while value == [0; 16] {
        UnwrapErr(SysRng).fill_bytes(&mut value);
    }
    value
}

fn valid_id(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PeripheralBrokerError {
    #[error("invalid verified peripheral identity")]
    InvalidIdentity,
    #[error("invalid forwardable device")]
    InvalidDevice,
    #[error("forwardable device is unavailable")]
    DeviceUnavailable,
    #[error("the exact app session is not focused")]
    NotFocused,
    #[error("fresh peripheral authorization is invalid, expired, foreign, or already used")]
    InvalidAuthorization,
    #[error("peripheral identity, device, or session binding does not match")]
    BindingMismatch,
    #[error("peripheral lease is invalid or expired")]
    InvalidLease,
    #[error("sanitized peripheral event is invalid")]
    InvalidEvent,
    #[error("peripheral event sequence is replayed or outside the bounded window")]
    Sequence,
    #[error("peripheral event rate limit exceeded")]
    RateLimited,
    #[error("peripheral broker capacity is exhausted")]
    Capacity,
    #[error("monotonic clock cannot represent the lease expiry")]
    ClockOverflow,
    #[error("constrained virtual gamepad failed: {0}")]
    VirtualDevice(String),
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingFactory {
        state: Arc<Mutex<RecordingState>>,
    }
    #[derive(Default)]
    struct RecordingState {
        plans: Vec<VirtualGamepadPlan>,
        events: Vec<ForwardedGamepadEvent>,
        releases: usize,
    }
    struct RecordingPad {
        state: Arc<Mutex<RecordingState>>,
    }
    impl VirtualGamepad for RecordingPad {
        fn apply(&mut self, event: ForwardedGamepadEvent) -> Result<(), String> {
            self.state.lock().unwrap().events.push(event);
            Ok(())
        }
        fn release_all(&mut self) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            state.releases = state.releases.saturating_add(1);
            drop(state);
            Ok(())
        }
    }
    impl VirtualGamepadFactory for RecordingFactory {
        fn create(&self, plan: VirtualGamepadPlan) -> Result<Box<dyn VirtualGamepad>, String> {
            self.state.lock().unwrap().plans.push(plan);
            Ok(Box::new(RecordingPad {
                state: self.state.clone(),
            }))
        }
    }

    fn identity(session: &str) -> ForwardedPeripheralIdentity {
        ForwardedPeripheralIdentity::new_after_verification(
            "source", "target", "game", "work", session,
        )
        .unwrap()
    }

    fn active() -> (
        ForwardedPeripheralBroker<RecordingFactory>,
        RecordingFactory,
        ForwardedPeripheralIdentity,
        PeripheralDeviceId,
        Instant,
    ) {
        let factory = RecordingFactory::default();
        let mut broker = ForwardedPeripheralBroker::new(factory.clone());
        let device = broker.register_gamepad("Controller").unwrap();
        let identity = identity("session");
        broker.set_focus(Some(identity.clone()));
        (broker, factory, identity, device, Instant::now())
    }

    fn lease(
        broker: &mut ForwardedPeripheralBroker<RecordingFactory>,
        identity: &ForwardedPeripheralIdentity,
        device: PeripheralDeviceId,
        now: Instant,
    ) -> PeripheralLeaseId {
        let auth = broker.authorize(identity, device, now).unwrap();
        broker.acquire(auth, identity, device, now).unwrap()
    }

    #[test]
    fn forwarding_authorization_is_one_shot_bound_and_expiring() {
        {
            let (mut broker, factory, identity, device, now) = active();
            let lease = lease(&mut broker, &identity, device, now);
            broker
                .submit(
                    lease,
                    &identity,
                    1,
                    ForwardedGamepadEvent::Axis {
                        axis: GamepadAxis::LeftX,
                        value: 123,
                    },
                    now,
                )
                .unwrap();
            broker
                .submit(
                    lease,
                    &identity,
                    2,
                    ForwardedGamepadEvent::Button {
                        button: GamepadButton::South,
                        pressed: true,
                    },
                    now,
                )
                .unwrap();
            assert_eq!(
                factory.state.lock().unwrap().plans,
                vec![VirtualGamepadPlan::default()]
            );
            assert_eq!(factory.state.lock().unwrap().events.len(), 2);
            assert_eq!(
                broker.submit(
                    lease,
                    &identity,
                    3,
                    ForwardedGamepadEvent::Hat { x: 2, y: 0 },
                    now
                ),
                Err(PeripheralBrokerError::InvalidEvent)
            );
        }
        {
            let (mut broker, _, owner, device, now) = active();
            let other = identity("other");
            let auth = broker.authorize(&owner, device, now).unwrap();
            assert_eq!(
                broker.acquire(auth, &other, device, now),
                Err(PeripheralBrokerError::BindingMismatch)
            );
            assert!(broker.acquire(auth, &owner, device, now).is_ok());
            assert_eq!(
                broker.acquire(auth, &owner, device, now),
                Err(PeripheralBrokerError::InvalidAuthorization)
            );
            let stale_after_focus_loss = broker.authorize(&owner, device, now).unwrap();
            broker.set_focus(Some(other.clone()));
            broker.set_focus(Some(owner.clone()));
            assert_eq!(
                broker.acquire(stale_after_focus_loss, &owner, device, now),
                Err(PeripheralBrokerError::InvalidAuthorization)
            );
            let factory = RecordingFactory::default();
            let mut foreign = ForwardedPeripheralBroker::new(factory);
            foreign.set_focus(Some(owner.clone()));
            assert_eq!(
                foreign.acquire(auth, &owner, device, now),
                Err(PeripheralBrokerError::InvalidAuthorization)
            );
        }
        {
            let (mut broker, _, owner, device, now) = active();
            let expired = broker.authorize(&owner, device, now).unwrap();
            assert_eq!(
                broker.acquire(
                    expired,
                    &owner,
                    device,
                    now + AUTHORIZATION_TTL + Duration::from_millis(1)
                ),
                Err(PeripheralBrokerError::InvalidAuthorization)
            );
            let lost = broker.authorize(&owner, device, now).unwrap();
            assert!(broker.remove_device(device));
            assert_eq!(
                broker.acquire(lost, &owner, device, now),
                Err(PeripheralBrokerError::InvalidAuthorization)
            );
        }
    }

    #[test]
    fn forwarding_enforces_focus_and_rate_and_audits_every_teardown() {
        {
            let (mut broker, factory, owner, device, now) = active();
            let lease = lease(&mut broker, &owner, device, now);
            let event = ForwardedGamepadEvent::Button {
                button: GamepadButton::East,
                pressed: true,
            };
            broker.submit(lease, &owner, 10, event, now).unwrap();
            assert_eq!(
                broker.submit(lease, &owner, 10, event, now),
                Err(PeripheralBrokerError::Sequence)
            );
            assert_eq!(
                broker.submit(lease, &owner, 10 + MAX_SEQUENCE_ADVANCE + 1, event, now),
                Err(PeripheralBrokerError::Sequence)
            );
            for sequence in 11..=u64::from(MAX_EVENTS_PER_SECOND) + 9 {
                broker.submit(lease, &owner, sequence, event, now).unwrap();
            }
            assert_eq!(
                broker.submit(lease, &owner, 250, event, now),
                Err(PeripheralBrokerError::RateLimited)
            );
            broker.set_focus(Some(identity("new-focus")));
            assert_eq!(factory.state.lock().unwrap().releases, 1);
            assert_eq!(
                broker.submit(lease, &owner, 251, event, now),
                Err(PeripheralBrokerError::InvalidLease)
            );
        }
        {
            for reason in [
                PeripheralRevocation::Explicit,
                PeripheralRevocation::FocusLost,
                PeripheralRevocation::ScreenLocked,
                PeripheralRevocation::DeviceLost,
                PeripheralRevocation::Expired,
                PeripheralRevocation::AppExited,
            ] {
                let (mut broker, factory, owner, device, now) = active();
                let lease = lease(&mut broker, &owner, device, now);
                match reason {
                    PeripheralRevocation::Explicit => {
                        broker.revoke(lease);
                    }
                    PeripheralRevocation::FocusLost => broker.set_focus(None),
                    PeripheralRevocation::ScreenLocked => broker.screen_locked(),
                    PeripheralRevocation::DeviceLost => {
                        broker.remove_device(device);
                    }
                    PeripheralRevocation::Expired => {
                        broker.reap_expired(now + LEASE_TTL + Duration::from_millis(1));
                    }
                    PeripheralRevocation::AppExited => broker.app_exited(&owner),
                }
                assert_eq!(factory.state.lock().unwrap().releases, 1);
                assert!(
                    matches!(broker.take_audit_events().as_slice(), [PeripheralAuditEvent::LeaseStarted { .. }, PeripheralAuditEvent::LeaseStopped { reason: observed, .. }] if *observed == reason)
                );
            }
        }
        {
            let (mut broker, _, owner, device, now) = active();
            for _ in 0..MAX_AUDIT_EVENTS.saturating_add(5) {
                let lease = lease(&mut broker, &owner, device, now);
                broker.revoke(lease);
            }
            assert_eq!(broker.audit.len(), MAX_AUDIT_EVENTS);
        }
    }
}
