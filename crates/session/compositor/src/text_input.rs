//! Text input for clients (`text-input-v3`) served by a trusted input method (`input-method-v2`,
//! `virtual-keyboard-v1` ): the on-screen keyboard or an IME. Its candidate popups are drawn under
//! the text they belong to, above every window.
//! A field is also named for dictation: which field the person types into ([`TextField`]), and
//! inserting committed text into it once, through the same input method
//! ([`State::insert_dictation`]).

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical coordinates of windows and outputs, far from i32 limits"
)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::PopupManager;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ContentHint, ContentPurpose, ZwpTextInputV3,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodManagerState, InputMethodSeat as _, PopupSurface,
};
use smithay::wayland::text_input::{CommittedState, TextInputManagerState, TextInputSeat};
use smithay::wayland::virtual_keyboard::{VirtualKeyboardHandler, VirtualKeyboardManagerState};

use crate::input::{InputEvent, Switch};
use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};
use tilo_session_core::surface::{HostIme, HostTextInput};

fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::trusted)
}

/// Whether a text field waits for typing, and whether the person is typing on the glass: what the
/// on-screen keyboard follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextEntry {
    /// A text field has the focus and has asked for typing.
    pub wants_typing: bool,
    /// The last typing was a touch on the screen: a hardware key since then hands back to the
    /// keyboard the person is holding.
    pub on_the_glass: bool,
    /// The machine can be typed on by touching it: a touchscreen is attached, the glass has been
    /// touched, or it is a convertible folded into tablet shape. The on-screen keyboard runs
    /// wherever this holds, so a tap into a field never waits for a keyboard process to start.
    pub touch_available: bool,
}

/// A text field as dictation names it: which field the person types into,
/// and the revision of what its client last committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextField {
    /// The field's identity within this compositor's life: its text-input object together with the
    /// surface it types into.
    pub id: u64,
    /// The serial of its client's last committed state: any commit since (a caret moved, a layout
    /// change, typing) moves it.
    pub revision: u64,
    /// Its client takes more than one line.
    pub multiline: bool,
}

/// Why dictated text may not be written into the field the person is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFieldRefusal {
    /// No field has the focus and asks for typing.
    NoField,
    /// The field takes a secret: a password, a PIN, digits, hidden or sensitive text.
    Secret,
    /// The field is a terminal's, where spoken words would be commands.
    Unsupported,
}

/// What became of committed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextInsertion {
    /// The field took the text, once.
    Inserted,
    /// The field is no longer the one dictation bound: nothing was inserted.
    Changed,
    /// The field may not be dictated into any more.
    Refused(TextFieldRefusal),
}

pub(crate) struct TextInput {
    popups: Vec<PopupSurface>,
    _text_inputs: TextInputManagerState,
    _input_method: InputMethodManagerState,
    _virtual_keyboard: VirtualKeyboardManagerState,
    /// When a key was last typed, and the screen last touched.
    last_key: Option<Instant>,
    last_touch: Option<Instant>,
    /// A touchscreen is attached.
    touchscreen: bool,
    /// A convertible folded into tablet shape: the screen is the keyboard.
    tablet_mode: bool,
    /// The hosted surface whose field an input method serves now, if one does: keys the input
    /// method composes go there, not to the surface.
    host_field: Option<HostField>,
    /// Whether the key being routed came back from a trusted input method through its virtual
    /// keyboard, rather than from a device.
    reinjected: bool,
    /// A key the person pressed reached the focused field, and that field has not yet committed
    /// where the key left its caret: the field it was then, and how long the desk waits for it.
    caret_wait: Option<CaretWait>,
}

/// How long the desk waits for the field a key reached to say where its caret stands before it
/// places a surface at the old one: a frame or two, and a little more for an app that was busy.
const CARET_WAIT: Duration = Duration::from_millis(250);

/// The field a key reached, and when the desk began waiting for its commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaretWait {
    /// The text field's id and the revision of what its client had committed, or `None` when no
    /// field took text then.
    field: Option<(u64, u64)>,
    since: Instant,
}

/// The hosted surface's field an input method serves.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HostField {
    /// Which hosted surface draws it.
    host: usize,
    /// What it declared and where its caret stands.
    input: HostTextInput,
    /// Whether the input method was told to compose in it: a method that connects later is
    /// activated on the pass that sees it.
    activated: bool,
}

/// Where the caret an input method places its composition at stands in the desk, in the protocol's
/// integers.
fn caret_rect(input: HostTextInput) -> Rectangle<i32, Logical> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a caret on the desk, far from i32 limits"
    )]
    let at = (input.x.round() as i32, input.y.round() as i32);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a caret on the desk, far from i32 limits"
    )]
    let size = (
        input.width.max(0.0).round() as i32,
        input.height.max(0.0).round() as i32,
    );
    Rectangle::new(at.into(), size.into())
}

impl TextInput {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            popups: Vec::new(),
            _text_inputs: TextInputManagerState::new::<State>(display),
            // Only the session's own input methods may read and type text.
            _input_method: InputMethodManagerState::new::<State, _>(display, trusted),
            _virtual_keyboard: VirtualKeyboardManagerState::new::<State, _>(display, trusted),
            last_key: None,
            last_touch: None,
            touchscreen: false,
            tablet_mode: false,
            host_field: None,
            reinjected: false,
            caret_wait: None,
        }
    }

    /// What the keyboard follows, at `now` .
    pub(crate) fn entry(&self, typing: bool) -> TextEntry {
        TextEntry {
            wants_typing: typing,
            on_the_glass: self.tablet_mode
                || match (self.last_touch, self.last_key) {
                    (Some(touch), Some(key)) => touch > key,
                    (Some(_), None) => true,
                    _ => false,
                },
            // A touch that reached the desktop came from a touchscreen, and
            // the person may touch it again: the keyboard goes on running,
            // hidden, after the screen that was touched is unplugged.
            touch_available: self.touchscreen || self.last_touch.is_some() || self.tablet_mode,
        }
    }
}

impl State {
    /// Note what the person is typing with: a touch on the screen or a key.
    pub(crate) fn note_typing(&mut self, event: &InputEvent, now: Instant) {
        match event {
            InputEvent::Key { .. } => self.text_input.last_key = Some(now),
            InputEvent::TouchDown { .. } | InputEvent::TouchMotion { .. } => {
                self.text_input.last_touch = Some(now);
            }
            InputEvent::Switch {
                switch: Switch::TabletMode,
                on,
            } => self.text_input.tablet_mode = *on,
            _ => {}
        }
    }

    /// A touchscreen was attached, or went away.
    pub(crate) fn note_touchscreen(&mut self, attached: bool) {
        self.text_input.touchscreen = attached;
    }

    /// What a text field and the person's own typing ask the on-screen keyboard to do.
    pub(crate) fn text_entry(&self) -> TextEntry {
        let mut typing = self.text_input.host_field.is_some();
        self.seat
            .text_input()
            .with_active_text_input(|_, _| typing = true);
        self.text_input.entry(typing)
    }

    /// The hosted surface whose field the person types into now: the topmost one that holds the
    /// keyboard and has a field with the focus, while the session is not locked. A surface that
    /// keeps its field focused under another that took the keys (the desk's search under the
    /// clipboard opened over it) is not typed into until it has them back.
    fn host_text_input(&self) -> Option<(usize, HostTextInput)> {
        if self.locked() {
            return None;
        }
        self.hosts
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, host)| host.wants_keyboard())
            .find_map(|(index, host)| host.text_input().map(|input| (index, input)))
    }

    /// Keep the field the person types into on the keyboard's focus, whoever draws it: a client
    /// tells its `text-input-v3` object, and the compositor's own fields are served by the input
    /// method directly.
    /// A hosted field holds the keyboard, so no client's field has the focus while it is served:
    /// the input method composes there, and its candidate window stands where the field's caret
    /// does.
    pub(crate) fn sync_text_input(&mut self) {
        let host = self.host_text_input();
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let handle = self.seat.text_input().clone();
        if handle.focus() != focused {
            handle.leave();
            let enter = focused.is_some();
            handle.set_focus(focused);
            if enter {
                handle.enter();
            }
            handle.done(false);
        }
        let input_method = self.seat.input_method().clone();
        match (host, self.text_input.host_field) {
            (None, None) => {}
            (None, Some(served)) => {
                // A client's field may have taken the focus in the same pass: it enables the method
                // itself, so the composition it just asked for is not torn down under it.
                let mut client_field = false;
                self.seat
                    .text_input()
                    .with_active_text_input(|_, _| client_field = true);
                if !client_field {
                    input_method.deactivate_input_method(self);
                }
                self.text_input.host_field = None;
                self.notify_host(served.host, HostIme::Disabled);
            }
            (Some((index, input)), Some(mut served)) if served.host == index => {
                if served.input != input && caret_rect(served.input) != caret_rect(input) {
                    // The composition follows the caret: the candidate window stands under it.
                    input_method.set_text_input_rectangle(self, caret_rect(input));
                }
                served.input = input;
                if served.activated && !input_method.has_instance() {
                    // The method went away: the composition is over, and one that connects later is
                    // activated on the pass that sees it.
                    input_method.deactivate_input_method(self);
                    served.activated = false;
                    self.notify_host(index, HostIme::Disabled);
                    self.text_input.host_field = Some(served);
                    return;
                }
                if !served.activated && input_method.has_instance() {
                    // The method may connect after the field took the focus: the pass that first
                    // sees both activates it.
                    input_method.activate_without_text_input(self, caret_rect(input));
                    served.activated = true;
                    self.text_input.host_field = Some(served);
                    self.notify_host(index, HostIme::Enabled);
                    return;
                }
                self.text_input.host_field = Some(served);
            }
            (Some((index, input)), served) => {
                if served.is_some() {
                    input_method.deactivate_input_method(self);
                }
                if let Some(served) = served {
                    self.notify_host(served.host, HostIme::Disabled);
                }
                let activated = input_method.has_instance();
                if activated {
                    input_method.activate_without_text_input(self, caret_rect(input));
                }
                self.text_input.host_field = Some(HostField {
                    host: index,
                    input,
                    activated,
                });
                if activated {
                    // Only a method that composes there is told of it: the pass that first sees one
                    // activates this field.
                    self.notify_host(index, HostIme::Enabled);
                }
            }
        }
    }

    /// Whether an input method is connected now, and so serves the field the person types into,
    /// whoever draws it.
    pub(crate) fn input_method_connected(&self) -> bool {
        self.seat.input_method().has_instance()
    }

    /// Hand `event` to hosted surface `host` , if it still stands.
    fn notify_host(&mut self, host: usize, event: HostIme) {
        if let Some(host) = self.hosts.get_mut(host) {
            host.ime(event);
        }
    }

    /// Hand `event` to the hosted field the input method serves now.
    fn notify_served_field(&mut self, event: HostIme) {
        if let Some(served) = self.text_input.host_field {
            self.notify_host(served.host, event);
        }
    }

    /// A key a trusted input method sent back through its virtual keyboard: routed as the person's
    /// own, so the field the method serves — a client's or the compositor's — hears what it left
    /// for it.
    pub(crate) fn handle_ime_key(&mut self, keycode: u32, pressed: bool) {
        self.text_input.reinjected = true;
        self.route_input(InputEvent::Key { keycode, pressed });
        self.text_input.reinjected = false;
    }

    /// The hosted surface whose field the input method takes the person's keys for now: it has the
    /// keyboard grabbed, so every key reaches the method first, and the surface hears only what the
    /// method leaves for it (`State::handle_ime_key`).
    pub(crate) fn composing(&self) -> Option<usize> {
        if self.text_input.reinjected || !self.seat.input_method().keyboard_grabbed() {
            return None;
        }
        self.text_input.host_field.map(|served| served.host)
    }

    /// The text field the person is typing into, as dictation names it.
    pub(crate) fn dictation_field(&self) -> Result<TextField, TextFieldRefusal> {
        let mut field = Err(TextFieldRefusal::NoField);
        self.seat
            .text_input()
            .with_active_text_input_committed(|input, surface, committed| {
                let (hint, purpose) = content_type(committed);
                let refusal = if secret(committed.content_type) {
                    Some(TextFieldRefusal::Secret)
                } else if purpose == ContentPurpose::Terminal {
                    Some(TextFieldRefusal::Unsupported)
                } else {
                    None
                };
                field = refusal.map_or_else(
                    || {
                        Ok(TextField {
                            id: field_id(input, surface),
                            revision: u64::from(committed.serial),
                            multiline: hint.contains(ContentHint::Multiline),
                        })
                    },
                    Err,
                );
            });
        field
    }

    /// Insert committed text into `field` once, if the person is still typing into it and it may
    /// take dictated text. The text goes through the trusted input method, never as keystrokes.
    pub(crate) fn insert_dictation(&self, field: TextField, text: &str) -> TextInsertion {
        let current = match self.dictation_field() {
            Ok(current) => current,
            Err(refusal) => return TextInsertion::Refused(refusal),
        };
        if current.id != field.id || current.revision != field.revision {
            return TextInsertion::Changed;
        }
        self.seat
            .text_input()
            .with_active_text_input(|input, _| input.commit_string(Some(text.to_owned())));
        self.seat.text_input().done(false);
        TextInsertion::Inserted
    }

    /// Insert `text` into the field the person is typing into, whatever it asks for: an emoji holds
    /// nothing of the field's secret. The text goes through the trusted input method, never as
    /// keystrokes.
    /// Answers whether the field declared its content secret, which decides whether the text is
    /// remembered; `None` when no field takes text now.
    pub(crate) fn insert_active_text(&mut self, text: &str) -> Option<bool> {
        if let Some(served) = self.text_input.host_field {
            // A hosted surface's field takes text the same way, through the input method that
            // serves it.
            self.notify_served_field(HostIme::Commit(text.to_owned()));
            return Some(secret(Some((
                ContentHint::from_bits_truncate(served.input.hint),
                ContentPurpose::try_from(served.input.purpose).unwrap_or(ContentPurpose::Normal),
            ))));
        }
        let mut sensitive = None;
        self.seat
            .text_input()
            .with_active_text_input_committed(|input, _, committed| {
                sensitive = Some(secret(committed.content_type));
                input.commit_string(Some(text.to_owned()));
            });
        if sensitive.is_some() {
            self.seat.text_input().done(false);
        }
        sensitive
    }

    /// Where the focused field's caret stands in the desk, in logical coordinates: the cursor
    /// rectangle its client last committed, placed on the surface it types into. `None` when no
    /// field takes text, it never reported one, or its window is not on screen.
    pub(crate) fn text_caret(&self) -> Option<TextCaret> {
        if let Some(served) = self.text_input.host_field {
            let input = served.input;
            return Some(TextCaret {
                x: input.x,
                y: input.y,
                width: input.width.max(0.0),
                height: input.height.max(0.0),
            });
        }
        let mut caret = None;
        self.seat
            .text_input()
            .with_active_text_input_committed(|_, surface, committed| {
                let Some(rect) = committed.cursor_rectangle else {
                    return;
                };
                let Some(window) = self.window_geometry(surface) else {
                    return;
                };
                caret = Some(TextCaret {
                    x: f64::from(window.loc.x.saturating_add(rect.loc.x)),
                    y: f64::from(window.loc.y.saturating_add(rect.loc.y)),
                    width: f64::from(rect.size.w.max(0)),
                    height: f64::from(rect.size.h.max(0)),
                });
            });
        caret
    }

    /// The focused field's identity and the revision of what its client committed: a key that
    /// moves the caret commits a new revision.
    fn caret_field(&self) -> Option<(u64, u64)> {
        let mut field = None;
        self.seat
            .text_input()
            .with_active_text_input_committed(|input, surface, committed| {
                field = Some((field_id(input, surface), u64::from(committed.serial)));
            });
        field
    }

    /// A key reached the focused client: where its caret stands is not known until that client
    /// commits the frame the key moved it in.
    pub(crate) fn note_caret_may_move(&mut self) {
        let field = self.caret_field();
        if field.is_some() {
            self.text_input.caret_wait = Some(CaretWait {
                field,
                since: Instant::now(),
            });
        }
    }

    /// Whether the focused field's caret is committed for the keys it received: a surface a key
    /// opens at the caret waits for it, so it stands under where the person's caret is now, not
    /// where it was before their last redraw.
    pub(crate) fn caret_settled(&mut self) -> bool {
        let Some(wait) = self.text_input.caret_wait else {
            return true;
        };
        let settled = self.caret_field() != wait.field || wait.since.elapsed() >= CARET_WAIT;
        if settled {
            self.text_input.caret_wait = None;
        }
        settled
    }

    /// Where `surface` stands in the desk: a window of the space, or a popup on one — a field the
    /// person types into may live in either.
    fn window_geometry(&self, surface: &WlSurface) -> Option<Rectangle<i32, Logical>> {
        for window in self.space.elements() {
            // An X11 window has no Wayland toplevel and holds no field of these: the windows after
            // it are still looked through.
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let rect = self.space.element_geometry(window)?;
            if toplevel.wl_surface() == surface {
                return Some(rect);
            }
            let popup = PopupManager::popups_for_surface(toplevel.wl_surface())
                .find(|(popup, _)| popup.wl_surface() == surface);
            if let Some((_, offset)) = popup {
                return Some(Rectangle::new(rect.loc + offset, rect.size));
            }
        }
        None
    }
}

/// Where a text field's caret stands, in the desk's logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextCaret {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What a field declared of itself: the content type it last committed, or the one of an ordinary
/// one-line text field when it never said.
fn content_type(committed: &CommittedState) -> (ContentHint, ContentPurpose) {
    committed
        .content_type
        .unwrap_or((ContentHint::empty(), ContentPurpose::Normal))
}

/// Whether a field whose content type is `content` takes a secret. Digits count too: the on-screen
/// keyboard treats a digits field as a PIN pad.
fn secret(content: Option<(ContentHint, ContentPurpose)>) -> bool {
    let (hint, purpose) = content.unwrap_or((ContentHint::empty(), ContentPurpose::Normal));
    hint.intersects(ContentHint::HiddenText | ContentHint::SensitiveData)
        || matches!(
            purpose,
            ContentPurpose::Digits | ContentPurpose::Password | ContentPurpose::Pin
        )
}

/// Which field a text-input object makes on a surface, for the life of this compositor: neither id
/// is reused while it runs.
fn field_id(input: &ZwpTextInputV3, surface: &WlSurface) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.id().hash(&mut hasher);
    surface.id().hash(&mut hasher);
    hasher.finish()
}

impl State {
    /// Input method popups on `output` , front first.
    pub(crate) fn input_popup_elements<R>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let Some(geometry) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = Scale::from(output.current_scale().fractional_scale());
        self.text_input
            .popups
            .iter()
            .rev()
            .flat_map(|popup| {
                let text = popup.text_input_rectangle();
                // Under the text being composed: on the field's own surface,
                // or, for a field the compositor serves itself, where the
                // field reported its caret.
                let caret = Point::from((text.loc.x, text.loc.y + text.size.h));
                let at = match popup.get_parent() {
                    Some(parent) => parent.location.loc + caret - geometry.loc,
                    None => caret - geometry.loc,
                };
                let at: Point<i32, Physical> = at.to_physical_precise_round(scale);
                render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    at,
                    scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .collect()
    }
}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.text_input
            .popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
        self.text_input.popups.push(surface);
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.text_input
            .popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        self.new_popup(surface);
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.window_geometry(parent).unwrap_or_default()
    }

    /// The input method composed `text` for a field the compositor draws: the text takes the place
    /// of the composition shown there, once.
    fn commit_to_host(&mut self, text: &str) {
        self.notify_served_field(HostIme::Commit(text.to_owned()));
    }

    fn preedit_to_host(&mut self, text: &str, cursor_begin: i32, cursor_end: i32) {
        let cursor = (cursor_begin >= 0).then(|| {
            (
                cursor_begin.unsigned_abs() as usize,
                cursor_end.unsigned_abs() as usize,
            )
        });
        self.notify_served_field(HostIme::Preedit(text.to_owned(), cursor));
    }
}

impl VirtualKeyboardHandler for State {
    fn virtual_key(&mut self, key: u32, pressed: bool, _time: u32) -> bool {
        // The protocol's key codes sit 8 above the evdev codes the router reads, as a client's do.
        self.handle_ime_key(key.saturating_sub(8), pressed);
        true
    }
}

smithay::delegate_text_input_manager!(State);
smithay::delegate_input_method_manager!(State);
smithay::delegate_virtual_keyboard_manager!(State);
