//! The seat: the desktop's keyboard, pointer and touch events reach the app as they came, with
//! the desktop's own serials, so every request the app makes with a serial (move, resize, a popup
//! grab, a cursor) is valid on the desktop as it stands. The app's `wl_seat` stays while no
//! desktop is attached; focus leaves it and the last keymap stands.

use std::collections::HashMap;
use std::os::fd::{AsFd as _, OwnedFd};

use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::{
    wl_keyboard, wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_seat,
    wl_seat::WlSeat, wl_surface::WlSurface, wl_touch::WlTouch,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::protocol as c;
use wayland_client::{Connection, Proxy as _, QueueHandle, WEnum};

use crate::state::State;
use crate::surfaces::Role;
use crate::upstream::SurfaceRef;

/// The `wl_seat` version the app is offered and the desktop's is bound at.
pub(crate) const SEAT_VERSION: u32 = 9;

/// The app's seat, mirroring the desktop's.
pub(crate) struct SeatMirror {
    name: String,
    capabilities: u32,
    seats: Vec<WlSeat>,
    keyboards: Vec<WlKeyboard>,
    pointers: Vec<WlPointer>,
    touches: Vec<WlTouch>,
    keymap: Option<(u32, OwnedFd, u32)>,
    repeat: Option<(i32, i32)>,
    keyboard_focus: Option<WlSurface>,
    pressed: Vec<u8>,
    modifiers: Option<[u32; 4]>,
    pointer_focus: Option<WlSurface>,
    touch_focus: HashMap<i32, WlSurface>,
    last_serial: u32,
}

impl SeatMirror {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, WlSeat, ()>(SEAT_VERSION, ());
        Self {
            name: "seat0".to_owned(),
            capabilities: 0,
            seats: Vec::new(),
            keyboards: Vec::new(),
            pointers: Vec::new(),
            touches: Vec::new(),
            keymap: None,
            repeat: None,
            keyboard_focus: None,
            pressed: Vec::new(),
            modifiers: None,
            pointer_focus: None,
            touch_focus: HashMap::new(),
            last_serial: 0,
        }
    }

    fn send_keymap(&self, keyboard: &WlKeyboard) {
        if let Some((format, fd, size)) = &self.keymap
            && let Ok(format) = wl_keyboard::KeymapFormat::try_from(*format)
        {
            keyboard.keymap(format, fd.as_fd(), *size);
        }
        if let Some((rate, delay)) = self.repeat
            && keyboard.version() >= 4
        {
            keyboard.repeat_info(rate, delay);
        }
    }

    fn keyboards_of(&self, surface: &WlSurface) -> impl Iterator<Item = &WlKeyboard> {
        let id = surface.id();
        self.keyboards
            .iter()
            .filter(move |keyboard| keyboard.id().same_client_as(&id))
    }

    fn pointers_of(&self, surface: &WlSurface) -> impl Iterator<Item = &WlPointer> {
        let id = surface.id();
        self.pointers
            .iter()
            .filter(move |pointer| pointer.id().same_client_as(&id))
    }

    fn touches_of(&self, surface: &WlSurface) -> impl Iterator<Item = &WlTouch> {
        let id = surface.id();
        self.touches
            .iter()
            .filter(move |touch| touch.id().same_client_as(&id))
    }

    /// The desktop went away: focus leaves the app, touches are cancelled.
    pub(crate) fn hold(&mut self) {
        let serial = self.last_serial;
        if let Some(surface) = self.keyboard_focus.take().filter(Resource::is_alive) {
            for keyboard in self.keyboards_of(&surface) {
                keyboard.leave(serial, &surface);
            }
        }
        if let Some(surface) = self.pointer_focus.take().filter(Resource::is_alive) {
            for pointer in self.pointers_of(&surface) {
                pointer.leave(serial, &surface);
                if pointer.version() >= 5 {
                    pointer.frame();
                }
            }
        }
        let touched: Vec<ClientId> = self
            .touch_focus
            .drain()
            .filter_map(|(_, surface)| surface.client().map(|client| client.id()))
            .collect();
        for touch in &self.touches {
            if touch
                .client()
                .is_some_and(|client| touched.contains(&client.id()))
            {
                touch.cancel();
            }
        }
        self.pressed.clear();
    }

    /// The app connection holding the keyboard, if one does.
    pub(crate) fn focused_client(&self) -> Option<ClientId> {
        self.keyboard_focus
            .as_ref()
            .and_then(Resource::client)
            .map(|client| client.id())
    }

    /// Pointer events the keeper makes for `surface`'s connection itself, past the desktop's: an
    /// X11 server learns where a drag over its window is, or that the button its own drag holds
    /// came up, only from its pointer (`xdnd.rs`).
    pub(crate) fn synthetic_pointer_enter(&self, surface: &WlSurface, (x, y): (f64, f64)) {
        for pointer in self.pointers_of(surface) {
            pointer.enter(self.last_serial, surface, x, y);
            if pointer.version() >= 5 {
                pointer.frame();
            }
        }
    }

    pub(crate) fn synthetic_pointer_motion(
        &self,
        surface: &WlSurface,
        time: u32,
        (x, y): (f64, f64),
    ) {
        for pointer in self.pointers_of(surface) {
            pointer.motion(time, x, y);
            if pointer.version() >= 5 {
                pointer.frame();
            }
        }
    }

    pub(crate) fn synthetic_pointer_release(&self, surface: &WlSurface, button: u32) {
        // Milliseconds, wrapping as the protocol's do.
        let time = synthetic_pointer_time_ms(tilo_clock::unix_now_ms());
        for pointer in self.pointers_of(surface) {
            pointer.button(
                self.last_serial,
                time,
                button,
                wl_pointer::ButtonState::Released,
            );
            if pointer.version() >= 5 {
                pointer.frame();
            }
        }
    }

    pub(crate) fn synthetic_pointer_leave(&self, surface: &WlSurface) {
        if !surface.is_alive() {
            return;
        }
        for pointer in self.pointers_of(surface) {
            pointer.leave(self.last_serial, surface);
            if pointer.version() >= 5 {
                pointer.frame();
            }
        }
    }

    /// The last serial the desktop gave, which its requests accept.
    pub(crate) fn last_serial(&self) -> u32 {
        self.last_serial
    }

    pub(crate) fn surface_gone(&mut self, id: &ObjectId) {
        if self
            .keyboard_focus
            .as_ref()
            .is_some_and(|surface| surface.id() == *id)
        {
            self.keyboard_focus = None;
        }
        if self
            .pointer_focus
            .as_ref()
            .is_some_and(|surface| surface.id() == *id)
        {
            self.pointer_focus = None;
        }
        self.touch_focus.retain(|_, surface| surface.id() != *id);
    }
}

impl GlobalDispatch<WlSeat, ()> for State {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WlSeat>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let seat = data_init.init(resource, ());
        seat.capabilities(wl_seat::Capability::from_bits_truncate(
            state.seat.capabilities,
        ));
        if seat.version() >= 2 {
            seat.name(state.seat.name.clone());
        }
        state.seat.seats.push(seat);
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &WlSeat,
        request: wl_seat::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let seat = &mut state.seat;
        match request {
            wl_seat::Request::GetKeyboard { id } => {
                let keyboard = data_init.init(id, ());
                seat.send_keymap(&keyboard);
                if let Some(surface) = seat
                    .keyboard_focus
                    .as_ref()
                    .filter(|surface| surface.id().same_client_as(&keyboard.id()))
                {
                    keyboard.enter(seat.last_serial, surface, seat.pressed.clone());
                    if let Some([depressed, latched, locked, group]) = seat.modifiers {
                        keyboard.modifiers(seat.last_serial, depressed, latched, locked, group);
                    }
                }
                seat.keyboards.push(keyboard);
            }
            wl_seat::Request::GetPointer { id } => seat.pointers.push(data_init.init(id, ())),
            wl_seat::Request::GetTouch { id } => seat.touches.push(data_init.init(id, ())),
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, seat: &WlSeat, (): &()) {
        state.seat.seats.retain(|kept| kept.id() != seat.id());
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlKeyboard,
        _: wl_keyboard::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, keyboard: &WlKeyboard, (): &()) {
        state
            .seat
            .keyboards
            .retain(|kept| kept.id() != keyboard.id());
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &WlPointer,
        request: wl_pointer::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_pointer::Request::SetCursor {
            serial,
            surface,
            hotspot_x,
            hotspot_y,
        } = request
        {
            state.set_cursor(serial, surface.as_ref(), hotspot_x, hotspot_y);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, pointer: &WlPointer, (): &()) {
        state.seat.pointers.retain(|kept| kept.id() != pointer.id());
    }
}

impl Dispatch<WlTouch, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlTouch,
        _: smithay::reexports::wayland_server::protocol::wl_touch::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, touch: &WlTouch, (): &()) {
        state.seat.touches.retain(|kept| kept.id() != touch.id());
    }
}

impl State {
    /// The app's surface a desktop surface mirrors.
    pub(crate) fn app_surface(&self, up: &c::wl_surface::WlSurface) -> Option<WlSurface> {
        let id = &up.data::<SurfaceRef>()?.0;
        Some(self.surfaces.get(id)?.surface.clone())
    }

    /// A cursor surface of the app, shown on the desktop: its twin, once the desktop has it.
    pub(crate) fn mirror_cursor(
        &mut self,
        surface: Option<&WlSurface>,
    ) -> Option<c::wl_surface::WlSurface> {
        let surface = surface?;
        let id = surface.id();
        if let Some(kept) = self.surfaces.get_mut(&id)
            && matches!(kept.role, Role::None)
        {
            kept.role = Role::Cursor;
        }
        self.push(&id);
        self.surfaces
            .get(&id)
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone())
    }

    fn set_cursor(&mut self, serial: u32, surface: Option<&WlSurface>, x: i32, y: i32) {
        let up_surface = self.mirror_cursor(surface);
        if surface.is_some() && up_surface.is_none() {
            return;
        }
        if let Some(pointer) = self.up.as_ref().and_then(|up| up.pointer.as_ref()) {
            pointer.set_cursor(serial, up_surface.as_ref(), x, y);
        }
    }

    /// The desktop's seat offers these devices: the app's seat follows, and the desktop's
    /// devices are taken.
    fn desktop_capabilities(
        &mut self,
        capabilities: c::wl_seat::Capability,
        qh: &QueueHandle<Self>,
    ) {
        if let Some(up) = self.up.as_mut()
            && let Some(seat) = up.seat.clone()
        {
            let has = |capability| capabilities.contains(capability);
            if has(c::wl_seat::Capability::Keyboard) != up.keyboard.is_some() {
                up.keyboard =
                    has(c::wl_seat::Capability::Keyboard).then(|| seat.get_keyboard(qh, ()));
            }
            if has(c::wl_seat::Capability::Pointer) != up.pointer.is_some() {
                up.pointer = has(c::wl_seat::Capability::Pointer).then(|| seat.get_pointer(qh, ()));
            }
            if has(c::wl_seat::Capability::Touch) != up.touch.is_some() {
                up.touch = has(c::wl_seat::Capability::Touch).then(|| seat.get_touch(qh, ()));
            }
        }
        let bits = capabilities.bits();
        if bits != self.seat.capabilities {
            self.seat.capabilities = bits;
            for seat in &self.seat.seats {
                seat.capabilities(wl_seat::Capability::from_bits_truncate(bits));
            }
        }
    }
}

impl wayland_client::Dispatch<c::wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_seat::WlSeat,
        event: c::wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            c::wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(capabilities),
            } => state.desktop_capabilities(capabilities, qh),
            c::wl_seat::Event::Name { name } if state.seat.name != name => {
                state.seat.name.clone_from(&name);
                for seat in &state.seat.seats {
                    if seat.version() >= 2 {
                        seat.name(name.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<c::wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_keyboard::WlKeyboard,
        event: c::wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            c::wl_keyboard::Event::Keymap {
                format: WEnum::Value(format),
                fd,
                size,
            } => {
                state.seat.keymap = Some((format.into(), fd, size));
                for keyboard in &state.seat.keyboards {
                    state.seat.send_keymap(keyboard);
                }
            }
            c::wl_keyboard::Event::Enter {
                serial,
                surface,
                keys,
            } => {
                state.seat.last_serial = serial;
                let Some(surface) = state.app_surface(&surface) else {
                    return;
                };
                for keyboard in state.seat.keyboards_of(&surface) {
                    keyboard.enter(serial, &surface, keys.clone());
                }
                state.seat.pressed = keys;
                state.seat.keyboard_focus = Some(surface);
                state.selection_focus_changed();
                state.primary_focus_changed();
            }
            c::wl_keyboard::Event::Leave { serial, surface } => {
                state.seat.last_serial = serial;
                let Some(surface) = state.app_surface(&surface) else {
                    return;
                };
                for keyboard in state.seat.keyboards_of(&surface) {
                    keyboard.leave(serial, &surface);
                }
                state.seat.keyboard_focus = None;
                state.seat.pressed.clear();
                state.selection_focus_changed();
                state.primary_focus_changed();
            }
            c::wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state: WEnum::Value(key_state),
            } => {
                state.seat.last_serial = serial;
                let pressed = key_state == c::wl_keyboard::KeyState::Pressed;
                let bytes = key.to_ne_bytes();
                let held: Vec<u8> = state
                    .seat
                    .pressed
                    .chunks_exact(4)
                    .filter(|chunk| *chunk != bytes)
                    .flatten()
                    .copied()
                    .chain(pressed.then_some(bytes).into_iter().flatten())
                    .collect();
                state.seat.pressed = held;
                let Ok(key_state) = wl_keyboard::KeyState::try_from(u32::from(key_state)) else {
                    return;
                };
                if let Some(surface) = &state.seat.keyboard_focus {
                    for keyboard in state.seat.keyboards_of(surface) {
                        keyboard.key(serial, time, key, key_state);
                    }
                }
            }
            c::wl_keyboard::Event::Modifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                state.seat.last_serial = serial;
                state.seat.modifiers = Some([mods_depressed, mods_latched, mods_locked, group]);
                if let Some(surface) = &state.seat.keyboard_focus {
                    for keyboard in state.seat.keyboards_of(surface) {
                        keyboard.modifiers(
                            serial,
                            mods_depressed,
                            mods_latched,
                            mods_locked,
                            group,
                        );
                    }
                }
            }
            c::wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.seat.repeat = Some((rate, delay));
                for keyboard in &state.seat.keyboards {
                    if keyboard.version() >= 4 {
                        keyboard.repeat_info(rate, delay);
                    }
                }
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<c::wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_pointer::WlPointer,
        event: c::wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let c::wl_pointer::Event::Enter {
            serial,
            surface,
            surface_x,
            surface_y,
        } = &event
        {
            state.seat.last_serial = *serial;
            let Some(surface) = state.app_surface(surface) else {
                return;
            };
            let pointers: Vec<WlPointer> = state.seat.pointers_of(&surface).cloned().collect();
            for pointer in &pointers {
                pointer.enter(*serial, &surface, *surface_x, *surface_y);
                state.reapply_cursor_shape(pointer, *serial);
            }
            state.x11_drag_pointer(&surface, &event);
            state.seat.pointer_focus = Some(surface);
            return;
        }
        let Some(surface) = state.seat.pointer_focus.clone() else {
            return;
        };
        state.x11_drag_pointer(&surface, &event);
        if let c::wl_pointer::Event::Leave { serial, .. } = event {
            state.seat.last_serial = serial;
            state.seat.pointer_focus = None;
        }
        if let c::wl_pointer::Event::Button { serial, .. } = event {
            state.seat.last_serial = serial;
        }
        for pointer in state.seat.pointers_of(&surface) {
            forward_pointer(pointer, &surface, &event);
        }
    }
}

fn server_axis(axis: WEnum<c::wl_pointer::Axis>) -> Option<wl_pointer::Axis> {
    match axis {
        WEnum::Value(axis) => wl_pointer::Axis::try_from(u32::from(axis)).ok(),
        WEnum::Unknown(_) => None,
    }
}

/// One pointer event, to one of the app's pointers at the version it bound.
fn forward_pointer(pointer: &WlPointer, surface: &WlSurface, event: &c::wl_pointer::Event) {
    let version = pointer.version();
    match event {
        c::wl_pointer::Event::Leave { serial, .. } => pointer.leave(*serial, surface),
        c::wl_pointer::Event::Motion {
            time,
            surface_x,
            surface_y,
        } => pointer.motion(*time, *surface_x, *surface_y),
        c::wl_pointer::Event::Button {
            serial,
            time,
            button,
            state: WEnum::Value(button_state),
        } => {
            if let Ok(button_state) = wl_pointer::ButtonState::try_from(u32::from(*button_state)) {
                pointer.button(*serial, *time, *button, button_state);
            }
        }
        c::wl_pointer::Event::Axis { time, axis, value } => {
            if let Some(axis) = server_axis(*axis) {
                pointer.axis(*time, axis, *value);
            }
        }
        c::wl_pointer::Event::Frame if version >= 5 => pointer.frame(),
        c::wl_pointer::Event::AxisSource {
            axis_source: WEnum::Value(source),
        } if version >= 5 => {
            if let Ok(source) = wl_pointer::AxisSource::try_from(u32::from(*source)) {
                pointer.axis_source(source);
            }
        }
        c::wl_pointer::Event::AxisStop { time, axis } if version >= 5 => {
            if let Some(axis) = server_axis(*axis) {
                pointer.axis_stop(*time, axis);
            }
        }
        c::wl_pointer::Event::AxisDiscrete { axis, discrete } if (5..8).contains(&version) => {
            if let Some(axis) = server_axis(*axis) {
                pointer.axis_discrete(axis, *discrete);
            }
        }
        c::wl_pointer::Event::AxisValue120 { axis, value120 } if version >= 8 => {
            if let Some(axis) = server_axis(*axis) {
                pointer.axis_value120(axis, *value120);
            }
        }
        c::wl_pointer::Event::AxisRelativeDirection {
            axis,
            direction: WEnum::Value(direction),
        } if version >= 9 => {
            if let (Some(axis), Ok(direction)) = (
                server_axis(*axis),
                wl_pointer::AxisRelativeDirection::try_from(u32::from(*direction)),
            ) {
                pointer.axis_relative_direction(axis, direction);
            }
        }
        _ => {}
    }
}

impl wayland_client::Dispatch<c::wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_touch::WlTouch,
        event: c::wl_touch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            c::wl_touch::Event::Down {
                serial,
                time,
                surface,
                id,
                x,
                y,
            } => {
                state.seat.last_serial = serial;
                let Some(surface) = state.app_surface(&surface) else {
                    return;
                };
                for touch in state.seat.touches_of(&surface) {
                    touch.down(serial, time, &surface, id, x, y);
                }
                state.seat.touch_focus.insert(id, surface);
            }
            c::wl_touch::Event::Up { serial, time, id } => {
                state.seat.last_serial = serial;
                if let Some(surface) = state.seat.touch_focus.remove(&id) {
                    for touch in state.seat.touches_of(&surface) {
                        touch.up(serial, time, id);
                    }
                }
            }
            c::wl_touch::Event::Motion { time, id, x, y } => {
                if let Some(surface) = state.seat.touch_focus.get(&id) {
                    for touch in state.seat.touches_of(surface) {
                        touch.motion(time, id, x, y);
                    }
                }
            }
            c::wl_touch::Event::Shape { id, major, minor } => {
                if let Some(surface) = state.seat.touch_focus.get(&id) {
                    for touch in state.seat.touches_of(surface).filter(|t| t.version() >= 6) {
                        touch.shape(id, major, minor);
                    }
                }
            }
            c::wl_touch::Event::Orientation { id, orientation } => {
                if let Some(surface) = state.seat.touch_focus.get(&id) {
                    for touch in state.seat.touches_of(surface).filter(|t| t.version() >= 6) {
                        touch.orientation(id, orientation);
                    }
                }
            }
            c::wl_touch::Event::Frame => {
                for touch in &state.seat.touches {
                    touch.frame();
                }
            }
            c::wl_touch::Event::Cancel => {
                state.seat.touch_focus.clear();
                for touch in &state.seat.touches {
                    touch.cancel();
                }
            }
            _ => {}
        }
    }
}

/// Milliseconds, wrapping into `u32` as the Wayland pointer protocol specifies.
fn synthetic_pointer_time_ms(now_ms: u64) -> u32 {
    u32::try_from(now_ms & u64::from(u32::MAX)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pointer_time_wraps_shared_clock_milliseconds_into_u32() {
        assert_eq!(synthetic_pointer_time_ms(0), 0);
        assert_eq!(synthetic_pointer_time_ms(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(synthetic_pointer_time_ms(u64::from(u32::MAX) + 1), 0);
        assert_eq!(synthetic_pointer_time_ms(u64::from(u32::MAX) + 42), 41);
        assert_eq!(
            tilo_clock::with_wall_ms(u64::from(u32::MAX) + 42, || {
                synthetic_pointer_time_ms(tilo_clock::unix_now_ms())
            }),
            41
        );
    }
}
