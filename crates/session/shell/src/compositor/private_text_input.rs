//! Content-free acknowledgement beside the existing standard IME delivery.
use super::state::State;

#[cfg(test)]
mod tests;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use std::collections::HashMap;
use ui::webview::text_input::{Failure, Receipt, Resolver};

pub mod protocol {
    use smithay::reexports::wayland_server;
    use wayland_server::protocol::wl_surface;
    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::{
            WL_SURFACE_INTERFACE, wl_surface_interface,
        };
        wayland_scanner::generate_interfaces!(
            "../../../vendor/winit/src/platform_impl/linux/wayland/types/tilo_text_input.xml"
        );
    }
    use self::__interfaces::{TILO_TEXT_INPUT_MANAGER_V1_INTERFACE, TILO_TEXT_INPUT_V1_INTERFACE};
    wayland_scanner::generate_server_code!(
        "../../../vendor/winit/src/platform_impl/linux/wayland/types/tilo_text_input.xml"
    );
}
use protocol::tilo_text_input_manager_v1::TiloTextInputManagerV1;
use protocol::tilo_text_input_v1::{self, Kind, TiloTextInputV1};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    object: ObjectId,
    snapshot: [u8; 32],
}
struct Pending {
    serial: u32,
    kind: Kind,
    identity: Identity,
    receipt: Receipt,
    resolver: Resolver,
}
struct Active {
    serial: u32,
    snapshot: [u8; 32],
    receipt: Receipt,
}
struct Field {
    resource: TiloTextInputV1,
    surface: WlSurface,
    snapshot: Option<[u8; 32]>,
    pending: Vec<Pending>,
    warm: Option<Active>,
    started: Option<Active>,
}
impl Field {
    fn cancel_pending(&mut self, failure: Failure) {
        for pending in self.pending.drain(..) {
            if self.resource.is_alive() {
                self.resource.operation(
                    pending.serial,
                    Kind::Cancel,
                    pending.identity.snapshot.to_vec(),
                );
            }
            let failure = if pending.kind == Kind::Final {
                Failure::Unknown
            } else {
                failure
            };
            pending.resolver.complete(Err(failure));
        }
    }
    fn cancel(&mut self) {
        for active in [self.started.take(), self.warm.take()]
            .into_iter()
            .flatten()
        {
            active.receipt.cancel();
            if self.resource.is_alive() {
                self.resource
                    .operation(active.serial, Kind::Cancel, active.snapshot.to_vec());
            }
        }
    }
}
impl Drop for Field {
    fn drop(&mut self) {
        self.cancel_pending(Failure::Unknown);
        self.cancel();
    }
}

#[derive(Default)]
pub(crate) struct PrivateTextInput {
    next: u32,
    fields: HashMap<ObjectId, Field>,
}
impl PrivateTextInput {
    pub(crate) fn register(display: &DisplayHandle) {
        display.create_global::<State, TiloTextInputManagerV1, _>(1, ());
    }
    pub(crate) fn identity(&self, surface: &ObjectId) -> Option<Identity> {
        self.fields.iter().find_map(|(object, field)| {
            (field.surface.id() == *surface
                && field.surface.is_alive()
                && field.resource.is_alive())
            .then_some(field.snapshot)
            .flatten()
            .map(|snapshot| Identity {
                object: object.clone(),
                snapshot,
            })
        })
    }
    pub(crate) fn request(&mut self, identity: &Identity, kind: Kind) -> Option<Receipt> {
        let field = self.fields.get_mut(&identity.object)?;
        if !field.surface.is_alive()
            || !field.resource.is_alive()
            || field.snapshot != Some(identity.snapshot)
            || !field.pending.is_empty()
            || (kind == Kind::Prepare && field.warm.is_some())
            || (kind == Kind::Start && (field.warm.is_none() || field.started.is_some()))
            || (kind == Kind::Final && field.started.is_none())
        {
            return None;
        }
        let serial = self.next.checked_add(1)?;
        self.next = serial;
        let (receipt, resolver) = Receipt::channel();
        field
            .resource
            .operation(serial, kind, identity.snapshot.to_vec());
        field.pending.push(Pending {
            serial,
            kind,
            identity: identity.clone(),
            receipt: receipt.clone(),
            resolver,
        });
        Some(receipt)
    }
    pub(crate) fn service(&mut self) {
        for field in self.fields.values_mut() {
            if !field.surface.is_alive() {
                field.cancel_pending(Failure::Unknown);
                field.cancel();
                field.snapshot = None;
                continue;
            }
            for pending in field
                .pending
                .extract_if(.., |pending| pending.receipt.poll().is_some())
            {
                if field.resource.is_alive() {
                    field.resource.operation(
                        pending.serial,
                        Kind::Cancel,
                        pending.identity.snapshot.to_vec(),
                    );
                }
            }
            if field
                .warm
                .as_ref()
                .is_some_and(|active| active.receipt.was_canceled())
                || field
                    .started
                    .as_ref()
                    .is_some_and(|active| active.receipt.was_canceled())
            {
                field.cancel();
            }
        }
    }
}

impl GlobalDispatch<TiloTextInputManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<TiloTextInputManagerV1>,
        (): &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}
impl Dispatch<TiloTextInputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &TiloTextInputManagerV1,
        request: <TiloTextInputManagerV1 as Resource>::Request,
        (): &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let protocol::tilo_text_input_manager_v1::Request::GetField { id, surface } = request {
            let same_client = surface
                .client()
                .is_some_and(|owner| owner.id() == client.id());
            let count = state
                .private_text_input
                .fields
                .values()
                .filter(|field| {
                    field
                        .resource
                        .client()
                        .is_some_and(|owner| owner.id() == client.id())
                })
                .count();
            if !same_client
                || count >= 32
                || state
                    .private_text_input
                    .fields
                    .values()
                    .any(|field| field.surface == surface)
            {
                manager.post_error(
                    1_u32,
                    "a private input field must belong to one unique client surface",
                );
                return;
            }
            let resource = init.init(id, ());
            state.private_text_input.fields.insert(
                resource.id(),
                Field {
                    resource,
                    surface,
                    snapshot: None,
                    pending: Vec::new(),
                    warm: None,
                    started: None,
                },
            );
        }
    }
}
impl Dispatch<TiloTextInputV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &TiloTextInputV1,
        request: <TiloTextInputV1 as Resource>::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let Some(field) = state.private_text_input.fields.get_mut(&resource.id()) else {
            return;
        };
        match request {
            tilo_text_input_v1::Request::Update { snapshot } => {
                let snapshot = if snapshot.is_empty() {
                    None
                } else {
                    let Ok(snapshot) = <[u8; 32]>::try_from(snapshot) else {
                        resource.post_error(
                            tilo_text_input_v1::Error::InvalidSnapshot,
                            "snapshot must be empty or 32 bytes",
                        );
                        return;
                    };
                    Some(snapshot)
                };
                if field.snapshot != snapshot {
                    field.snapshot = snapshot;
                    field.cancel_pending(Failure::Rejected);
                    field.cancel();
                }
            }
            tilo_text_input_v1::Request::Acknowledge { serial, outcome } => {
                let Some(index) = field
                    .pending
                    .iter()
                    .position(|pending| pending.serial == serial)
                else {
                    return;
                };
                let pending = field.pending.remove(index);
                if pending.receipt.poll().is_some() {
                    if field.resource.is_alive() {
                        field.resource.operation(
                            pending.serial,
                            Kind::Cancel,
                            pending.identity.snapshot.to_vec(),
                        );
                    }
                    return;
                }
                let result = match u32::from(outcome) {
                    0 => Ok(None),
                    1 => Err(Failure::Rejected),
                    _ => Err(Failure::Unknown),
                };
                pending.resolver.complete(result.clone());
                if result.is_ok() {
                    let active = Active {
                        serial,
                        snapshot: pending.identity.snapshot,
                        receipt: pending.receipt,
                    };
                    match pending.kind {
                        Kind::Prepare => field.warm = Some(active),
                        Kind::Start => field.started = Some(active),
                        Kind::Final => field.cancel(),
                        _ => {}
                    }
                } else {
                    field.cancel();
                }
            }
            _ => {}
        }
    }
    fn destroyed(
        state: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &TiloTextInputV1,
        (): &(),
    ) {
        state.private_text_input.fields.remove(&resource.id());
    }
}
