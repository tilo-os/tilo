//! Text input for clients (`text-input-v3`) served by a trusted input
//! method (`input-method-v2`, `virtual-keyboard-v1`): the on-screen
//! keyboard or an IME. Its candidate popups are drawn under the text they
//! belong to, above every window.
//!
//! A field is also named for dictation: which field the person types into
//! ([`TextField`]), and inserting committed text into it once, through the
//! same input method ([`State::insert_dictation`]).

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical coordinates of windows and outputs, far from i32 limits"
)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::time::Instant;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ContentHint, ContentPurpose, ZwpTextInputV3,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
use smithay::wayland::text_input::{TextInputManagerState, TextInputSeat};
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

use crate::input::{InputEvent, Switch};
use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};

fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::trusted)
}

/// Whether a text field waits for typing, and whether the person is typing
/// on the glass: what the on-screen keyboard follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextEntry {
    /// A text field has the focus and has asked for typing.
    pub wants_typing: bool,
    /// The last typing was a touch on the screen: a hardware key since then
    /// hands back to the keyboard the person is holding.
    pub on_the_glass: bool,
}

/// A text field as dictation names it: which field the person types into,
/// and the revision of what its client last committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextField {
    /// The field's identity within this compositor's life: its text-input
    /// object together with the surface it types into.
    pub id: u64,
    /// The serial of its client's last committed state: any commit since
    /// (a caret moved, a layout change, typing) moves it.
    pub revision: u64,
    /// Its client takes more than one line.
    pub multiline: bool,
}

/// Why dictated text may not be written into the field the person is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFieldRefusal {
    /// No field has the focus and asks for typing.
    NoField,
    /// The field takes a secret: a password, a PIN, digits, hidden or
    /// sensitive text.
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
    /// A convertible folded into tablet shape: the screen is the keyboard.
    tablet_mode: bool,
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
            tablet_mode: false,
        }
    }

    /// What the keyboard follows, at `now`.
    pub(crate) fn entry(&self, typing: bool) -> TextEntry {
        TextEntry {
            wants_typing: typing,
            on_the_glass: self.tablet_mode
                || match (self.last_touch, self.last_key) {
                    (Some(touch), Some(key)) => touch > key,
                    (Some(_), None) => true,
                    _ => false,
                },
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

    /// What a text field and the person's own typing ask the on-screen
    /// keyboard to do.
    pub(crate) fn text_entry(&self) -> TextEntry {
        let mut typing = false;
        self.seat
            .text_input()
            .with_active_text_input(|_, _| typing = true);
        self.text_input.entry(typing)
    }

    /// The text field the person is typing into, as dictation names it.
    pub(crate) fn dictation_field(&self) -> Result<TextField, TextFieldRefusal> {
        let mut field = Err(TextFieldRefusal::NoField);
        self.seat.text_input().with_active_text_input_committed(
            |input, surface, revision, content| {
                // A field that never declared a content type is an ordinary
                // one-line text field.
                let (hint, purpose) =
                    content.unwrap_or((ContentHint::empty(), ContentPurpose::Normal));
                // Digits count as a secret: the on-screen keyboard treats a
                // digits field as a PIN pad too.
                let secret = hint.intersects(ContentHint::HiddenText | ContentHint::SensitiveData)
                    || matches!(
                        purpose,
                        ContentPurpose::Digits | ContentPurpose::Password | ContentPurpose::Pin
                    );
                let refusal = if secret {
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
                            revision: u64::from(revision),
                            multiline: hint.contains(ContentHint::Multiline),
                        })
                    },
                    Err,
                );
            },
        );
        field
    }

    /// Insert committed text into `field` once, if the person is still
    /// typing into it and it may take dictated text. The text goes through
    /// the trusted input method, never as keystrokes.
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
}

/// Which field a text-input object makes on a surface, for the life of this
/// compositor: neither id is reused while it runs.
fn field_id(input: &ZwpTextInputV3, surface: &WlSurface) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.id().hash(&mut hasher);
    surface.id().hash(&mut hasher);
    hasher.finish()
}

impl State {
    /// Input method popups on `output`, front first.
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
            .filter_map(|popup| {
                let parent = popup.get_parent()?;
                let text = popup.text_input_rectangle();
                // Under the text being composed.
                let at = parent.location.loc + Point::from((text.loc.x, text.loc.y + text.size.h))
                    - geometry.loc;
                let at: Point<i32, Physical> = at.to_physical_precise_round(scale);
                Some(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    at,
                    scale,
                    1.0,
                    Kind::Unspecified,
                ))
            })
            .flatten()
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
        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == parent)
            })
            .and_then(|window| self.space.element_geometry(window))
            .unwrap_or_default()
    }
}

smithay::delegate_text_input_manager!(State);
smithay::delegate_input_method_manager!(State);
smithay::delegate_virtual_keyboard_manager!(State);
