//! A web page running on another machine, drawn here from its paint stream.
//!
//! A session shares a browser tab as a `tilo-paint` legacy-window stream:
//! a whole frame, then patches and the image blobs its items reference
//! (`tilo_paint_stream::wire`). [`RemotePage`] keeps the latest frame and,
//! with the `toolkit` feature, is the engine of an ordinary
//! `ui::widget::WebView`, so the page is drawn by the toolkit inside the
//! desktop like every other window, never as video. Pointer, wheel and keys
//! on it become the legacy-window input the host forwards to the session,
//! in page (CSS) pixels.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tilo_paint_stream::Frame;
use tilo_paint_stream::wire::{Payload, Receiver};
use tilo_relay_protocol::legacy_window::InputEvent;

/// Image bytes kept for one page; past this the oldest blobs are dropped.
const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// Input events waiting for the page to send; past this the oldest go.
const MAX_INPUT: usize = 256;

/// Primary button: Linux input codes, as `legacy_window::InputEvent` speaks evdev.
pub const BTN_LEFT: u32 = 272;
/// Secondary button.
pub const BTN_RIGHT: u32 = 273;
/// Middle button.
pub const BTN_MIDDLE: u32 = 274;
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFTMETA: u32 = 125;

/// Modifier keys held with a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per physical modifier key, as every keyboard API reports them"
)]
pub struct Held {
    /// Control.
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
    /// Super / Command.
    pub meta: bool,
}

#[derive(Default)]
struct State {
    receiver: Receiver,
    frame: Option<Arc<Frame>>,
    blobs: HashMap<[u8; 32], Arc<Vec<u8>>>,
    blob_order: VecDeque<[u8; 32]>,
    blob_bytes: usize,
    input: VecDeque<InputEvent>,
    scale: f32,
}

/// A page another machine streams. See the module docs.
pub struct RemotePage {
    state: Mutex<State>,
    #[cfg(feature = "toolkit")]
    frames: ui::webview::FrameSlot,
}

impl RemotePage {
    /// A page showing nothing until the stream's first whole frame.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                scale: 1.0,
                ..State::default()
            }),
            #[cfg(feature = "toolkit")]
            frames: Arc::new(Mutex::new(None)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    /// Apply one stream frame. Returns whether what the page shows changed.
    pub fn push(&self, bytes: &[u8]) -> bool {
        let Ok(payload) = Payload::decode(bytes) else {
            return false;
        };
        let mut state = self.lock();
        let shows_frame = !matches!(payload, Payload::Blob { .. });
        if let Some((hash, bytes)) = state.receiver.apply(payload) {
            // A blob is trusted only under its own name.
            if *blake3::hash(&bytes).as_bytes() != hash {
                return false;
            }
            Self::keep_blob(&mut state, hash, bytes);
            return true;
        }
        if !shows_frame {
            return false;
        }
        let next = state.receiver.frame().map(|f| Arc::new(f.clone()));
        let changed = next.as_ref().map(|f| f.seq) != state.frame.as_ref().map(|f| f.seq);
        state.frame = next;
        changed
    }

    fn keep_blob(state: &mut State, hash: [u8; 32], bytes: Vec<u8>) {
        if state.blobs.contains_key(&hash) || bytes.len() > MAX_BLOB_BYTES {
            return;
        }
        state.blob_bytes = state.blob_bytes.saturating_add(bytes.len());
        state.blobs.insert(hash, Arc::new(bytes));
        state.blob_order.push_back(hash);
        while state.blob_bytes > MAX_BLOB_BYTES {
            let Some(oldest) = state.blob_order.pop_front() else {
                break;
            };
            if let Some(gone) = state.blobs.remove(&oldest) {
                state.blob_bytes = state.blob_bytes.saturating_sub(gone.len());
            }
        }
    }

    /// The page as the stream last described it.
    #[must_use]
    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.lock().frame.clone()
    }

    /// An image the frame references, once its blob arrived.
    #[must_use]
    pub fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.lock().blobs.get(hash).cloned()
    }

    /// The input for the host since the last call, oldest first.
    pub fn take_input(&self) -> Vec<InputEvent> {
        self.lock().input.drain(..).collect()
    }

    /// Device pixels per page pixel on this screen.
    pub fn set_scale(&self, scale: f32) {
        self.lock().scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
    }

    fn page(state: &State, v: f32) -> f64 {
        f64::from(v / state.scale)
    }

    /// The pointer moved to device pixel `(x, y)` of the view.
    pub fn pointer_motion(&self, x: f32, y: f32) {
        let mut state = self.lock();
        let event = InputEvent::PointerMotion {
            x: Self::page(&state, x),
            y: Self::page(&state, y),
        };
        Self::queue(&mut state, event);
        drop(state);
    }

    /// A button (`BTN_*`) changed at device pixel `(x, y)`.
    pub fn pointer_button(&self, button: u32, pressed: bool, x: f32, y: f32) {
        self.pointer_motion(x, y);
        Self::queue(
            &mut self.lock(),
            InputEvent::PointerButton { button, pressed },
        );
    }

    /// The wheel scrolled by device pixels.
    pub fn scroll(&self, dx: f32, dy: f32) {
        let mut state = self.lock();
        let event = InputEvent::PointerAxis {
            dx: Self::page(&state, dx),
            dy: Self::page(&state, dy),
        };
        Self::queue(&mut state, event);
        drop(state);
    }

    /// Keyboard focus entered or left the page.
    pub fn focus(&self, focused: bool) {
        Self::queue(
            &mut self.lock(),
            InputEvent::KeyboardFocus {
                surface: 0,
                focused,
            },
        );
    }

    /// A key with evdev code `keysym` changed; `shifted` when the character
    /// typed needs Shift on a US layout.
    pub fn key(&self, keysym: u32, shifted: bool, down: bool, held: Held) {
        let mut state = self.lock();
        // Modifiers reach the host as their own keys around the press, so
        // the session's keyboard state matches what the person held.
        let mods: Vec<u32> = [
            (held.ctrl, KEY_LEFTCTRL),
            (held.alt, KEY_LEFTALT),
            (held.meta, KEY_LEFTMETA),
            (held.shift || shifted, KEY_LEFTSHIFT),
        ]
        .into_iter()
        .filter_map(|(on, code)| on.then_some(code))
        .collect();
        let press = |state: &mut State, keysym, pressed| {
            Self::queue(
                state,
                InputEvent::Key {
                    keysym,
                    pressed,
                    modifiers: 0,
                },
            );
        };
        if down {
            for &code in &mods {
                press(&mut state, code, true);
            }
            press(&mut state, keysym, true);
        } else {
            press(&mut state, keysym, false);
            for &code in mods.iter().rev() {
                press(&mut state, code, false);
            }
        }
    }

    /// One typed character, on a US layout. Characters it lacks are dropped.
    pub fn character(&self, text: &str, down: bool, held: Held) {
        if let Some((keysym, shifted)) = evdev_char(text) {
            self.key(keysym, shifted, down, held);
        }
    }

    fn queue(state: &mut State, event: InputEvent) {
        // Latest-wins pointer motion, as the page's own input layer does.
        if let (InputEvent::PointerMotion { .. }, Some(InputEvent::PointerMotion { .. })) =
            (&event, state.input.back())
        {
            state.input.pop_back();
        }
        if state.input.len() >= MAX_INPUT {
            state.input.pop_front();
        }
        state.input.push_back(event);
    }
}

/// The evdev code of a single character on a US layout, and whether it needs
/// Shift.
#[must_use]
pub fn evdev_char(text: &str) -> Option<(u32, bool)> {
    const ROW_Q: &str = "qwertyuiop";
    const ROW_A: &str = "asdfghjkl";
    const ROW_Z: &str = "zxcvbnm";
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    const PUNCT: [(char, u32, bool); 23] = [
        (' ', 57, false),
        ('-', 12, false),
        ('_', 12, true),
        ('=', 13, false),
        ('+', 13, true),
        ('[', 26, false),
        ('{', 26, true),
        (']', 27, false),
        ('}', 27, true),
        (';', 39, false),
        (':', 39, true),
        ('\'', 40, false),
        ('"', 40, true),
        ('`', 41, false),
        ('~', 41, true),
        ('\\', 43, false),
        ('|', 43, true),
        (',', 51, false),
        ('<', 51, true),
        ('.', 52, false),
        ('>', 52, true),
        ('/', 53, false),
        ('?', 53, true),
    ];
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let lower = c.to_ascii_lowercase();
    let shifted = c.is_ascii_uppercase();
    let row = |row: &str, first: u32| {
        row.find(lower)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| (first.saturating_add(i), shifted))
    };
    if let Some(found) = row(ROW_Q, 16)
        .or_else(|| row(ROW_A, 30))
        .or_else(|| row(ROW_Z, 44))
    {
        return Some(found);
    }
    let digit = |d: u32| if d == 0 { 11 } else { d.saturating_add(1) };
    if let Some(d) = c.to_digit(10) {
        return Some((digit(d), false));
    }
    if let Some(i) = SHIFTED_DIGITS.find(c).and_then(|i| u32::try_from(i).ok()) {
        return Some((digit(i), true));
    }
    PUNCT
        .iter()
        .find(|(p, _, _)| *p == c)
        .map(|(_, code, shift)| (*code, *shift))
}

/// The toolkit's engine seam: a `WebView` over a [`RemotePage`].
#[cfg(feature = "toolkit")]
mod engine {
    use std::sync::Arc;

    use ui::webview::{
        FrameSlot, KeyInput, KeyModifiers, MouseButton, NamedKey, WebViewCommand, WebViewEngine,
        WebViewEvent,
    };

    use super::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Held, RemotePage};

    const fn named(key: NamedKey) -> u32 {
        match key {
            NamedKey::Escape => 1,
            NamedKey::Backspace => 14,
            NamedKey::Tab => 15,
            NamedKey::Enter => 28,
            NamedKey::Home => 102,
            NamedKey::ArrowUp => 103,
            NamedKey::PageUp => 104,
            NamedKey::ArrowLeft => 105,
            NamedKey::ArrowRight => 106,
            NamedKey::End => 107,
            NamedKey::ArrowDown => 108,
            NamedKey::PageDown => 109,
            NamedKey::Delete => 111,
        }
    }

    const fn held(m: KeyModifiers) -> Held {
        Held {
            ctrl: m.ctrl,
            shift: m.shift,
            alt: m.alt,
            meta: m.meta,
        }
    }

    impl WebViewEngine for RemotePage {
        fn send(&self, cmd: WebViewCommand) {
            match cmd {
                WebViewCommand::Resize { scale_factor, .. } => self.set_scale(scale_factor),
                WebViewCommand::MouseMove { x, y } => self.pointer_motion(x, y),
                WebViewCommand::MouseButton { button, down, x, y } => {
                    let button = match button {
                        MouseButton::Left => BTN_LEFT,
                        MouseButton::Middle => BTN_MIDDLE,
                        MouseButton::Right => BTN_RIGHT,
                    };
                    self.pointer_button(button, down, x, y);
                }
                WebViewCommand::Scroll { dx, dy, .. } => self.scroll(dx, dy),
                WebViewCommand::Key {
                    key,
                    down,
                    modifiers,
                } => match key {
                    KeyInput::Named(key) => self.key(named(key), false, down, held(modifiers)),
                    KeyInput::Character(text) => self.character(&text, down, held(modifiers)),
                },
                WebViewCommand::SetFocused(focused) => self.focus(focused),
                _ => {}
            }
        }

        fn frames(&self) -> FrameSlot {
            Arc::clone(&self.frames)
        }

        fn drain_events(&self) -> Vec<WebViewEvent> {
            Vec::new()
        }

        fn paint_frame(&self) -> Option<Arc<tilo_paint_stream::Frame>> {
            self.frame()
        }

        fn paint_blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
            self.blob(hash)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_paint_stream::{Color, Item, ItemKey, Layer, Point, ROOT, Rect};

    fn frame(seq: u64, color: u8) -> Frame {
        let mut f = Frame {
            seq,
            viewport: Point { x: 100.0, y: 100.0 },
            ..Frame::default()
        };
        f.layers.insert(
            ROOT,
            Layer {
                size: Point { x: 100.0, y: 100.0 },
                clips: true,
                opacity: 1.0,
                items: vec![Item::Fill {
                    key: ItemKey { node: 1, part: 0 },
                    rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                    color: Color([color, 0, 0, 255]),
                    radius: 0.0,
                }],
                ..Layer::default()
            },
        );
        f
    }

    fn bytes(payload: &Payload) -> Vec<u8> {
        payload.encode().expect("encode")
    }

    #[test]
    fn a_stream_becomes_the_frame_the_page_shows() {
        let page = RemotePage::new();
        assert!(page.frame().is_none(), "nothing before a whole frame");
        let (a, b) = (frame(1, 10), frame(2, 20));
        assert!(!page.push(&bytes(&Payload::Patches(vec![a.diff(&b)]))));
        assert!(page.push(&bytes(&Payload::Key(a.clone()))));
        assert_eq!(page.frame().as_deref(), Some(&a));
        assert!(page.push(&bytes(&Payload::Patches(vec![a.diff(&b)]))));
        assert_eq!(page.frame().as_deref(), Some(&b));
        assert!(!page.push(b"not a payload"));
    }

    #[test]
    fn blobs_are_kept_only_under_their_own_hash() {
        let page = RemotePage::new();
        let real = vec![1_u8, 2, 3];
        let hash = *blake3::hash(&real).as_bytes();
        assert!(!page.push(&bytes(&Payload::Blob {
            hash,
            bytes: vec![9, 9]
        })));
        assert!(page.blob(&hash).is_none(), "a forged blob is refused");
        assert!(page.push(&bytes(&Payload::Blob {
            hash,
            bytes: real.clone()
        })));
        assert_eq!(page.blob(&hash).as_deref(), Some(&real));
    }

    #[test]
    fn pointer_and_keys_become_host_input_in_page_pixels() {
        let page = RemotePage::new();
        page.set_scale(2.0);
        page.pointer_motion(10.0, 10.0);
        page.pointer_button(BTN_LEFT, true, 40.0, 60.0);
        page.character("A", true, Held::default());
        page.character("€", true, Held::default());
        page.scroll(0.0, 80.0);
        let input = page.take_input();
        assert_eq!(
            input,
            vec![
                InputEvent::PointerMotion { x: 20.0, y: 30.0 },
                InputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: true
                },
                InputEvent::Key {
                    keysym: KEY_LEFTSHIFT,
                    pressed: true,
                    modifiers: 0
                },
                InputEvent::Key {
                    keysym: 30,
                    pressed: true,
                    modifiers: 0
                },
                InputEvent::PointerAxis { dx: 0.0, dy: 40.0 },
            ],
            "motion coalesces, device pixels become page pixels, capitals hold Shift, \
             characters a US layout lacks are dropped"
        );
        let request = tilo_relay_protocol::legacy_window::InputRequest {
            workspace_id: 1,
            publisher: [0; 32],
            surface: 7,
            events: input,
        };
        assert!(request.valid(), "the host accepts every event");
        assert!(page.take_input().is_empty());
    }

    #[test]
    fn a_release_lets_go_of_modifiers_in_reverse() {
        let page = RemotePage::new();
        let held = Held {
            ctrl: true,
            ..Held::default()
        };
        page.character("c", false, held);
        assert_eq!(
            page.take_input(),
            vec![
                InputEvent::Key {
                    keysym: 46,
                    pressed: false,
                    modifiers: 0
                },
                InputEvent::Key {
                    keysym: KEY_LEFTCTRL,
                    pressed: false,
                    modifiers: 0
                },
            ]
        );
        assert_eq!(evdev_char("?"), Some((53, true)));
        assert_eq!(evdev_char("0"), Some((11, false)));
        assert_eq!(evdev_char(")"), Some((11, true)));
        assert_eq!(evdev_char("ab"), None);
    }
}
