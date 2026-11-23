//! `xdg_activation_v1`: the app asks for a token, the desktop makes it, and `activate` goes up
//! with the same token.
//!
//! The token the app receives is the desktop's own, handed back verbatim, so an activation the app
//! requests is the activation the desktop's policy judges. A token requested while no desktop is
//! attached can never be used, and is answered with an empty one.

use std::collections::HashMap;
use std::sync::Mutex;

use smithay::reexports::wayland_server::protocol::{wl_seat::WlSeat, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Dispatch as ClientDispatch, Proxy as _, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1 as client_token;
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1 as client_activation;
use wayland_protocols::xdg::activation::v1::server::xdg_activation_token_v1 as server_token;
use wayland_protocols::xdg::activation::v1::server::xdg_activation_v1 as server_activation;

use crate::state::State;

/// What the app attached to a token before committing it.
#[derive(Default)]
pub(crate) struct Goal {
    /// The input serial it came from and the seat that serial belongs to.
    serial: Option<(u32, WlSeat)>,
    app_id: Option<String>,
    surface: Option<WlSurface>,
}

/// A token the app is still filling in.
#[derive(Default)]
pub(crate) struct TokenData(Mutex<Goal>);

impl TokenData {
    fn goal(&self) -> std::sync::MutexGuard<'_, Goal> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The app's token object, answered when the desktop makes a token.
pub(crate) struct TokenRef(pub server_token::XdgActivationTokenV1);

impl State {
    /// Offer the app `xdg_activation_v1` once a desktop offers it.
    pub(crate) fn offer_activation(&mut self) {
        if self.activation_global.is_none() {
            self.activation_global = Some(
                self.dh
                    .create_global::<Self, server_activation::XdgActivationV1, _>(1, ()),
            );
        }
    }

    /// The app committed a token: ask the desktop for one with what the app attached, and answer
    /// the app with the desktop's token when it comes.
    fn app_token_committed(
        &mut self,
        token: &server_token::XdgActivationTokenV1,
        data: &TokenData,
    ) {
        let goal = std::mem::take(&mut *data.goal());
        let ready = self.up.as_ref().and_then(|up| {
            up.activation
                .clone()
                .map(|activation| (activation, up.qh.clone(), up.seat.clone()))
        });
        let Some((activation, qh, seat)) = ready else {
            // No desktop can act on it; an empty token is never valid.
            if token.is_alive() {
                token.done(String::new());
            }
            return;
        };
        let upstream = activation.get_activation_token(&qh, TokenRef(token.clone()));
        if let Some((serial, _)) = goal.serial
            && let Some(seat) = &seat
        {
            // The serial is the desktop's, passed through when its event reached the app, so the
            // desktop's seat is the one it belongs to.
            upstream.set_serial(serial, seat);
        }
        if let Some(app_id) = goal.app_id {
            upstream.set_app_id(app_id);
        }
        if let Some(surface) = goal.surface
            && let Some(up_surface) = self.up_surface_of(&surface)
        {
            upstream.set_surface(&up_surface);
        }
        self.activation_pending
            .insert(upstream.id().protocol_id(), token.clone());
        upstream.commit();
    }

    /// The app asked to be activated with `token` on `surface`: the same request goes to the
    /// desktop, whose policy decides.
    fn app_activated(&self, token: String, surface: &WlSurface) {
        let (Some(up), Some(up_surface)) = (self.up.as_ref(), self.up_surface_of(surface)) else {
            return;
        };
        if let Some(activation) = &up.activation {
            activation.activate(token, &up_surface);
        }
    }

    fn up_surface_of(
        &self,
        surface: &WlSurface,
    ) -> Option<wayland_client::protocol::wl_surface::WlSurface> {
        self.surfaces
            .get(&surface.id())?
            .up
            .as_ref()
            .map(|up| up.surface.clone())
    }

    /// The desktop went away before it answered a token: the app is told so, and any token it
    /// holds from before is stale on the next desktop.
    pub(crate) fn hold_activation(&mut self) {
        for (_, token) in std::mem::take(&mut self.activation_pending) {
            if token.is_alive() {
                token.done(String::new());
            }
        }
    }
}

impl GlobalDispatch<server_activation::XdgActivationV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<server_activation::XdgActivationV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<server_activation::XdgActivationV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &server_activation::XdgActivationV1,
        request: server_activation::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            server_activation::Request::GetActivationToken { id } => {
                data_init.init(id, TokenData::default());
            }
            server_activation::Request::Activate { token, surface } => {
                state.app_activated(token, &surface);
            }
            _ => {}
        }
    }
}

impl Dispatch<server_token::XdgActivationTokenV1, TokenData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        token: &server_token::XdgActivationTokenV1,
        request: server_token::Request,
        data: &TokenData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            server_token::Request::SetSerial { serial, seat } => {
                data.goal().serial = Some((serial, seat));
            }
            server_token::Request::SetAppId { app_id } => {
                data.goal().app_id = Some(app_id);
            }
            server_token::Request::SetSurface { surface } => {
                data.goal().surface = Some(surface);
            }
            server_token::Request::Commit => state.app_token_committed(token, data),
            _ => {}
        }
    }
}

impl ClientDispatch<client_token::XdgActivationTokenV1, TokenRef> for State {
    fn event(
        state: &mut Self,
        token: &client_token::XdgActivationTokenV1,
        event: client_token::Event,
        app: &TokenRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let client_token::Event::Done { token: made } = event else {
            return;
        };
        state.activation_pending.remove(&token.id().protocol_id());
        if app.0.is_alive() {
            app.0.done(made);
        }
    }
}

wayland_client::delegate_noop!(State: ignore client_activation::XdgActivationV1);

/// The desktop's token objects in flight, by protocol id: what to answer if the desktop goes.
pub(crate) type PendingTokens = HashMap<u32, server_token::XdgActivationTokenV1>;
