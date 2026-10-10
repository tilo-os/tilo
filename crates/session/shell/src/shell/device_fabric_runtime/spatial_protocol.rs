//! Optional lease-bound spatial input sequencing. Legacy action bytes stay
//! stable; both sides must confirm support on the grant's certified route.
use super::{
    FabricInputEvent, InputLease, OutboundSpatialInput, PersonalFabricAction, SpatialInputState,
};
use crate::shell::{
    network::{AuthenticatedPersonalRoute, ShellNetwork},
    personal_continuity::DeviceId,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct SpatialProtocol {
    receive: BTreeMap<[u8; 16], u64>,
    send: BTreeMap<DeviceId, ([u8; 16], u64)>,
    probes: BTreeMap<DeviceId, Probe>,
}
struct Probe {
    nonce: [u8; 16],
    last_try: Option<Instant>,
}

impl SpatialInputState {
    pub(super) fn handle_spatial_protocol_action(
        &mut self,
        route: &AuthenticatedPersonalRoute,
        target: &DeviceId,
        action: &PersonalFabricAction,
        now: u64,
    ) -> Option<PersonalFabricAction> {
        match action {
            PersonalFabricAction::SpatialInputProtocol {
                lease_nonce,
                acknowledgement,
            } => self.negotiate_spatial_protocol(
                route,
                target,
                (*lease_nonce, *acknowledgement),
                now,
            ),
            PersonalFabricAction::SpatialInputSequenced {
                lease_nonce,
                sequence,
                event,
            } => {
                self.accept_sequenced_input(route, target, (*lease_nonce, *sequence, *event), now);
                None
            }
            _ => None,
        }
    }

    pub(super) fn input_is_permitted(
        &self,
        route: &AuthenticatedPersonalRoute,
        target: &DeviceId,
        input: ([u8; 16], FabricInputEvent),
        now: u64,
    ) -> bool {
        self.issued_routes.get(&input.0) == Some(route)
            && crate::shell::device_fabric::permits_spatial_input(
                self.issued.get(&input.0),
                route.device(),
                target,
                input,
                now,
            )
    }
    pub(super) fn receive_spatial_grant(
        &mut self,
        source: DeviceId,
        route: AuthenticatedPersonalRoute,
        lease: InputLease,
    ) {
        if self.received.get(&source) == Some(&lease)
            && self.received_routes.get(&source) == Some(&route)
        {
            return;
        }
        self.outbound.retain(|input| input.target != source);
        self.protocol.send.remove(&source);
        self.received_routes.insert(source.clone(), route);
        let nonce = lease.nonce;
        self.received.insert(source.clone(), lease);
        self.retain_spatial_protocol();
        self.protocol.probes.insert(
            source,
            Probe {
                nonce,
                last_try: None,
            },
        );
    }
    pub(super) fn negotiate_spatial_protocol(
        &mut self,
        route: &AuthenticatedPersonalRoute,
        target: &DeviceId,
        offer: ([u8; 16], bool),
        now: u64,
    ) -> Option<PersonalFabricAction> {
        let (nonce, acknowledgement) = offer;
        if acknowledgement {
            let matching = self.received.get(route.device()).is_some_and(|lease| {
                lease.nonce == nonce
                    && &lease.controller == target
                    && &lease.target == route.device()
                    && lease.validate_at(now).is_ok()
            }) && self.received_routes.get(route.device()) == Some(route);
            if matching {
                let entry = self
                    .protocol
                    .send
                    .entry(route.device().clone())
                    .or_insert((nonce, 0));
                if entry.0 != nonce {
                    *entry = (nonce, 0);
                }
                self.protocol.probes.remove(route.device());
            }
            return None;
        }
        if self.issued_routes.get(&nonce) == Some(route)
            && self.issued.get(&nonce).is_some_and(|lease| {
                &lease.controller == route.device()
                    && &lease.target == target
                    && lease.validate_at(now).is_ok()
            })
        {
            self.protocol.receive.entry(nonce).or_insert(0);
            return Some(PersonalFabricAction::SpatialInputProtocol {
                lease_nonce: nonce,
                acknowledgement: true,
            });
        }
        None
    }
    pub(super) fn accept_sequenced_input(
        &mut self,
        route: &AuthenticatedPersonalRoute,
        target: &DeviceId,
        input: ([u8; 16], u64, FabricInputEvent),
        now: u64,
    ) {
        let (nonce, sequence, event) = input;
        if sequence == 0
            || !self.protocol.receive.contains_key(&nonce)
            || !self.input_is_permitted(route, target, (nonce, event), now)
        {
            return;
        }
        if matches!(event, FabricInputEvent::PointerMotion { .. }) {
            let Some(latest) = self.protocol.receive.get_mut(&nonce) else {
                return;
            };
            if sequence <= *latest {
                return;
            }
            *latest = sequence;
        }
        // Only motion overtakes motion. A delayed key or button edge remains
        // deliverable even when its sequence precedes the latest pointer sample.
        self.inbound.push_back((route.device().clone(), event));
    }
    pub(super) fn legacy_motion_overtaken(&self, nonce: [u8; 16], event: FabricInputEvent) -> bool {
        matches!(event, FabricInputEvent::PointerMotion { .. })
            && self
                .protocol
                .receive
                .get(&nonce)
                .is_some_and(|latest| *latest > 0)
    }
    pub(super) fn prepare_spatial_input(
        &mut self,
        input: &mut OutboundSpatialInput,
        now: u64,
    ) -> Result<PersonalFabricAction, String> {
        let permitted = self.received.get(&input.target).is_some_and(|lease| {
            crate::shell::device_fabric::permits_spatial_input(
                Some(lease),
                &lease.controller,
                &input.target,
                (input.lease_nonce, input.event),
                now,
            )
        });
        if !permitted {
            return Err(crate::shell::i18n::t(
                "device-fabric-error-control-not-granted",
            ));
        }
        if let Some((nonce, counter)) = self.protocol.send.get_mut(&input.target)
            && *nonce == input.lease_nonce
        {
            if input.sequence.is_none() {
                *counter = counter.checked_add(1).ok_or_else(|| {
                    crate::shell::i18n::t("device-fabric-error-input-sequence-exhausted")
                })?;
                input.sequence = Some(*counter);
            }
            return Ok(PersonalFabricAction::SpatialInputSequenced {
                lease_nonce: input.lease_nonce,
                sequence: input.sequence.ok_or_else(|| {
                    crate::shell::i18n::t("device-fabric-error-input-sequence-exhausted")
                })?,
                event: input.event,
            });
        }
        Ok(PersonalFabricAction::Input {
            lease_nonce: input.lease_nonce,
            event: input.event,
        })
    }
    pub(super) fn retain_spatial_protocol(&mut self) {
        self.protocol
            .receive
            .retain(|nonce, _| self.issued.contains_key(nonce));
        self.protocol.send.retain(|device, (nonce, _)| {
            self.received
                .get(device)
                .is_some_and(|lease| &lease.nonce == nonce)
        });
        self.protocol.probes.retain(|device, probe| {
            self.received
                .get(device)
                .is_some_and(|lease| lease.nonce == probe.nonce)
        });
    }
}

pub(super) fn probe(network: &ShellNetwork) {
    let now = Instant::now();
    let probes = super::spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |mut spatial| {
            let mut attempts = Vec::new();
            let SpatialInputState {
                protocol,
                received_routes,
                received,
                ..
            } = &mut *spatial;
            for (device, probe) in &mut protocol.probes {
                if probe
                    .last_try
                    .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
                {
                    continue;
                }
                if received
                    .get(device)
                    .is_none_or(|lease| lease.validate_at(tilo_clock::unix_now_ms()).is_err())
                {
                    continue;
                }
                if let Some(route) = received_routes.get(device) {
                    probe.last_try = Some(now);
                    attempts.push((route.clone(), probe.nonce));
                }
            }
            attempts
        },
    );
    // Never acquire the route registry while holding spatial state: queue
    // admission takes those locks in the opposite order.
    for (route, nonce) in probes {
        let _ = network.send_personal_fabric_action_on_route(
            &route,
            PersonalFabricAction::SpatialInputProtocol {
                lease_nonce: nonce,
                acknowledgement: false,
            },
        );
    }
}

#[cfg(test)]
mod tests;
