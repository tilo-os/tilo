//! The observed shared layout consumed by the native pointer path.
//! Hardware facts and arrangement are observations, never input authority.

use super::super::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use super::super::device_fabric::{DeviceFabric, FabricStamp};
use super::super::network::ShellNetwork;
use super::super::personal_continuity::DeviceId;
use settings_store::person_displays::{Arrangement, Layout};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug)]
pub(crate) struct PersonLayoutView {
    pub local: DeviceId,
    pub layout: Layout,
    pub legacy_targets: Vec<DeviceId>,
    revisions: Vec<(DeviceId, FabricStamp)>,
}

impl PersonLayoutView {
    pub(super) fn is_current(&self, fabric: &DeviceFabric, local: &DeviceId) -> bool {
        local == &self.local
            && self.revisions.iter().all(|(device, stamp)| {
                fabric
                    .snapshots()
                    .get(device)
                    .and_then(|item| item.display.as_ref())
                    .is_some_and(|display| {
                        &display.stamp == stamp && fabric.display_outputs(device).is_some()
                    })
            })
    }
}

fn cache() -> &'static Mutex<Option<Arc<PersonLayoutView>>> {
    static CACHE: OnceLock<Mutex<Option<Arc<PersonLayoutView>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Forget the previous session's layout ([`super::start_session_for_test`]).
#[cfg(test)]
pub(super) fn forget_for_test() {
    use tilo_lock::MutexRecover as _;
    *cache().lock_recover() = None;
}

/// Preserve current checked device placements when no shared arrangement has
/// anchored the device. The bridge does not write or guess hardware facts.
pub(super) fn resolve(
    fabric: &DeviceFabric,
    local: &DeviceId,
    arrangement: &Arrangement,
    devices: &[settings_store::person_displays::PersonDevice],
) -> PersonLayoutView {
    let mut revisions = Vec::new();
    let mut origins = std::collections::BTreeMap::new();
    for device in devices {
        if device.outputs.is_empty() {
            continue;
        }
        let Ok(id) = DeviceId::new(device.device.clone()) else {
            continue;
        };
        let Some(display) = fabric
            .snapshots()
            .get(&id)
            .and_then(|item| item.display.as_ref())
        else {
            continue;
        };
        revisions.push((id, display.stamp.clone()));
        origins.insert(device.device.clone(), (display.rect.x, display.rect.y));
    }
    let legacy_targets = fabric
        .snapshots()
        .values()
        .filter(|snapshot| {
            &snapshot.owner != local
                && snapshot.presence.online
                && snapshot.display.is_some()
                && fabric.display_outputs(&snapshot.owner).is_none()
        })
        .map(|snapshot| snapshot.owner.clone())
        .collect();
    PersonLayoutView {
        legacy_targets,
        local: local.clone(),
        layout: Layout::resolve_at_device_origins(devices, arrangement, &origins),
        revisions,
    }
}

pub(crate) fn refresh(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    let view = network
        .local_device_fabric_identity()
        .filter(|_| policy.permits(ContinuityCapability::DisplayInput))
        .map(|(local, _)| {
            Arc::new(resolve(
                &network.device_fabric(),
                &local,
                &Arrangement::load(),
                &network.person_display_devices(&local),
            ))
        });
    if let Ok(mut cached) = cache().lock() {
        cached.clone_from(&view);
    }
    super::display_auto_leases::follow(network, view.as_deref(), policy);
}

/// A hardware revision change invalidates the old layout immediately rather
/// than moving through an obsolete connector while the next sample is pending.
pub(crate) fn current(network: &ShellNetwork) -> Option<Arc<PersonLayoutView>> {
    let view = cache().lock().ok()?.clone()?;
    let (local, _) = network.local_device_fabric_identity()?;
    view.is_current(&network.device_fabric(), &local)
        .then_some(view)
}

pub(crate) fn reachable(
    network: &ShellNetwork,
    display: &settings_store::person_displays::ResolvedDisplay,
) -> bool {
    display.local
        || (display.enabled
            && display.online
            && DeviceId::new(display.device.clone())
                .ok()
                .is_some_and(|device| super::spatial_pointer_lease(network, &device).is_some()))
}
