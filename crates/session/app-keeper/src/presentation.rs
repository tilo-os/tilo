//! `wp_presentation`: the app's feedback objects are the desktop's, so a video player's frame
//! times are the times its pictures were really shown.
//!
//! The app asks for feedback on the content it commits; the desktop gets the same request on the
//! surface that mirrors it, and its answer travels back with its own timestamps, refresh, sequence
//! and flags. A feedback whose content is replaced before the desktop shows it, or whose desktop
//! goes away, is discarded. The app's `wp_presentation` carries the desktop's clock id, which is
//! kept while no desktop is attached.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Dispatch as ClientDispatch, Proxy as _, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::wp_presentation as client_presentation;
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback as client_feedback;
use wayland_protocols::wp::presentation_time::server::wp_presentation as server_presentation;
use wayland_protocols::wp::presentation_time::server::wp_presentation_feedback as server_feedback;

use crate::state::State;

/// The feedback the app asked for on each surface, before the commit it belongs to.
pub(crate) type Pending = HashMap<ObjectId, Vec<server_feedback::WpPresentationFeedback>>;

/// One feedback in the desktop's hands.
struct Flight {
    app: server_feedback::WpPresentationFeedback,
    surface: ObjectId,
    /// The desktop output the last `sync_output` named, by its global's number.
    output: Option<u32>,
}

/// The desktop's feedback objects in flight, by protocol id.
#[derive(Default)]
pub(crate) struct Flights(HashMap<u32, Flight>);

/// What the desktop's feedback object it made for one of the app's surfaces is.
pub(crate) struct FeedbackRef;

impl State {
    /// The desktop said which clock its presentation times are in: the app's own protocol now
    /// exists, with that clock.
    pub(crate) fn desktop_presentation_clock(&mut self, clock: u32) {
        if let Some(known) = self.presentation_clock {
            if known != clock {
                // A clock id is fixed for a bound global; the app already knows the old one. A
                // compositor cannot change it without removing and re-making the global, which
                // would make every client bind again; the desktop's clock stays CLOCK_MONOTONIC
                // in practice.
                tracing::warn!(
                    known,
                    clock,
                    "the desktop presents on another clock; the app keeps the first"
                );
            }
            return;
        }
        let version = self
            .up
            .as_ref()
            .map_or(2, |up| up.presentation_version.min(2));
        self.presentation_clock = Some(clock);
        self.presentation_global = Some(
            self.dh
                .create_global::<Self, server_presentation::WpPresentation, u32>(version, clock),
        );
    }

    /// The app committed `surface`: its feedback objects go to the desktop, and feedback on
    /// content the desktop never showed is discarded.
    pub(crate) fn presentation_committed(&mut self, surface: &WlSurface) {
        let id = surface.id();
        let Some(feedbacks) = self.presentation_pending.remove(&id) else {
            return;
        };
        let stale: Vec<u32> = self
            .presentation_flight
            .0
            .iter()
            .filter(|(_, flight)| flight.surface == id && flight.app.is_alive())
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            if let Some(flight) = self.presentation_flight.0.remove(&key) {
                flight.app.discarded();
            }
        }
        let mut feedbacks = feedbacks.into_iter().filter(Resource::is_alive).peekable();
        if feedbacks.peek().is_none() {
            return;
        }
        // The feedback request must reach the desktop before the commit that carries the content,
        // and the surface it names must exist there already.
        if !self.ensure_up(&id) {
            for app in feedbacks {
                app.discarded();
            }
            return;
        }
        let ready = self.up.as_ref().and_then(|up| {
            up.presentation
                .clone()
                .map(|presentation| (presentation, up.qh.clone()))
        });
        let up_surface = self
            .surfaces
            .get(&id)
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone());
        for app in feedbacks {
            let (Some((presentation, qh)), Some(up_surface)) = (ready.clone(), up_surface.clone())
            else {
                app.discarded();
                continue;
            };
            let proxy = presentation.feedback(&up_surface, &qh, FeedbackRef);
            self.presentation_flight.0.insert(
                proxy.id().protocol_id(),
                Flight {
                    app,
                    surface: id.clone(),
                    output: None,
                },
            );
        }
    }
}

/// The fields of the desktop's `presented` event, passed on as they came.
struct Shown {
    tv_sec_hi: u32,
    tv_sec_lo: u32,
    tv_nsec: u32,
    refresh: u32,
    seq_hi: u32,
    seq_lo: u32,
    flags: WEnum<client_feedback::Kind>,
}

impl State {
    fn desktop_presented(&mut self, key: u32, shown: &Shown) {
        let Some(flight) = self.presentation_flight.0.remove(&key) else {
            return;
        };
        if !flight.app.is_alive() {
            return;
        }
        if let Some(global) = flight.output
            && let Some(output) = self.app_output(global)
            && let Some(client) = flight.app.client()
            && let Some(wl_output) = output.client_outputs(&client).next()
        {
            flight.app.sync_output(&wl_output);
        }
        let bits = match shown.flags {
            WEnum::Value(flags) => flags.bits(),
            WEnum::Unknown(bits) => bits,
        };
        flight.app.presented(
            shown.tv_sec_hi,
            shown.tv_sec_lo,
            shown.tv_nsec,
            shown.refresh,
            shown.seq_hi,
            shown.seq_lo,
            server_feedback::Kind::from_bits_truncate(bits),
        );
    }

    fn desktop_discarded(&mut self, key: u32) {
        let Some(flight) = self.presentation_flight.0.remove(&key) else {
            return;
        };
        if flight.app.is_alive() {
            flight.app.discarded();
        }
    }

    /// The app's output that mirrors the desktop's output `global`.
    fn app_output(&self, global: u32) -> Option<&smithay::output::Output> {
        let up = self.up.as_ref()?;
        let name = up
            .outputs
            .iter()
            .find(|output| output.global == global)
            .and_then(|output| output.name.clone())?;
        self.outputs
            .iter()
            .find(|kept| kept.output.name() == name)
            .map(|kept| &kept.output)
    }

    /// The desktop went away: every feedback it never answered is discarded.
    pub(crate) fn hold_presentation(&mut self) {
        for (_, flight) in std::mem::take(&mut self.presentation_flight.0) {
            if flight.app.is_alive() {
                flight.app.discarded();
            }
        }
    }

    /// The app destroyed a surface: its feedback can never be shown.
    pub(crate) fn presentation_surface_gone(&mut self, id: &ObjectId) {
        self.presentation_pending.remove(id);
        let in_flight: Vec<u32> = self
            .presentation_flight
            .0
            .iter()
            .filter(|(_, flight)| flight.surface == *id)
            .map(|(key, _)| *key)
            .collect();
        for key in in_flight {
            self.desktop_discarded(key);
        }
    }
}

impl GlobalDispatch<server_presentation::WpPresentation, u32> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<server_presentation::WpPresentation>,
        clock: &u32,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let presentation = data_init.init(resource, *clock);
        presentation.clock_id(*clock);
    }
}

impl Dispatch<server_presentation::WpPresentation, u32> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &server_presentation::WpPresentation,
        request: server_presentation::Request,
        _: &u32,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let server_presentation::Request::Feedback { surface, callback } = request {
            let callback = data_init.init(callback, ());
            state
                .presentation_pending
                .entry(surface.id())
                .or_default()
                .push(callback);
        }
    }
}

impl Dispatch<server_feedback::WpPresentationFeedback, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &server_feedback::WpPresentationFeedback,
        _: server_feedback::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl ClientDispatch<client_presentation::WpPresentation, ()> for State {
    fn event(
        state: &mut Self,
        _: &client_presentation::WpPresentation,
        event: client_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let client_presentation::Event::ClockId { clk_id } = event {
            state.desktop_presentation_clock(clk_id);
        }
    }
}

impl ClientDispatch<client_feedback::WpPresentationFeedback, FeedbackRef> for State {
    fn event(
        state: &mut Self,
        feedback: &client_feedback::WpPresentationFeedback,
        event: client_feedback::Event,
        _: &FeedbackRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let key = feedback.id().protocol_id();
        match event {
            client_feedback::Event::SyncOutput { output } => {
                if let Some(flight) = state.presentation_flight.0.get_mut(&key) {
                    flight.output = output.data::<u32>().copied();
                }
            }
            client_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => state.desktop_presented(
                key,
                &Shown {
                    tv_sec_hi,
                    tv_sec_lo,
                    tv_nsec,
                    refresh,
                    seq_hi,
                    seq_lo,
                    flags,
                },
            ),
            client_feedback::Event::Discarded => state.desktop_discarded(key),
            _ => {}
        }
    }
}
