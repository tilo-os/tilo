//! Cleanup has bounded slots independent of ordinary input congestion. It waits
//! for older input on the same lease, and blocks re-entry until releases send.
use super::{FabricInputEvent, InputLease, OutboundSpatialInput, SpatialInputState};
use crate::shell::personal_continuity::DeviceId;
use std::collections::BTreeMap;

type LeaseKey = (DeviceId, [u8; 16]);
type ReleaseKey = (u8, u32);

#[derive(Default)]
pub(super) struct SpatialCleanup {
    // At most one entry per retained target lease, with at most 768 key and
    // 1023 button releases. Duplicate teardown cannot grow or reset retries.
    pending: BTreeMap<LeaseKey, BTreeMap<ReleaseKey, OutboundSpatialInput>>,
}
fn release_key(event: FabricInputEvent) -> Option<ReleaseKey> {
    match event {
        FabricInputEvent::Key {
            keycode,
            pressed: false,
            modifiers: 0,
        } => Some((0, keycode)),
        FabricInputEvent::PointerButton {
            button,
            pressed: false,
        } => Some((1, button)),
        _ => None,
    }
}
impl SpatialCleanup {
    pub(super) fn blocks(&self, target: &DeviceId, nonce: [u8; 16]) -> bool {
        self.pending.contains_key(&(target.clone(), nonce))
    }
    pub(super) fn prepared(&mut self, input: &OutboundSpatialInput) {
        if let Some(entry) = self
            .pending
            .get_mut(&(input.target.clone(), input.lease_nonce))
            && let Some(key) = release_key(input.event)
            && let Some(stored) = entry.get_mut(&key)
        {
            *stored = input.clone();
        }
    }
    pub(super) fn delivered(&mut self, input: &OutboundSpatialInput) {
        let lease_key = (input.target.clone(), input.lease_nonce);
        if let Some(entry) = self.pending.get_mut(&lease_key)
            && let Some(key) = release_key(input.event)
            && entry.get(&key) == Some(input)
        {
            entry.remove(&key);
            if entry.is_empty() {
                self.pending.remove(&lease_key);
            }
        }
    }
    pub(super) fn forget_target(&mut self, target: &DeviceId) {
        self.pending.retain(|(device, _), _| device != target);
    }
    pub(super) fn retain(&mut self, leases: &BTreeMap<DeviceId, InputLease>) {
        self.pending.retain(|(target, nonce), _| {
            leases
                .get(target)
                .is_some_and(|lease| lease.nonce == *nonce)
        });
    }
    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }
}
impl SpatialInputState {
    pub(super) fn queue_cleanup_bound(
        &mut self,
        controller: &DeviceId,
        target: &DeviceId,
        batch: ([u8; 16], Vec<FabricInputEvent>),
        now: u64,
    ) -> Result<(), String> {
        let (nonce, events) = batch;
        if events.iter().any(|event| {
            !event.validate()
                || release_key(*event).is_none()
                || !crate::shell::device_fabric::permits_spatial_input(
                    self.received.get(target),
                    controller,
                    target,
                    (nonce, *event),
                    now,
                )
        }) {
            return Err(crate::shell::i18n::t(
                "device-fabric-error-control-not-granted",
            ));
        }
        if events.is_empty() {
            return Ok(());
        }
        let entry = self
            .cleanup
            .pending
            .entry((target.clone(), nonce))
            .or_default();
        for event in events {
            if let Some(key) = release_key(event) {
                entry.entry(key).or_insert_with(|| OutboundSpatialInput {
                    sequence: None,
                    target: target.clone(),
                    lease_nonce: nonce,
                    event,
                });
            }
        }
        Ok(())
    }
    pub(super) fn next_spatial_input(&mut self) -> Option<(OutboundSpatialInput, bool)> {
        let ready = self.cleanup.pending.iter().find(|((target, nonce), _)| {
            !self
                .outbound
                .iter()
                .any(|input| &input.target == target && input.lease_nonce == *nonce)
        });
        if let Some((_, events)) = ready
            && let Some(input) = events.values().next()
        {
            // Keep this entry, including its assigned sequence, until the exact
            // send succeeds. Re-entry stays blocked during the unlocked send.
            return Some((input.clone(), true));
        }
        self.outbound.pop_front().map(|input| (input, false))
    }
}

#[cfg(test)]
mod tests;
