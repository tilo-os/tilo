//! Raw keys, pastes and pointers a focused component sends somewhere else.
//!
//! A component that shows what another program draws — a terminal's grid, a remote desktop's
//! framebuffer, a drawing surface someone types into — takes the window's keys itself and hands
//! them to that program. This is the one vocabulary and the one path they share: the window
//! holds the destinations by the id a view names ([`set_window_inputs`]), and every component
//! that takes raw input finds its own there. Encoding what a key means is the destination's: a
//! shell's key table is the engine's, and no second copy lives in the toolkit.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// The keys a program reads as an escape sequence rather than as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Enter,
    Space,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// A function key, numbered from 1.
    Function(u8),
}

/// One key press: a character, or a named key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Named(Named),
}

/// The modifiers held beside a key or a button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub sup: bool,
}

/// Which button was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// Something that takes a focused component's raw input.
///
/// The handle is shared and may be called from the frame that draws the component.
pub trait Keys: Send + Sync {
    /// Send `key`, with `text` as the layout produced it for a character key.
    fn key(&self, key: Key, text: Option<&str>, held: Held);
    /// Send `text` as typed input, not as a paste (an input method's commit).
    fn text(&self, text: &str);
    /// Paste `text` as the destination asked pastes to arrive.
    fn paste(&self, text: &str);
    /// Report a pointer button at a one-based cell; `false` when the destination takes no
    /// pointer and the component acts on it itself.
    fn mouse(&self, button: Button, pressed: bool, held: Held, column: usize, row: usize) -> bool;
    /// The component took or lost the keyboard.
    fn focus(&self, focused: bool);
}

/// A window's input destinations by the id the app names them by.
pub type Inputs = HashMap<String, Arc<dyn Keys>>;

thread_local! {
    /// The destinations the window drawing now hands the components it builds.
    static WINDOW_INPUTS: RefCell<Option<Arc<Inputs>>> = const { RefCell::new(None) };
}

/// Hand `inputs` to the components the window builds until the next call; `None` when the
/// window draws none.
pub fn set_window_inputs(inputs: Option<Arc<Inputs>>) {
    WINDOW_INPUTS.with(|held| *held.borrow_mut() = inputs);
}

/// The destinations the window drawing now hands its components.
#[must_use]
pub fn window_inputs() -> Option<Arc<Inputs>> {
    WINDOW_INPUTS.with(|held| held.borrow().clone())
}

/// The destination `id` names, as the window drawing now holds it.
#[must_use]
pub fn window_input(id: &str) -> Option<Arc<dyn Keys>> {
    WINDOW_INPUTS.with(|held| {
        held.borrow()
            .as_ref()
            .and_then(|inputs| inputs.get(id).cloned())
    })
}

/// Take the destination `id` names out of what the window drew with.
#[must_use]
pub fn take_window_input(id: &str) -> Option<Arc<dyn Keys>> {
    WINDOW_INPUTS.with(|held| {
        let mut held = held.borrow_mut();
        let inputs = Arc::make_mut(held.as_mut()?);
        inputs.remove(id)
    })
}

/// The key `key` names in this vocabulary, when it is one a destination reads.
#[must_use]
pub fn map_key(key: &winit::keyboard::Key) -> Option<Key> {
    use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};
    match key {
        WKey::Character(text) => text.chars().next().map(Key::Char),
        WKey::Named(named) => Some(Key::Named(match named {
            WNamedKey::Enter => Named::Enter,
            WNamedKey::Space => Named::Space,
            WNamedKey::Backspace => Named::Backspace,
            WNamedKey::Tab => Named::Tab,
            WNamedKey::Escape => Named::Escape,
            WNamedKey::ArrowUp => Named::ArrowUp,
            WNamedKey::ArrowDown => Named::ArrowDown,
            WNamedKey::ArrowLeft => Named::ArrowLeft,
            WNamedKey::ArrowRight => Named::ArrowRight,
            WNamedKey::Home => Named::Home,
            WNamedKey::End => Named::End,
            WNamedKey::PageUp => Named::PageUp,
            WNamedKey::PageDown => Named::PageDown,
            WNamedKey::Insert => Named::Insert,
            WNamedKey::Delete => Named::Delete,
            WNamedKey::F1 => Named::Function(1),
            WNamedKey::F2 => Named::Function(2),
            WNamedKey::F3 => Named::Function(3),
            WNamedKey::F4 => Named::Function(4),
            WNamedKey::F5 => Named::Function(5),
            WNamedKey::F6 => Named::Function(6),
            WNamedKey::F7 => Named::Function(7),
            WNamedKey::F8 => Named::Function(8),
            WNamedKey::F9 => Named::Function(9),
            WNamedKey::F10 => Named::Function(10),
            WNamedKey::F11 => Named::Function(11),
            WNamedKey::F12 => Named::Function(12),
            // A modifier or a key no destination reads as a sequence stays with the window.
            _ => return None,
        })),
        _ => None,
    }
}

/// The modifiers held, in this vocabulary.
#[must_use]
pub fn held(modifiers: &winit::keyboard::ModifiersState) -> Held {
    Held {
        shift: modifiers.shift_key(),
        alt: modifiers.alt_key(),
        ctrl: modifiers.control_key(),
        sup: modifiers.super_key(),
    }
}
