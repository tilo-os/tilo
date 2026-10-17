//! Shell-owned physical-controller forwarding lifecycle.
//!
//! Discovery and raw evdev decoding stay in `os-actions`; this module owns
//! trusted user starts, exact UI-focus binding, bounded delivery, hotplug and
//! teardown through the certified personal-device fabric.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use os_actions::{
    ForwardedGamepadEvent, LinuxPhysicalGamepadCatalog, LinuxPhysicalGamepadSource,
    PhysicalGamepadId, PhysicalGamepadInfo,
};

use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;

const RESCAN_INTERVAL: Duration = Duration::from_secs(1);
const READ_BUDGET_PER_PUMP: usize = 64;
const SEND_BUDGET_PER_PUMP: usize = 8;
const EVENT_QUEUE_CAPACITY: usize = 96;
const EVENTS_PER_SECOND: usize = 180;
const PENDING_TIMEOUT: Duration = os_actions::AUTHORIZATION_TTL;

/// Locally derived focus identity. None of these fields are accepted from a
/// remote peer or an untrusted application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadBinding {
    pub app_id: String,
    pub workspace_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPhysicalGamepadView {
    pub token: u64,
    pub name: String,
    pub forwarding: Option<PhysicalGamepadForwardingView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadForwardingView {
    pub session: u64,
    pub target: DeviceId,
    pub binding: PhysicalGamepadBinding,
    pub ready: bool,
}

struct CatalogEntry {
    token: u64,
    info: PhysicalGamepadInfo,
}

struct ActiveForwarding {
    session: u64,
    source_id: PhysicalGamepadId,
    source: LinuxPhysicalGamepadSource,
    target: DeviceId,
    binding: PhysicalGamepadBinding,
    forwarding_id: [u8; 16],
    requested: Instant,
    ready: bool,
    pending: EventBuffer,
    sent: VecDeque<Instant>,
}

struct PhysicalGamepadManager {
    catalog_provider: LinuxPhysicalGamepadCatalog,
    catalog: BTreeMap<PhysicalGamepadId, CatalogEntry>,
    active: BTreeMap<u64, ActiveForwarding>,
    next_token: u64,
    next_session: u64,
    last_scan: Option<Instant>,
}

impl Default for PhysicalGamepadManager {
    fn default() -> Self {
        Self {
            catalog_provider: LinuxPhysicalGamepadCatalog::default(),
            catalog: BTreeMap::new(),
            active: BTreeMap::new(),
            next_token: 1,
            next_session: 1,
            last_scan: None,
        }
    }
}

impl PhysicalGamepadManager {
    fn refresh(&mut self, now: Instant, force: bool) -> Result<(), String> {
        if !force
            && self
                .last_scan
                .is_some_and(|last| now.duration_since(last) < RESCAN_INTERVAL)
        {
            return Ok(());
        }
        self.last_scan = Some(now);
        let discovered = self
            .catalog_provider
            .discover()
            .map_err(|error| format!("could not enumerate game controllers: {error}"))?;
        let mut next = BTreeMap::new();
        for info in discovered {
            let token = if let Some(entry) = self.catalog.get(&info.id) {
                entry.token
            } else {
                self.allocate_token()
            };
            next.insert(info.id.clone(), CatalogEntry { token, info });
        }
        self.catalog = next;
        Ok(())
    }

    fn allocate_token(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token = self.next_token.saturating_add(1).max(1);
        token
    }

    fn allocate_session(&mut self) -> u64 {
        let session = self.next_session;
        self.next_session = self.next_session.saturating_add(1).max(1);
        session
    }

    fn views(&self) -> Vec<LocalPhysicalGamepadView> {
        self.catalog
            .values()
            .map(|entry| {
                let forwarding = self
                    .active
                    .values()
                    .find(|active| active.source_id == entry.info.id)
                    .map(|active| PhysicalGamepadForwardingView {
                        session: active.session,
                        target: active.target.clone(),
                        binding: active.binding.clone(),
                        ready: active.ready,
                    });
                LocalPhysicalGamepadView {
                    token: entry.token,
                    name: entry.info.name.clone(),
                    forwarding,
                }
            })
            .collect()
    }
}

#[derive(Default)]
struct EventBuffer {
    events: VecDeque<ForwardedGamepadEvent>,
}

impl EventBuffer {
    fn push(&mut self, event: ForwardedGamepadEvent) -> Result<(), String> {
        let suffix_start = self
            .events
            .iter()
            .rposition(|queued| matches!(queued, ForwardedGamepadEvent::Button { .. }))
            // `index` is a position in `events`, so it is below usize::MAX.
            .map_or(0, |index| index.saturating_add(1));
        let replace = match event {
            ForwardedGamepadEvent::Axis { axis, .. } => self
                .events
                .iter()
                .enumerate()
                .skip(suffix_start)
                .find_map(|(index, queued)| {
                    matches!(queued, ForwardedGamepadEvent::Axis { axis: old, .. } if *old == axis)
                        .then_some(index)
                }),
            ForwardedGamepadEvent::Hat { .. } => self
                .events
                .iter()
                .enumerate()
                .skip(suffix_start)
                .find_map(|(index, queued)| {
                    matches!(queued, ForwardedGamepadEvent::Hat { .. }).then_some(index)
                }),
            ForwardedGamepadEvent::Button { .. } => None,
        };
        if let Some(index) = replace {
            if let Some(queued) = self.events.get_mut(index) {
                *queued = event;
            }
            return Ok(());
        }
        if self.events.len() >= EVENT_QUEUE_CAPACITY {
            return Err("physical gamepad delivery queue is full".into());
        }
        self.events.push_back(event);
        Ok(())
    }

    fn pop_front(&mut self) -> Option<ForwardedGamepadEvent> {
        self.events.pop_front()
    }
}

fn manager() -> &'static Mutex<PhysicalGamepadManager> {
    static MANAGER: OnceLock<Mutex<PhysicalGamepadManager>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(PhysicalGamepadManager::default()))
}

/// Snapshot locally connected, strictly classified controllers.
///
/// # Errors
///
/// Returns a message when the gamepad manager is unavailable or the local
/// controller catalog cannot be refreshed.
pub fn local_physical_gamepads() -> Result<Vec<LocalPhysicalGamepadView>, String> {
    let now = Instant::now();
    let mut manager = manager()
        .lock()
        .map_err(|_| crate::shell::i18n::t("gamepad-error-unavailable"))?;
    manager.refresh(now, false)?;
    Ok(manager.views())
}

/// Controllers currently leased to a remote device. Local desktop navigation
/// stays off for these so one press never acts in two places.
pub fn forwarded_physical_gamepads() -> Vec<PhysicalGamepadId> {
    manager().lock().map_or_else(
        |_| Vec::new(),
        |manager| {
            manager
                .active
                .values()
                .map(|active| active.source_id.clone())
                .collect()
        },
    )
}

/// Begin forwarding after a trusted shell action. The source token is local
/// and the app/workspace/session binding must have been derived from current
/// compositor focus by the native integration.
///
/// # Errors
///
/// Returns a message when typed-gamepad continuity is disabled, this device
/// has no certified identity, `target` is not an online remote device, the
/// controller is gone, already forwarded, cannot be opened, or is busy while
/// starting, or the forwarding request cannot be sent.
pub fn start_physical_gamepad_forwarding(
    network: &ShellNetwork,
    source_token: u64,
    target: &DeviceId,
    binding: PhysicalGamepadBinding,
) -> Result<u64, String> {
    if !ContinuityPolicySnapshot::current().permits(ContinuityCapability::TypedGamepad) {
        return Err(crate::shell::i18n::t("continuity-disabled-gamepad"));
    }
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| crate::shell::i18n::t("gamepad-error-no-identity"))?;
    let fabric = network.device_fabric();
    let target_snapshot = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| crate::shell::i18n::t("gamepad-error-target-unknown"))?;
    if target == &local || !target_snapshot.presence.online {
        return Err(crate::shell::i18n::t("gamepad-error-target-offline"));
    }

    let now = Instant::now();
    let mut manager = manager()
        .lock()
        .map_err(|_| crate::shell::i18n::t("gamepad-error-unavailable"))?;
    manager.refresh(now, true)?;
    let entry = manager
        .catalog
        .values()
        .find(|entry| entry.token == source_token)
        .ok_or_else(|| crate::shell::i18n::t("gamepad-error-disconnected"))?;
    if manager
        .active
        .values()
        .any(|active| active.source_id == entry.info.id)
    {
        return Err(crate::shell::i18n::t("gamepad-error-already-forwarded"));
    }
    let source_id = entry.info.id.clone();
    let label = entry.info.name.clone();
    let mut source = manager.catalog_provider.open(&source_id).map_err(|error| {
        crate::shell::i18n::t_args(
            "gamepad-error-open-failed",
            &[("error", error.to_string().into())],
        )
    })?;
    // Never replay button presses accumulated before the person's explicit
    // start action.
    let primed = source.drain(READ_BUDGET_PER_PUMP * 4).map_err(|error| {
        crate::shell::i18n::t_args(
            "gamepad-error-prime-failed",
            &[("error", error.to_string().into())],
        )
    })?;
    if primed.saturated {
        return Err(crate::shell::i18n::t("gamepad-error-busy"));
    }
    let forwarding_id = crate::shell::device_fabric_runtime::request_typed_gamepad_forwarding(
        network,
        target,
        &binding.app_id,
        &binding.workspace_id,
        &binding.session_id,
        &label,
    )?;
    let session = manager.allocate_session();
    manager.active.insert(
        session,
        ActiveForwarding {
            session,
            source_id,
            source,
            target: target.clone(),
            binding,
            forwarding_id,
            requested: now,
            ready: false,
            pending: EventBuffer::default(),
            sent: VecDeque::new(),
        },
    );
    drop(manager);
    Ok(session)
}

/// Explicitly stop one local forwarding. Removal is unconditional even when
/// the target vanished; a reachable target receives the typed stop and
/// neutralizes every virtual control.
///
/// # Errors
///
/// Returns a message when the gamepad manager is unavailable, the session is
/// unknown, or the typed stop cannot be sent to the target.
pub fn stop_physical_gamepad_forwarding(
    network: &ShellNetwork,
    session: u64,
) -> Result<(), String> {
    let active = manager()
        .lock()
        .map_err(|_| crate::shell::i18n::t("gamepad-error-unavailable"))?
        .active
        .remove(&session)
        .ok_or_else(|| crate::shell::i18n::t("gamepad-error-unknown"))?;
    crate::shell::device_fabric_runtime::stop_typed_gamepad_forwarding(
        network,
        active.forwarding_id,
    )
}

/// Stop every locally owned forwarding during compositor/network teardown.
pub fn stop_all_physical_gamepads(network: &ShellNetwork) {
    let active = manager().lock().map_or_else(
        |_| Vec::new(),
        |mut manager| std::mem::take(&mut manager.active).into_values().collect(),
    );
    for forwarding in active {
        let _ = crate::shell::device_fabric_runtime::stop_typed_gamepad_forwarding(
            network,
            forwarding.forwarding_id,
        );
    }
}

/// Advance physical input without ever blocking the compositor. Any loss of
/// policy, exact focus, target presence, source synchronization or delivery
/// tears down the whole lease rather than risking stuck or misdirected input.
#[expect(
    clippy::too_many_lines,
    reason = "one pump applies every teardown condition before forwarding input"
)]
pub fn pump_physical_gamepads(
    network: &ShellNetwork,
    current_binding: Option<&PhysicalGamepadBinding>,
    policy: ContinuityPolicySnapshot,
) {
    if let Ok(mut manager) = manager().lock() {
        // Settings performs explicit discovery and starting a lease performs
        // a final identity-checked scan. With no active lease the compositor
        // has no physical input to maintain, so do not walk udev/sysfs merely
        // to rediscover an unused optional device class every frame.
        if manager.active.is_empty() {
            return;
        }
        let now = Instant::now();
        let policy_allowed = policy.permits(ContinuityCapability::TypedGamepad);
        let online: Vec<DeviceId> = network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
            .collect();
        let mut stops = Vec::new();
        if let Err(error) = manager.refresh(now, false) {
            tracing::warn!(%error, "physical gamepad hotplug scan failed");
        }
        let connected: Vec<PhysicalGamepadId> = manager.catalog.keys().cloned().collect();
        for (session, active) in &mut manager.active {
            let binding_matches = current_binding == Some(&active.binding);
            if !policy_allowed
                || !binding_matches
                || !online.contains(&active.target)
                || !connected.contains(&active.source_id)
                || now.duration_since(active.requested) > PENDING_TIMEOUT && !active.ready
            {
                stops.push(*session);
                continue;
            }

            let ready = crate::shell::device_fabric_runtime::typed_gamepad_forwarding_ready(
                active.forwarding_id,
            );
            if !crate::shell::device_fabric_runtime::typed_gamepad_forwarding_exists(
                active.forwarding_id,
            ) {
                stops.push(*session);
                continue;
            }
            if !ready {
                // Discard while consent is pending: input that happened before
                // the target granted the lease must never be replayed later.
                if active.source.drain(READ_BUDGET_PER_PUMP).is_err() {
                    stops.push(*session);
                }
                continue;
            }
            if !active.ready {
                match active.source.drain(READ_BUDGET_PER_PUMP) {
                    Ok(drained) if drained.saturated => continue,
                    Ok(_) => {
                        active.ready = true;
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "physical gamepad source lost synchronization");
                        stops.push(*session);
                        continue;
                    }
                }
            }
            let incoming = match active.source.drain(READ_BUDGET_PER_PUMP) {
                Ok(drained) => drained.events,
                Err(error) => {
                    tracing::warn!(%error, "physical gamepad source lost synchronization");
                    stops.push(*session);
                    continue;
                }
            };
            if incoming
                .into_iter()
                .try_for_each(|event| active.pending.push(event))
                .is_err()
            {
                stops.push(*session);
                continue;
            }
            while active
                .sent
                .front()
                .is_some_and(|sent| now.duration_since(*sent) >= Duration::from_secs(1))
            {
                active.sent.pop_front();
            }
            let allowance = EVENTS_PER_SECOND.saturating_sub(active.sent.len());
            for _ in 0..SEND_BUDGET_PER_PUMP.min(allowance) {
                let Some(event) = active.pending.pop_front() else {
                    break;
                };
                if crate::shell::device_fabric_runtime::send_typed_gamepad_event(
                    network,
                    active.forwarding_id,
                    event,
                )
                .is_err()
                {
                    stops.push(*session);
                    break;
                }
                active.sent.push_back(now);
            }
        }
        for session in &stops {
            if let Some(active) = manager.active.remove(session) {
                let _ = crate::shell::device_fabric_runtime::stop_typed_gamepad_forwarding(
                    network,
                    active.forwarding_id,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::{GamepadAxis, GamepadButton};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gamepad_backpressure_coalesces_motion_and_hats_but_keeps_button_edges() {
        fn backpressure_coalesces_motion_but_preserves_button_edges() {
            let mut buffer = EventBuffer::default();
            buffer
                .push(ForwardedGamepadEvent::Axis {
                    axis: GamepadAxis::LeftX,
                    value: 10,
                })
                .unwrap();
            buffer
                .push(ForwardedGamepadEvent::Button {
                    button: GamepadButton::South,
                    pressed: true,
                })
                .unwrap();
            buffer
                .push(ForwardedGamepadEvent::Axis {
                    axis: GamepadAxis::LeftX,
                    value: 30,
                })
                .unwrap();
            buffer
                .push(ForwardedGamepadEvent::Button {
                    button: GamepadButton::South,
                    pressed: false,
                })
                .unwrap();
            assert_eq!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Axis {
                    axis: GamepadAxis::LeftX,
                    value: 10
                })
            );
            assert!(matches!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Button { pressed: true, .. })
            ));
            assert_eq!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Axis {
                    axis: GamepadAxis::LeftX,
                    value: 30
                })
            );
            assert!(matches!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Button { pressed: false, .. })
            ));
        }

        fn consecutive_motion_coalesces_without_an_ordering_barrier() {
            let mut buffer = EventBuffer::default();
            for value in [10, 20, 30] {
                buffer
                    .push(ForwardedGamepadEvent::Axis {
                        axis: GamepadAxis::RightY,
                        value,
                    })
                    .unwrap();
            }
            assert_eq!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Axis {
                    axis: GamepadAxis::RightY,
                    value: 30
                })
            );
            assert_eq!(buffer.pop_front(), None);
        }

        fn button_edge_overflow_fails_closed() {
            let mut buffer = EventBuffer::default();
            for pressed in (0..EVENT_QUEUE_CAPACITY).map(|index| index % 2 == 0) {
                buffer
                    .push(ForwardedGamepadEvent::Button {
                        button: GamepadButton::East,
                        pressed,
                    })
                    .unwrap();
            }
            assert!(
                buffer
                    .push(ForwardedGamepadEvent::Button {
                        button: GamepadButton::East,
                        pressed: true,
                    })
                    .is_err()
            );
        }

        fn hats_are_coalesced_as_one_atomic_semantic_event() {
            let mut buffer = EventBuffer::default();
            buffer
                .push(ForwardedGamepadEvent::Hat { x: -1, y: 0 })
                .unwrap();
            buffer
                .push(ForwardedGamepadEvent::Hat { x: 1, y: 1 })
                .unwrap();
            assert_eq!(
                buffer.pop_front(),
                Some(ForwardedGamepadEvent::Hat { x: 1, y: 1 })
            );
            assert_eq!(buffer.pop_front(), None);
        }

        backpressure_coalesces_motion_but_preserves_button_edges();
        consecutive_motion_coalesces_without_an_ordering_barrier();
        button_edge_overflow_fails_closed();
        hats_are_coalesced_as_one_atomic_semantic_event();
    }
}
