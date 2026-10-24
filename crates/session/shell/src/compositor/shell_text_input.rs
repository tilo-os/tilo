//! The shell's own text fields, reached by the seat's input method.
//!
//! An input method (the on-screen keyboard, the composer) types into the
//! field that holds the seat's text-input focus: a client's
//! `zwp_text_input_v3`. The shell's chrome — palette and launcher search,
//! onboarding, rename fields, Quick Settings' Wi-Fi password — is drawn by
//! the compositor itself and has no Wayland object of its own, so without a
//! stand-in an input method could never reach it.
//!
//! [`ShellTextInput`] is that stand-in: an in-process Wayland client with one
//! bare `wl_surface` and one `zwp_text_input_v3`, holding nothing else. While
//! a chrome field has the focus (and no client holds the keyboard), the
//! compositor gives this surface the seat's text-input focus and the client
//! enables its text input with the field's purpose and caret. Smithay then
//! activates the input method exactly as for an application field, so the
//! keyboard appears and its layout follows the field (a password gets the
//! password layout). What the input method sends back — preedit and commit
//! strings — arrives here and becomes the `WindowEvent::Ime` the chrome
//! widget already understands. Editing keys the keyboard sends through
//! `virtual-keyboard-v1` are translated by the compositor (see
//! [`translate_key`]), because the chrome holds no `wl_keyboard`.
//!
//! The client is public (no privileged or input-method credential), owns no
//! toplevel and is never shown; it exists only to be the text-input focus.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface as ServerSurface;
use smithay::wayland::text_input::TextInputSeat as _;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, EventQueue, Proxy as _, QueueHandle, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use winit::event::{Ime, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::state::{ClientState, State};
use super::text_commit::LoopbackClient;

/// What a chrome field asks of an input method: its purpose and where its
/// caret is, in desktop coordinates.
pub type FieldRequest = ui::ime::ImeWindowState;

/// A native keyboard owner, independent of its caret geometry. Widget ids
/// identify the current layout node; the palette owns keyboard input even
/// when the host's ordinary focused node has not changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFieldId {
    Widget(u64),
    Palette,
    /// A shell password prompt: the compositor's sheet or Quick Settings'
    /// Wi-Fi prompt.
    Prompt,
}

/// Private opaque edit metadata, never surrounding text or a persisted hash.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeEditStamp {
    pub document: ui::bound_dispatch::DocumentEditStamp,
    pub fingerprint: [u8; 32],
}

/// Client-side protocol state.
#[derive(Default)]
struct ClientSide {
    compositor: Option<(u32, u32)>,
    seat: Option<(u32, u32)>,
    manager: Option<(u32, u32)>,
    entered: bool,
    /// Each enter starts a fresh enable cycle, even after a batched leave/enter.
    reentered: bool,
    pending_preedit: Option<String>,
    pending_commit: Option<String>,
    /// Translated, ready for the chrome.
    events: Vec<WindowEvent>,
    serial: u32,
}

/// The stand-in client. Dropping it disconnects it, which also retires the
/// text-input focus Smithay held for its surface.
pub struct ShellTextInput {
    /// Declared first so the server side disconnects before `conn` closes.
    link: LoopbackClient,
    conn: Connection,
    queue: EventQueue<ClientSide>,
    state: ClientSide,
    surface: Option<wl_surface::WlSurface>,
    text_input: Option<zwp_text_input_v3::ZwpTextInputV3>,
    /// What the client last enabled its text input for.
    enabled: Option<FieldRequest>,
    /// The field the compositor wants served, if any.
    wanted: Option<FieldRequest>,
    native_field: Option<NativeFieldId>,
    edit_stamp: Option<NativeEditStamp>,
    edit_stamp_session: Option<super::dictation::SessionId>,
}

impl ShellTextInput {
    /// Connect the stand-in client to the compositor's own display.
    pub fn connect(state: &mut State) -> Result<Self, String> {
        let (link, conn) = LoopbackClient::connect(state, ClientState::new_arc())?;
        let queue = conn.new_event_queue::<ClientSide>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let _ = conn.flush();
        Ok(Self {
            link,
            conn,
            queue,
            state: ClientSide::default(),
            surface: None,
            text_input: None,
            enabled: None,
            wanted: None,
            native_field: None,
            edit_stamp: None,
            edit_stamp_session: None,
        })
    }

    /// The compositor-side surface the input method's text-input focus
    /// rests on while a chrome field is served.
    fn server_surface(&self) -> Option<ServerSurface> {
        let surface = self.surface.as_ref()?;
        self.link
            .server
            .object_from_protocol_id::<ServerSurface>(
                &self.link.display,
                surface.id().protocol_id(),
            )
            .ok()
    }

    /// Whether the seat's text-input focus is this client's surface.
    pub fn holds_focus(&self, state: &State) -> bool {
        let Some(ours) = self.server_surface() else {
            return false;
        };
        state
            .seat
            .text_input()
            .focus()
            .is_some_and(|focus| focus == ours)
    }

    /// Serve `field` (or nobody): move the seat's text-input focus onto the
    /// stand-in surface or off it, and have the client enable or disable its
    /// text input to match. Returns what the input method typed since the
    /// last call, as chrome events.
    pub fn sync(&mut self, state: &mut State, field: Option<FieldRequest>) -> Vec<WindowEvent> {
        self.sync_for_field(state, field, None)
    }

    /// Serve an explicitly identified native field. Changing owners cancels
    /// dictation immediately and starts a fresh protocol enable cycle, even
    /// when both fields have the same purpose and caret rectangle. Events
    /// already received for the previous owner are discarded.
    pub fn sync_for_field(
        &mut self,
        state: &mut State,
        field: Option<FieldRequest>,
        identity: Option<NativeFieldId>,
    ) -> Vec<WindowEvent> {
        let identity = identity.filter(|_| field.is_some());
        let changed = identity != self.native_field;
        if changed {
            state.cancel_active_dictation();
            if self.enabled.is_some()
                && let Some(text_input) = self.text_input.as_ref()
            {
                text_input.disable();
                text_input.commit();
                self.enabled = None;
            }
            self.state.pending_preedit = None;
            self.state.pending_commit = None;
            self.state.events.clear();
            self.native_field = identity;
            self.edit_stamp = None;
            self.edit_stamp_session = None;
        }
        self.wanted = field;
        self.pump();
        let holds = self.holds_focus(state);
        match (field.is_some(), holds) {
            (true, false) => {
                if let Some(surface) = self.server_surface() {
                    let text_input = state.seat.text_input();
                    text_input.leave();
                    text_input.set_focus(Some(surface));
                    text_input.enter();
                }
            }
            (false, true) => {
                let text_input = state.seat.text_input();
                text_input.leave();
                text_input.set_focus(None);
            }
            _ => {}
        }
        self.pump();
        self.update_enable();
        state.shell_field_active = self.holds_focus(state);
        let events = std::mem::take(&mut self.state.events);
        if changed { Vec::new() } else { events }
    }

    /// Call before processing speech replies. A same-field edit/selection or
    /// changed record owner cancels deferred insertion even with equal caret
    /// geometry. An unavailable active-field stamp fails closed.
    pub(crate) fn observe_edit_stamp(&mut self, state: &mut State, stamp: Option<NativeEditStamp>) {
        let session = state.dictation_target_id();
        if self.edit_stamp_session != session {
            self.edit_stamp = None;
            self.edit_stamp_session = session;
        }
        if session.is_some()
            && (stamp.is_none() || (self.edit_stamp.is_some() && self.edit_stamp != stamp))
        {
            state.cancel_active_dictation();
        }
        self.edit_stamp = stamp;
    }

    /// After the compositor flushes a native dictation final, drain its local
    /// bridge in the same input turn. Reads are nonblocking and bounded; a
    /// missing complete commit is failure, never a delayed insertion attempt.
    pub(crate) fn drain_dictation_commit(&mut self) -> Option<Vec<WindowEvent>> {
        for _ in 0..32 {
            self.pump();
            if self
                .state
                .events
                .iter()
                .any(|event| matches!(event, WindowEvent::Ime(Ime::Commit(_))))
            {
                return Some(std::mem::take(&mut self.state.events));
            }
        }
        None
    }

    fn update_enable(&mut self) {
        let Some(text_input) = self.text_input.as_ref() else {
            return;
        };
        let target = self.wanted.filter(|_| self.state.entered);
        if target == self.enabled {
            return;
        }
        let Some(field) = target else {
            text_input.disable();
            text_input.commit();
            self.enabled = None;
            let _ = self.conn.flush();
            return;
        };
        let (hint, purpose) = content_type(field.content);
        if self.enabled.is_none() {
            text_input.enable();
        }
        text_input.set_content_type(hint, purpose);
        let clamp = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
        text_input.set_cursor_rectangle(
            clamp(field.x),
            clamp(field.y),
            clamp(field.width),
            clamp(field.height),
        );
        text_input.commit();
        self.enabled = target;
        let _ = self.conn.flush();
    }

    /// Read and dispatch what the compositor sent; bind the globals and
    /// create the surface once they are known.
    fn pump(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        if let Some(guard) = self.conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = self.queue.dispatch_pending(&mut self.state);
        if self.text_input.is_none()
            && let (Some((compositor, cv)), Some((seat, sv)), Some((manager, mv))) =
                (self.state.compositor, self.state.seat, self.state.manager)
        {
            let qh = self.queue.handle();
            let registry = self.conn.display().get_registry(&qh, ());
            let compositor: wl_compositor::WlCompositor =
                registry.bind(compositor, cv.min(4), &qh, ());
            let seat: wl_seat::WlSeat = registry.bind(seat, sv.min(1), &qh, ());
            let manager: zwp_text_input_manager_v3::ZwpTextInputManagerV3 =
                registry.bind(manager, mv.min(1), &qh, ());
            // Focus can rest on the surface once the compositor has
            // dispatched these requests, on a later tick.
            self.surface = Some(compositor.create_surface(&qh, ()));
            self.text_input = Some(manager.get_text_input(&seat, &qh, ()));
        }
        let reentered = std::mem::take(&mut self.state.reentered);
        if !self.state.entered || reentered {
            self.enabled = None;
        }
        let _ = self.conn.flush();
    }
}

/// The text-input content type the field asks for, as the toolkit derived
/// it: a password is hidden and sensitive, so the keyboard shows its
/// password layout and learns nothing from it.
fn content_type(
    content: ui::ime::InputContent,
) -> (
    zwp_text_input_v3::ContentHint,
    zwp_text_input_v3::ContentPurpose,
) {
    use zwp_text_input_v3::{ContentHint, ContentPurpose};
    let (hint, purpose) = content.wayland();
    (
        ContentHint::from_bits_truncate(hint),
        ContentPurpose::try_from(purpose).unwrap_or(ContentPurpose::Normal),
    )
}

/// The chrome event an editing key from the on-screen keyboard's virtual
/// keyboard stands for, from the keysym its keymap gives the key. Printable
/// keys come as text through the input method, so only editing keys and the
/// odd literal character map here.
pub fn translate_key(keysym: u32, pressed: bool) -> Option<WindowEvent> {
    use smithay::input::keyboard::keysyms as k;
    let key = match keysym {
        k::KEY_BackSpace => Key::Named(NamedKey::Backspace),
        k::KEY_Return | k::KEY_KP_Enter => Key::Named(NamedKey::Enter),
        k::KEY_Tab => Key::Named(NamedKey::Tab),
        k::KEY_Left => Key::Named(NamedKey::ArrowLeft),
        k::KEY_Right => Key::Named(NamedKey::ArrowRight),
        k::KEY_Up => Key::Named(NamedKey::ArrowUp),
        k::KEY_Down => Key::Named(NamedKey::ArrowDown),
        k::KEY_Delete => Key::Named(NamedKey::Delete),
        k::KEY_Escape => Key::Named(NamedKey::Escape),
        k::KEY_space => Key::Character(" ".into()),
        other => {
            let character = smithay::input::keyboard::xkb::keysym_to_utf32(
                smithay::input::keyboard::Keysym::new(other),
            );
            let character = char::from_u32(character).filter(|c| !c.is_control())?;
            Key::Character(character.to_string().into())
        }
    };
    Some(crate::shell::test_control::keyboard_input_event(
        &key, pressed,
    ))
}

/// The modifiers a virtual keyboard's `wl_keyboard.modifiers` mask holds.
/// The on-screen keyboard's keymap keeps XKB's core modifiers, whose bits
/// every keymap shares: Shift 0, Control 2, Mod1 (Alt) 3, Mod4 (Super) 6.
#[must_use]
pub fn modifiers_state(mask: u32) -> winit::keyboard::ModifiersState {
    use winit::keyboard::ModifiersState as M;
    [
        (1 << 0, M::SHIFT),
        (1 << 2, M::CONTROL),
        (1 << 3, M::ALT),
        (1 << 6, M::SUPER),
    ]
    .into_iter()
    .filter(|(bit, _)| mask & bit != 0)
    .fold(M::empty(), |held, (_, modifier)| held | modifier)
}

impl wayland_client::Dispatch<wl_registry::WlRegistry, ()> for ClientSide {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some((name, version)),
                "wl_seat" if state.seat.is_none() => state.seat = Some((name, version)),
                "zwp_text_input_manager_v3" => state.manager = Some((name, version)),
                _ => {}
            }
        }
    }
}

impl wayland_client::Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for ClientSide {
    fn event(
        state: &mut Self,
        _: &zwp_text_input_v3::ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                state.entered = true;
                state.reentered = true;
            }
            zwp_text_input_v3::Event::Leave { .. } => state.entered = false,
            zwp_text_input_v3::Event::PreeditString { text, .. } => {
                state.pending_preedit = Some(text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                state.pending_commit = Some(text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::Done { serial } => {
                state.serial = serial;
                // The protocol's order: clear the old preedit, insert the
                // commit, then show the new preedit.
                if let Some(commit) = state.pending_commit.take()
                    && !commit.is_empty()
                {
                    state
                        .events
                        .push(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                    state.events.push(WindowEvent::Ime(Ime::Commit(commit)));
                }
                if let Some(preedit) = state.pending_preedit.take() {
                    let cursor = (!preedit.is_empty()).then_some((preedit.len(), preedit.len()));
                    state
                        .events
                        .push(WindowEvent::Ime(Ime::Preedit(preedit, cursor)));
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(ClientSide: ignore wl_compositor::WlCompositor);
delegate_noop!(ClientSide: ignore wl_surface::WlSurface);
delegate_noop!(ClientSide: ignore wl_seat::WlSeat);
delegate_noop!(ClientSide: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);

#[cfg(test)]
mod tests {
    use super::*;

    fn key_of(event: &WindowEvent) -> (Key, bool) {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            panic!("not a key: {event:?}");
        };
        (
            event.logical_key.clone(),
            event.state == winit::event::ElementState::Pressed,
        )
    }

    #[test]
    fn a_password_field_asks_for_the_hidden_password_layout() {
        use zwp_text_input_v3::{ContentHint, ContentPurpose};
        let (hint, purpose) = content_type(ui::ime::InputContent::PASSWORD);
        assert_eq!(purpose, ContentPurpose::Password);
        assert!(hint.contains(ContentHint::HiddenText | ContentHint::SensitiveData));
        assert_eq!(
            content_type(ui::ime::InputContent::TEXT),
            (ContentHint::None, ContentPurpose::Normal)
        );
    }

    #[test]
    fn an_email_field_asks_for_the_email_layout() {
        use zwp_text_input_v3::{ContentHint, ContentPurpose};
        let field = ui::widget::TextInput::local().with_kind(ui::widget::TextInputKind::Email);
        let content = ui::widget::Widget::ime_content(&field).unwrap();
        assert_eq!(
            content_type(content),
            (ContentHint::Lowercase, ContentPurpose::Email)
        );
    }

    #[test]
    fn editing_keys_from_the_virtual_keyboard_become_chrome_keys() {
        use smithay::input::keyboard::keysyms as k;
        for (keysym, key) in [
            (k::KEY_BackSpace, Key::Named(NamedKey::Backspace)),
            (k::KEY_Return, Key::Named(NamedKey::Enter)),
            (k::KEY_Tab, Key::Named(NamedKey::Tab)),
            (k::KEY_Left, Key::Named(NamedKey::ArrowLeft)),
            (k::KEY_Right, Key::Named(NamedKey::ArrowRight)),
            (k::KEY_space, Key::Character(" ".into())),
            (k::KEY_a, Key::Character("a".into())),
            (k::KEY_eacute, Key::Character("é".into())),
        ] {
            let event = translate_key(keysym, true).expect("translated");
            assert_eq!(key_of(&event), (key.clone(), true), "{keysym:#x}");
            let event = translate_key(keysym, false).expect("translated");
            assert_eq!(key_of(&event), (key, false), "{keysym:#x}");
        }
        assert!(translate_key(k::KEY_Shift_L, true).is_none());
    }
}
