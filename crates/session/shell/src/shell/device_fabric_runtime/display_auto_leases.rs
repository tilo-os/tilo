//! Arrangement-following input leases, with an explicit device-local opt-in.
//! Automatic renewal never adopts or revokes a manually replaced lease.

use super::{InputLease, SpatialInputState, spatial_input_state};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;
use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

const RENEW_MARGIN_MS: u64 = 5 * 60 * 1000;

#[derive(Default)]
struct Declines {
    user: Option<[u8; 32]>,
    ready: bool,
    devices: BTreeSet<DeviceId>,
}

fn declines() -> &'static Mutex<Declines> {
    static STATE: OnceLock<Mutex<Declines>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Declines::default()))
}

fn load_declines(network: &ShellNetwork, state: &mut Declines) {
    let user = network.personal_user_public_key();
    if state.user == user {
        return;
    }
    state.user = user;
    state.devices.clear();
    state.ready = false;
    if user.is_some() {
        match network.load_display_input_declines() {
            Ok(devices) => {
                state.devices = devices;
                state.ready = true;
            }
            Err(error) => {
                tracing::warn!(%error, "automatic display input remains disabled: declines could not be read");
            }
        }
    }
}

pub(super) fn record_decline(
    network: &ShellNetwork,
    device: &DeviceId,
    declined: bool,
) -> Result<(), String> {
    let mut state = declines()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-input-decline-unavailable"))?;
    load_declines(network, &mut state);
    if declined {
        state.devices.insert(device.clone());
    } else {
        state.devices.remove(device);
    }
    if !state.ready {
        // Invalid storage never enables automatic authority. An explicit manual
        // grant can still control one device without discarding unreadable holds.
        return if declined {
            Err(crate::shell::i18n::t(
                "device-fabric-error-input-decline-save-failed",
            ))
        } else {
            Ok(())
        };
    }
    // Written on the state writer, never on the loop. A write that fails
    // turns automatic input off, as unreadable storage does.
    let user = state.user;
    let result = network.save_display_input_declines_in_background(&state.devices, move |error| {
        tracing::warn!(%error, "automatic display input is disabled: declines could not be saved");
        if let Ok(mut state) = declines().lock()
            && state.user == user
        {
            state.ready = false;
        }
    });
    if result.is_err() {
        state.ready = false;
    }
    result
}

pub(super) fn wanted_controllers(
    network: &ShellNetwork,
    view: Option<&super::person_layout::PersonLayoutView>,
    policy: ContinuityPolicySnapshot,
) -> BTreeSet<DeviceId> {
    if !policy.automatic_display_input()
        || !policy.permits(ContinuityCapability::DisplayInput)
        || !view.is_some_and(|view| {
            view.layout
                .displays
                .iter()
                .any(|display| display.local && display.enabled)
        })
    {
        return BTreeSet::new();
    }
    let Ok(mut state) = declines().lock() else {
        return BTreeSet::new();
    };
    load_declines(network, &mut state);
    if !state.ready {
        return BTreeSet::new();
    }
    let (Some((local, _)), fabric) = (
        network.local_device_fabric_identity(),
        network.device_fabric(),
    ) else {
        return BTreeSet::new();
    };
    fabric
        .snapshots()
        .values()
        .filter(|snapshot| {
            snapshot.owner != local
                && snapshot.presence.online
                && (snapshot.presence.capabilities.pointer
                    || snapshot.presence.capabilities.keyboard)
                && network.personal_device_is_enrolled(&snapshot.owner)
                && network.current_personal_route(&snapshot.owner).is_some()
                && !state.devices.contains(&snapshot.owner)
        })
        .map(|snapshot| snapshot.owner.clone())
        .collect()
}

pub(super) fn follow(
    network: &ShellNetwork,
    view: Option<&super::person_layout::PersonLayoutView>,
    policy: ContinuityPolicySnapshot,
) {
    let wanted = wanted_controllers(network, view, policy);
    let automatic = spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .automatic_issued
                .iter()
                .map(|(device, nonce)| (device.clone(), *nonce))
                .collect::<Vec<_>>()
        },
    );
    for (controller, nonce) in &automatic {
        if !wanted.contains(controller) {
            let _ = super::revoke_input_control_automatic(network, controller, *nonce);
        }
    }
    for controller in wanted {
        if let Err(error) = super::grant_input_control_automatic(network, &controller) {
            tracing::debug!(%error, device = controller.as_str(), "automatic display input lease was not granted");
        }
    }
}

fn needs_grant(current: Option<&InputLease>, automatic: bool, now: u64) -> bool {
    current.is_none_or(|lease| {
        automatic && lease.expires_unix_ms <= now.saturating_add(RENEW_MARGIN_MS)
    })
}

impl SpatialInputState {
    pub(super) fn automatic_needs_grant(&self, controller: &DeviceId, now: u64) -> bool {
        let lease = self
            .issued
            .values()
            .find(|lease| &lease.controller == controller);
        let automatic =
            lease.is_some_and(|lease| self.automatic_lease_matches(controller, lease.nonce));
        needs_grant(lease, automatic, now)
    }

    pub(super) fn automatic_lease_matches(&self, controller: &DeviceId, nonce: [u8; 16]) -> bool {
        self.automatic_issued.get(controller) == Some(&nonce)
            && self
                .issued
                .get(&nonce)
                .is_some_and(|lease| &lease.controller == controller)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_display_input_renews_only_its_live_nonce_near_expiry() {
        let controller = DeviceId::new("controller").unwrap();
        let lease = InputLease {
            controller: controller.clone(),
            target: DeviceId::new("target").unwrap(),
            issued_unix_ms: 1,
            expires_unix_ms: 1_000_000,
            nonce: [1; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        let mut state = SpatialInputState::default();
        assert!(state.automatic_needs_grant(&controller, 1));
        state.issued.insert(lease.nonce, lease.clone());
        state
            .automatic_issued
            .insert(controller.clone(), lease.nonce);
        assert!(
            !state.automatic_needs_grant(&controller, lease.expires_unix_ms - RENEW_MARGIN_MS - 1)
        );
        assert!(state.automatic_needs_grant(&controller, lease.expires_unix_ms - RENEW_MARGIN_MS));
        let replacement = InputLease {
            nonce: [2; 16],
            ..lease.clone()
        };
        state.issued.remove(&lease.nonce);
        state.issued.insert(replacement.nonce, replacement.clone());
        assert!(!state.automatic_lease_matches(&controller, lease.nonce));
        assert!(!state.automatic_needs_grant(&controller, replacement.expires_unix_ms - 1));
    }
}
