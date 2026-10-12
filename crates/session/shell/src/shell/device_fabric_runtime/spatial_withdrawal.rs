//! Terminal controller withdrawal uses the existing revoke wire action. It can
//! remove only the exact prior lease on its certified route, never admit input.
use super::{InputLease, PersonalFabricAction, SpatialInputState, spatial_input_state};
use crate::shell::{
    device_fabric::MAX_DEVICES,
    network::{AuthenticatedPersonalRoute, ShellNetwork},
    personal_continuity::DeviceId,
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct SpatialWithdrawals {
    pending: BTreeMap<DeviceId, (InputLease, AuthenticatedPersonalRoute)>,
    declined_routes: BTreeMap<DeviceId, AuthenticatedPersonalRoute>,
}

impl SpatialInputState {
    pub(super) fn queue_controller_withdrawals(&mut self) {
        let now = tilo_clock::unix_now_ms();
        self.withdrawals
            .pending
            .retain(|_, (lease, _)| lease.validate_at(now).is_ok());
        for (device, lease) in &self.received {
            if let Some(route) = self.received_routes.get(device)
                && (self.withdrawals.pending.contains_key(device)
                    || self.withdrawals.pending.len() < MAX_DEVICES)
            {
                // A newer target-issued grant replaces the old target lease.
                // Keep just its terminal action, bounded by the fabric limit.
                self.withdrawals
                    .pending
                    .insert(device.clone(), (lease.clone(), route.clone()));
            }
        }
    }

    pub(super) fn automatic_route_withdrawn(&self, route: &AuthenticatedPersonalRoute) -> bool {
        self.withdrawals.declined_routes.get(route.device()) == Some(route)
    }

    pub(super) fn resume_controller_explicitly(&mut self, controller: &DeviceId) {
        self.withdrawals.declined_routes.remove(controller);
    }

    pub(super) fn accept_spatial_withdrawal(
        &mut self,
        route: &AuthenticatedPersonalRoute,
        target: &DeviceId,
        nonce: [u8; 16],
    ) {
        let source = route.device();
        if self.issued.get(&nonce).is_some_and(|lease| {
            &lease.controller == source
                && &lease.target == target
                && self.issued_routes.get(&nonce) == Some(route)
        }) {
            self.withdrawals
                .declined_routes
                .insert(source.clone(), route.clone());
            self.issued.remove(&nonce);
            self.issued_routes.remove(&nonce);
            self.automatic_issued.retain(|_, current| *current != nonce);
            self.release_controller(source);
        }
        if self.received.get(source).is_some_and(|lease| {
            lease.nonce == nonce
                && &lease.controller == target
                && self.received_routes.get(source) == Some(route)
        }) {
            self.received.remove(source);
            self.received_routes.remove(source);
            self.outbound
                .retain(|input| &input.target != source || input.lease_nonce != nonce);
        }
    }
}

pub(super) fn flush(network: &ShellNetwork) {
    let withdrawn = spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |spatial| {
            spatial
                .withdrawals
                .declined_routes
                .values()
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let obsolete = withdrawn
        .into_iter()
        .filter(|route| !network.personal_route_is_current(route))
        .collect::<Vec<_>>();
    if let Ok(mut spatial) = spatial_input_state().lock() {
        spatial
            .withdrawals
            .declined_routes
            .retain(|_, route| !obsolete.contains(route));
    }
    let pending = spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |spatial| {
            spatial
                .withdrawals
                .pending
                .values()
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    for (lease, route) in pending {
        let finished = lease.validate_at(tilo_clock::unix_now_ms()).is_err()
            || !network.personal_route_is_current(&route)
            || network
                .send_personal_fabric_action_on_route(
                    &route,
                    PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
                )
                .is_ok();
        if finished
            && let Ok(mut spatial) = spatial_input_state().lock()
            && spatial.withdrawals.pending.get(&lease.target).is_some_and(
                |(current, current_route)| current.nonce == lease.nonce && current_route == &route,
            )
        {
            spatial.withdrawals.pending.remove(&lease.target);
        }
    }
}
