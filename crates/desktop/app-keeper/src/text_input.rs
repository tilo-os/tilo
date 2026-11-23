//! Text input: `zwp_text_input_v3`.
//!
//! The app's text inputs reach the desktop's, and the desktop's answers reach the app. Each side
//! counts its own commits: the app's `done` carries the number of commits the keeper has seen it
//! make, never the desktop's serials. An enabled field is enabled again, with the state it last
//! committed, once a new desktop is attached.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::{
    client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3 as ClientManager,
        zwp_text_input_v3::{self as client_input, ZwpTextInputV3 as ClientInput},
    },
    server::{
        zwp_text_input_manager_v3::{self as manager, ZwpTextInputManagerV3},
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
};

use crate::state::State;
use crate::upstream::SurfaceRef;

/// What a text input asked for; `commit` makes it the state in effect.
#[derive(Clone, Default)]
struct Wanted {
    enabled: bool,
    surrounding: Option<(String, i32, i32)>,
    cause: Option<u32>,
    content: Option<(u32, u32)>,
    cursor: Option<(i32, i32, i32, i32)>,
}

/// One text input of the app.
struct Input {
    resource: ZwpTextInputV3,
    /// Its twin on the current desktop, while one is attached.
    up: Option<ClientInput>,
    /// The app's own commits: the serial its `done` carries.
    serial: u32,
    pending: Wanted,
    committed: Wanted,
    /// The app surface the desktop entered, so `leave` names the same surface.
    surface: Option<ObjectId>,
    /// The state to enable on a new desktop once it enters the field.
    replay: Option<Wanted>,
}

/// The text inputs of every app connection.
pub(crate) struct TextInput {
    inputs: HashMap<ObjectId, Input>,
}

impl TextInput {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ZwpTextInputManagerV3, ()>(1, ());
        Self {
            inputs: HashMap::new(),
        }
    }

    fn upstream(&self, id: &ObjectId) -> Option<ClientInput> {
        self.inputs.get(id).and_then(|input| input.up.clone())
    }
}

impl State {
    /// The desktop's text input for the app's, made on first use.
    fn text_input_up(&mut self, id: &ObjectId) -> Option<ClientInput> {
        if let Some(up) = self.text_input.upstream(id) {
            return Some(up);
        }
        let up = self.up.as_ref()?;
        let (Some(manager), Some(seat)) = (up.text_input.clone(), up.seat.clone()) else {
            return None;
        };
        let input = manager.get_text_input(&seat, &up.qh, ());
        self.text_input.inputs.get_mut(id)?.up = Some(input.clone());
        Some(input)
    }

    /// Send what the app last committed to a desktop that was just attached, so an enabled field
    /// is enabled again.
    pub(crate) fn desktop_text_input(&mut self) {
        let ids: Vec<ObjectId> = self
            .text_input
            .inputs
            .iter()
            .filter(|(_, input)| input.committed.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let Some(committed) = self
                .text_input
                .inputs
                .get(&id)
                .map(|input| input.committed.clone())
            else {
                continue;
            };
            if self.text_input_up(&id).is_none() {
                continue;
            }
            // The field is enabled once the new desktop enters it: a commit sent before that is
            // discarded by the compositor's focus check.
            if let Some(input) = self.text_input.inputs.get_mut(&id) {
                input.replay = Some(committed);
            }
        }
    }

    /// The desktop went away: the app's text input is left, and its committed state kept.
    pub(crate) fn hold_text_input(&mut self) {
        for input in self.text_input.inputs.values_mut() {
            if let Some(id) = input.surface.take()
                && let Some(surface) = self.surfaces.get(&id).map(|kept| kept.surface.clone())
            {
                input.resource.leave(&surface);
            }
            input.up = None;
        }
    }

    fn app_text_request(&mut self, id: &ObjectId, request: zwp_text_input_v3::Request) {
        let up = self.text_input_up(id);
        match request {
            zwp_text_input_v3::Request::Enable => {
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.enabled = true;
                }
                if let Some(up) = &up {
                    up.enable();
                }
            }
            zwp_text_input_v3::Request::Disable => {
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.enabled = false;
                }
                if let Some(up) = &up {
                    up.disable();
                }
            }
            zwp_text_input_v3::Request::SetSurroundingText {
                text,
                cursor,
                anchor,
            } => {
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.surrounding = Some((text.clone(), cursor, anchor));
                }
                if let Some(up) = &up {
                    up.set_surrounding_text(text, cursor, anchor);
                }
            }
            zwp_text_input_v3::Request::SetTextChangeCause {
                cause: WEnum::Value(cause),
            } => {
                let bits = u32::from(cause);
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.cause = Some(bits);
                }
                if let Some(up) = &up
                    && let Ok(cause) = client_input::ChangeCause::try_from(bits)
                {
                    up.set_text_change_cause(cause);
                }
            }
            zwp_text_input_v3::Request::SetContentType {
                hint: WEnum::Value(hint),
                purpose: WEnum::Value(purpose),
            } => {
                let hint_bits = hint.bits();
                let purpose_bits = u32::from(purpose);
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.content = Some((hint_bits, purpose_bits));
                }
                if let Some(up) = &up
                    && let Ok(purpose) = client_input::ContentPurpose::try_from(purpose_bits)
                {
                    up.set_content_type(
                        client_input::ContentHint::from_bits_truncate(hint_bits),
                        purpose,
                    );
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => {
                if let Some(input) = self.text_input.inputs.get_mut(id) {
                    input.pending.cursor = Some((x, y, width, height));
                }
                if let Some(up) = &up {
                    up.set_cursor_rectangle(x, y, width, height);
                }
            }
            zwp_text_input_v3::Request::Commit => {
                let Some(input) = self.text_input.inputs.get_mut(id) else {
                    return;
                };
                input.committed = input.pending.clone();
                input.serial = input.serial.wrapping_add(1);
                input.replay = None;
                let serial = input.serial;
                match up {
                    Some(up) => up.commit(),
                    // No desktop is attached: the keeper is the app's compositor and answers the
                    // commit itself. The committed state goes to the next desktop.
                    None => {
                        if let Some(input) = self.text_input.inputs.get(id) {
                            input.resource.done(serial);
                        }
                    }
                }
            }
            zwp_text_input_v3::Request::Destroy => {
                if let Some(input) = self.text_input.inputs.remove(id)
                    && let Some(up) = input.up
                {
                    up.destroy();
                }
            }
            _ => {}
        }
    }

    /// The desktop's text input said something: the app's text input hears it.
    fn upstream_text_input(&mut self, proxy: &ClientInput, event: client_input::Event) {
        use client_input::Event;
        let Some(id) = self
            .text_input
            .inputs
            .iter()
            .find(|(_, input)| input.up.as_ref().is_some_and(|up| up == proxy))
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        match event {
            Event::Enter { surface } => {
                let Some(app) = surface.data::<SurfaceRef>().map(|r| r.0.clone()) else {
                    return;
                };
                let Some(app_surface) = self.surfaces.get(&app).map(|kept| kept.surface.clone())
                else {
                    return;
                };
                if let Some(input) = self.text_input.inputs.get_mut(&id) {
                    input.surface = Some(app);
                    input.resource.enter(&app_surface);
                }
                if let Some(state) = self
                    .text_input
                    .inputs
                    .get_mut(&id)
                    .and_then(|input| input.replay.take())
                    && let Some(up) = self.text_input_up(&id)
                {
                    // Enabled again on this desktop, with what it last committed.
                    up.enable();
                    send_state(&up, &state);
                    up.commit();
                }
            }
            Event::Leave { surface } => {
                let Some(app) = surface.data::<SurfaceRef>().map(|r| r.0.clone()) else {
                    return;
                };
                let Some(app_surface) = self.surfaces.get(&app).map(|kept| kept.surface.clone())
                else {
                    return;
                };
                if let Some(input) = self.text_input.inputs.get_mut(&id) {
                    input.surface = None;
                    input.resource.leave(&app_surface);
                }
            }
            Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                if let Some(input) = self.text_input.inputs.get(&id) {
                    input
                        .resource
                        .preedit_string(text, cursor_begin, cursor_end);
                }
            }
            Event::CommitString { text } => {
                if let Some(input) = self.text_input.inputs.get(&id) {
                    input.resource.commit_string(text);
                }
            }
            Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                if let Some(input) = self.text_input.inputs.get(&id) {
                    input
                        .resource
                        .delete_surrounding_text(before_length, after_length);
                }
            }
            Event::Done { .. } => {
                if let Some(input) = self.text_input.inputs.get(&id) {
                    input.resource.done(input.serial);
                }
            }
            _ => {}
        }
    }
}

/// The committed state, as requests to the desktop's input.
fn send_state(up: &ClientInput, state: &Wanted) {
    if let Some((text, cursor, anchor)) = &state.surrounding {
        up.set_surrounding_text(text.clone(), *cursor, *anchor);
    }
    if let Some(cause) = state.cause
        && let Ok(cause) = client_input::ChangeCause::try_from(cause)
    {
        up.set_text_change_cause(cause);
    }
    if let Some((hint, purpose)) = state.content
        && let Ok(purpose) = client_input::ContentPurpose::try_from(purpose)
    {
        up.set_content_type(client_input::ContentHint::from_bits_truncate(hint), purpose);
    }
    if let Some((x, y, width, height)) = state.cursor {
        up.set_cursor_rectangle(x, y, width, height);
    }
}

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpTextInputManagerV3>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpTextInputManagerV3,
        request: manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::GetTextInput { id, seat: _ } = request {
            let input = data_init.init(id, ());
            state.text_input.inputs.insert(
                input.id(),
                Input {
                    resource: input,
                    up: None,
                    serial: 0,
                    pending: Wanted::default(),
                    committed: Wanted::default(),
                    surface: None,
                    replay: None,
                },
            );
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        input: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let id = input.id();
        state.app_text_request(&id, request);
    }

    fn destroyed(
        state: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        input: &ZwpTextInputV3,
        (): &(),
    ) {
        if let Some(input) = state.text_input.inputs.remove(&input.id())
            && let Some(up) = input.up
        {
            up.destroy();
        }
    }
}

impl wayland_client::Dispatch<ClientManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &ClientManager,
        _: <ClientManager as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<ClientInput, ()> for State {
    fn event(
        state: &mut Self,
        input: &ClientInput,
        event: client_input::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.upstream_text_input(input, event);
    }
}
