//! Input-method client (the clean auto-show path).
//!
//! The on-screen keyboard becomes a Wayland `input-method-v2` client so it can
//! *react to text focus*: when the user taps into a text field, the compositor
//! activates this input method and tells us the field's content purpose; we show
//! the keyboard with the matching layout, and we hide it again when focus leaves.
//! Typed (and swiped) text is sent straight to the focused application via
//! `commit_string` + `commit` — no synthetic key events, no guessing the host's
//! keymap.
//!
//! ## Two halves, one seam
//!
//! * **Pure logic** — the wire-`content_purpose` → [`InputPurpose`] mapping and
//!   the show/hide/purpose [`ImState`] reducer. Both are unit-tested here; they
//!   have no Wayland dependency, so the auto-show behaviour is verified without a
//!   compositor.
//! * **Live Wayland** — [`InputMethodClient`] opens its own connection and event
//!   queue (independent of the one winit owns, exactly like
//!   [`crate::inject::VirtualKeyboardInjector`]), binds the input-method manager
//!   and a seat, and turns incoming protocol events into [`ImEvent`]s on a
//!   channel the app loop drains. This half needs a compositor that advertises
//!   `zwp_input_method_manager_v2`; it is exercised at compile/type level only in
//!   tests.
//!
//! ## Relationship to virtual-keyboard injection
//!
//! `commit_string` is the preferred way to deliver text — it is the protocol
//! designed for input methods and works regardless of the host's keymap. The
//! older [`crate::inject::VirtualKeyboardInjector`] stays as a fallback for
//! compositors that expose `virtual-keyboard-v1` but not `input-method-v2`, kept
//! behind the existing [`crate::inject::TextInjector`] seam.
//!
//! ## Manual verification
//!
//! Run the OSK with `--input-method` under the Tilo compositor (or any compositor
//! advertising `zwp_input_method_manager_v2`). Focus a native Tilo text field:
//! the keyboard should appear; focus a password or terminal field and its layout
//! changes. A third-party toolkit sending numeric/email content purposes selects
//! those dedicated layouts too. Blur the field (focus something non-text): it
//! hides. Typing on the keyboard inserts characters into the focused field.

use crate::layout::InputPurpose;

/// Wire values of `zwp_text_input_v3.content_purpose`. These are stable protocol
/// constants, so mapping over the raw `u32` keeps the translation free of any
/// Wayland type and lets it be unit-tested directly.
pub mod content_purpose {
    pub const NORMAL: u32 = 0;
    pub const ALPHA: u32 = 1;
    pub const DIGITS: u32 = 2;
    pub const NUMBER: u32 = 3;
    pub const PHONE: u32 = 4;
    pub const URL: u32 = 5;
    pub const EMAIL: u32 = 6;
    pub const NAME: u32 = 7;
    pub const PASSWORD: u32 = 8;
    pub const PIN: u32 = 9;
    pub const DATE: u32 = 10;
    pub const TIME: u32 = 11;
    pub const DATETIME: u32 = 12;
    pub const TERMINAL: u32 = 13;
}

/// Wire bits of `zwp_text_input_v3.content_hint`.
pub mod content_hint {
    pub const NONE: u32 = 0;
    pub const COMPLETION: u32 = 0x1;
    pub const SPELLCHECK: u32 = 0x2;
    pub const AUTO_CAPITALIZATION: u32 = 0x4;
    pub const LOWERCASE: u32 = 0x8;
    pub const UPPERCASE: u32 = 0x10;
    pub const TITLECASE: u32 = 0x20;
    pub const HIDDEN_TEXT: u32 = 0x40;
    pub const SENSITIVE_DATA: u32 = 0x80;
    pub const LATIN: u32 = 0x100;
    pub const MULTILINE: u32 = 0x200;
}

/// Whether text typed into a field with this purpose and these content hints
/// may be remembered, suggested or shown as preedit. Secrets — passwords, PINs,
/// and any field marking its text hidden or sensitive — never are: nothing
/// typed there is learned, predicted from, or echoed outside the field.
pub fn may_learn(purpose: InputPurpose, hints: u32) -> bool {
    !matches!(purpose, InputPurpose::Password | InputPurpose::Pin)
        && hints & (content_hint::HIDDEN_TEXT | content_hint::SENSITIVE_DATA) == 0
}

/// Map a Wayland `content_purpose` wire value to the OSK's own [`InputPurpose`].
///
/// `digits` is a numeric secret-ish field with no separators — closest to a PIN
/// pad. `datetime` has no dedicated layout, so it borrows the date pad. `name`,
/// `terminal`, `alpha`, and any unknown value fall back to free text, which is
/// the safe default (every character reachable).
pub fn purpose_from_wire(wire: u32) -> InputPurpose {
    use content_purpose as cp;
    match wire {
        cp::NUMBER => InputPurpose::Number,
        // `digits` is a numeric secret-ish field with no separators — the
        // same pad as an actual PIN.
        cp::DIGITS | cp::PIN => InputPurpose::Pin,
        cp::PHONE => InputPurpose::Telephone,
        cp::URL => InputPurpose::Url,
        cp::EMAIL => InputPurpose::Email,
        cp::PASSWORD => InputPurpose::Password,
        // `datetime` has no dedicated layout, so it borrows the date pad.
        cp::DATE | cp::DATETIME => InputPurpose::Date,
        cp::TIME => InputPurpose::Time,
        // normal / alpha / name / terminal / anything unrecognised → plain text.
        _ => InputPurpose::Text,
    }
}

/// A decoded, app-facing input-method event. The live Wayland half produces
/// these; the app loop reacts to them (show the window, swap the layout, hide).
///
/// The protocol's own `content_type` / `done` double-buffering is folded away
/// here: [`ImState::reduce`] holds a content type as *pending* until [`Done`]
/// applies it, so the app only ever sees a settled purpose.
///
/// [`Done`]: ImEvent::Done
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImEvent {
    /// A text input gained focus on this seat: the keyboard should appear.
    /// Resets any pending content type (the protocol says `activate` clears it).
    Activate,
    /// Text focus left: the keyboard should hide.
    Deactivate,
    /// The focused field's content hints and purpose (raw wire values).
    /// Pending until the next [`Done`](ImEvent::Done).
    ContentType { hint: u32, purpose: u32 },
    /// Apply all state buffered since the last `Done` (the protocol commit point).
    Done,
}

/// The settled show/hide + layout state derived from the [`ImEvent`] stream.
///
/// This is the pure reducer that drives auto-show. It mirrors the protocol's
/// double-buffering: `content_type` updates a *pending* purpose that only becomes
/// the active [`InputPurpose`] on `done`, and `activate` clears any pending
/// purpose (per the protocol's activate semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImState {
    /// Whether the keyboard should currently be on screen.
    visible: bool,
    /// The applied purpose (what the visible layout reflects).
    purpose: InputPurpose,
    /// The applied content hints ([`content_hint`] bits).
    hints: u32,
    /// A content type received but not yet applied by a `done`.
    pending: Option<(InputPurpose, u32)>,
}

impl Default for ImState {
    fn default() -> Self {
        Self {
            visible: false,
            purpose: InputPurpose::Text,
            hints: content_hint::NONE,
            pending: None,
        }
    }
}

/// What changed after [`ImState::reduce`], so the app can act minimally: show or
/// hide the window, and/or switch the layout. `None` fields mean "no change".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImTransition {
    /// `Some(true)` → became visible; `Some(false)` → became hidden.
    pub visibility_changed: Option<bool>,
    /// `Some(p)` → the active purpose changed to `p`.
    pub purpose_changed: Option<InputPurpose>,
    /// `Some(h)` → the active content hints changed to `h`.
    pub hints_changed: Option<u32>,
    /// A text field gained or lost focus, so any composition in progress
    /// belonged to the previous field.
    pub field_changed: bool,
}

impl ImState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the keyboard should be on screen right now.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// The currently applied input purpose.
    pub fn purpose(&self) -> InputPurpose {
        self.purpose
    }

    /// The currently applied content hints.
    pub fn hints(&self) -> u32 {
        self.hints
    }

    /// Fold one [`ImEvent`] into the state, returning what the app must change.
    ///
    /// * `Activate` → visible; clears pending content type. Resets purpose to the
    ///   default so a stale layout from a previous field never lingers before the
    ///   new field's `content_type` arrives.
    /// * `Deactivate` → hidden; pending content type dropped.
    /// * `ContentType` → stage the mapped purpose and hints (not applied yet).
    /// * `Done` → apply any staged purpose and hints.
    pub fn reduce(&mut self, event: ImEvent) -> ImTransition {
        let was_visible = self.visible;
        let prev_purpose = self.purpose;
        let prev_hints = self.hints;
        let field_changed = matches!(event, ImEvent::Activate | ImEvent::Deactivate);

        match event {
            ImEvent::Activate => {
                self.visible = true;
                // `activate` resets prior content_type state; until the field's
                // own content_type arrives, assume free text without hints.
                self.pending = None;
                self.purpose = InputPurpose::Text;
                self.hints = content_hint::NONE;
            }
            ImEvent::Deactivate => {
                self.visible = false;
                self.pending = None;
            }
            ImEvent::ContentType { hint, purpose } => {
                self.pending = Some((purpose_from_wire(purpose), hint));
            }
            ImEvent::Done => {
                if let Some((purpose, hints)) = self.pending.take() {
                    self.purpose = purpose;
                    self.hints = hints;
                }
            }
        }

        ImTransition {
            visibility_changed: (self.visible != was_visible).then_some(self.visible),
            purpose_changed: (self.purpose != prev_purpose).then_some(self.purpose),
            hints_changed: (self.hints != prev_hints).then_some(self.hints),
            field_changed,
        }
    }
}

/// Try the live `input-method-v2` backend. Returns `None` (so the caller keeps
/// the always-visible path / virtual-keyboard fallback) when there is no Wayland
/// display, no seat, or the compositor lacks `zwp_input_method_manager_v2`.
pub fn try_input_method() -> Option<wayland::InputMethodClient> {
    wayland::InputMethodClient::new()
}

pub use wayland::InputMethodClient;

/// Live Wayland backend, isolated so the protocol plumbing stays in one place.
mod wayland {
    use std::sync::mpsc::{Receiver, Sender, channel};

    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols_misc::zwp_input_method_v2::client::{
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        zwp_input_method_v2::{self, ZwpInputMethodV2},
    };

    use super::ImEvent;

    /// Input-method client holding its own connection + event queue, independent
    /// of the one winit owns.
    ///
    /// Events the compositor sends (activate / `content_type` / deactivate / done)
    /// are decoded into [`ImEvent`]s and pushed onto a channel the app loop
    /// drains via [`InputMethodClient::poll`]. Typed text goes the other way via
    /// [`InputMethodClient::commit_text`].
    pub struct InputMethodClient {
        conn: Connection,
        queue: wayland_client::EventQueue<State>,
        state: State,
        input_method: ZwpInputMethodV2,
        rx: Receiver<ImEvent>,
    }

    /// Dispatch state. Tracks the `done` serial the protocol requires `commit` to
    /// echo, and forwards decoded events to the channel.
    struct State {
        tx: Sender<ImEvent>,
        /// Number of `done` events received — the serial `commit` must carry.
        serial: u32,
        /// The compositor permits one input method per seat. A second client
        /// receives `unavailable`; treating that as a working backend would
        /// leave a permanently hidden, non-functional keyboard.
        unavailable: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_seat::WlSeat,
            _: wl_seat::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpInputMethodManagerV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpInputMethodManagerV2,
            _: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpInputMethodV2, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZwpInputMethodV2,
            event: zwp_input_method_v2::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            use zwp_input_method_v2::Event;
            let decoded = match event {
                Event::Activate => Some(ImEvent::Activate),
                Event::Deactivate => Some(ImEvent::Deactivate),
                Event::ContentType { hint, purpose } => {
                    // Both are WEnums; the wire values are what our mappings
                    // understand (and tolerate if unknown).
                    Some(ImEvent::ContentType {
                        hint: u32::from(hint),
                        purpose: u32::from(purpose),
                    })
                }
                Event::Done => {
                    // The serial `commit` must echo is the count of `done`s seen.
                    state.serial = state.serial.wrapping_add(1);
                    Some(ImEvent::Done)
                }
                Event::Unavailable => {
                    state.unavailable = true;
                    None
                }
                // Surrounding-text / change-cause carry no auto-show signal.
                _ => None,
            };
            if let Some(ev) = decoded {
                // A full receiver only happens if the app stopped draining; drop
                // rather than block the Wayland thread.
                let _ = state.tx.send(ev);
            }
        }
    }

    impl InputMethodClient {
        /// Connect, bind the manager + a seat, and create the input method.
        /// Returns `None` (caller degrades) on any missing piece. Never panics.
        pub fn new() -> Option<Self> {
            let conn = Connection::connect_to_env().ok()?;
            let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
            let qh = queue.handle();

            // The manager is the protocol that may be absent — its absence is the
            // expected "no input-method, stay always-visible" path.
            let manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ()).ok()?;
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=8, ()).ok()?;

            let (tx, rx) = channel();
            let input_method = manager.get_input_method(&seat, &qh, ());

            let mut state = State {
                tx,
                serial: 0,
                unavailable: false,
            };
            // Flush the get_input_method request.
            queue.roundtrip(&mut state).ok()?;
            if state.unavailable {
                input_method.destroy();
                let _ = queue.flush();
                return None;
            }

            Some(Self {
                conn,
                queue,
                state,
                input_method,
                rx,
            })
        }

        /// Pump the Wayland connection without blocking, then drain every decoded
        /// [`ImEvent`] that arrived. Call once per app-loop tick.
        pub fn poll(&mut self) -> Vec<ImEvent> {
            // Read any pending wire events and dispatch them into `State` (which
            // forwards to the channel). Non-blocking: returns immediately if the
            // socket has nothing.
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.state);
            let _ = self.queue.flush();
            self.rx.try_iter().collect()
        }

        /// Block until at least one event is available, dispatch it, and return
        /// the drained batch. Lets a dedicated thread sleep on the socket instead
        /// of busy-polling.
        pub fn blocking_dispatch(&mut self) -> Vec<ImEvent> {
            let _ = self.queue.blocking_dispatch(&mut self.state);
            let _ = self.queue.flush();
            self.rx.try_iter().collect()
        }

        /// Replace the composing word with `text` and make it final:
        /// `commit_string(text)` stages it and `commit(serial)` applies it,
        /// where `serial` is the number of `done` events seen so far (the
        /// protocol's required echo). Applying a commit also removes the
        /// preedit, so an empty `text` still ends the composition.
        pub fn commit_text(&mut self, text: &str) {
            self.input_method.commit_string(text.to_string());
            self.input_method.commit(self.state.serial);
            let _ = self.queue.flush();
        }

        /// Show `text` as the composing (preedit) word, cursor at its end.
        /// An empty `text` clears the preedit.
        pub fn set_preedit(&mut self, text: &str) {
            let end = i32::try_from(text.len()).unwrap_or(i32::MAX);
            self.input_method
                .set_preedit_string(text.to_string(), end, end);
            self.input_method.commit(self.state.serial);
            let _ = self.queue.flush();
        }

        /// Wait until the compositor has processed every request sent so far.
        /// Needed before a request on another connection that must land after
        /// them.
        pub fn sync(&mut self) {
            let _ = self.queue.roundtrip(&mut self.state);
            // Keep the connection field live alongside the queue it feeds.
            let _ = &self.conn;
        }

        /// The serial `commit` will echo on the next [`commit_text`] — the count
        /// of `done` events received. Exposed for tests / diagnostics.
        ///
        /// [`commit_text`]: InputMethodClient::commit_text
        pub fn serial(&self) -> u32 {
            self.state.serial
        }
    }

    impl Drop for InputMethodClient {
        fn drop(&mut self) {
            self.input_method.destroy();
            let _ = self.queue.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purpose_only(purpose: u32) -> ImEvent {
        ImEvent::ContentType {
            hint: content_hint::NONE,
            purpose,
        }
    }

    #[test]
    fn may_learn_refuses_every_secret_field() {
        use content_hint as h;
        let cases = [
            (InputPurpose::Text, h::NONE, true),
            (
                InputPurpose::Text,
                h::SPELLCHECK | h::AUTO_CAPITALIZATION,
                true,
            ),
            (InputPurpose::Email, h::NONE, true),
            (InputPurpose::Url, h::COMPLETION, true),
            (InputPurpose::Number, h::NONE, true),
            (InputPurpose::Password, h::NONE, false),
            (InputPurpose::Pin, h::NONE, false),
            (InputPurpose::Text, h::SENSITIVE_DATA, false),
            (InputPurpose::Text, h::HIDDEN_TEXT, false),
            (InputPurpose::Email, h::SENSITIVE_DATA | h::LOWERCASE, false),
        ];
        for (purpose, hints, want) in cases {
            assert_eq!(may_learn(purpose, hints), want, "{purpose:?} {hints:#x}");
        }
    }

    #[test]
    fn hints_apply_on_done_and_reset_on_activate() {
        let mut s = ImState::new();
        s.reduce(ImEvent::Activate);
        let t = s.reduce(ImEvent::ContentType {
            hint: content_hint::SENSITIVE_DATA,
            purpose: content_purpose::NORMAL,
        });
        assert_eq!(t.hints_changed, None);
        let t = s.reduce(ImEvent::Done);
        assert_eq!(t.hints_changed, Some(content_hint::SENSITIVE_DATA));
        assert_eq!(s.hints(), content_hint::SENSITIVE_DATA);
        let t = s.reduce(ImEvent::Activate);
        assert_eq!(t.hints_changed, Some(content_hint::NONE));
    }

    #[test]
    fn purpose_from_wire_maps_known_values_and_reducer_starts_hidden_text() {
        eprintln!("phase purpose_from_wire_maps_every_known_value");
        {
            use content_purpose as cp;
            assert_eq!(purpose_from_wire(cp::NORMAL), InputPurpose::Text);
            assert_eq!(purpose_from_wire(cp::ALPHA), InputPurpose::Text);
            assert_eq!(purpose_from_wire(cp::DIGITS), InputPurpose::Pin);
            assert_eq!(purpose_from_wire(cp::NUMBER), InputPurpose::Number);
            assert_eq!(purpose_from_wire(cp::PHONE), InputPurpose::Telephone);
            assert_eq!(purpose_from_wire(cp::URL), InputPurpose::Url);
            assert_eq!(purpose_from_wire(cp::EMAIL), InputPurpose::Email);
            assert_eq!(purpose_from_wire(cp::NAME), InputPurpose::Text);
            assert_eq!(purpose_from_wire(cp::PASSWORD), InputPurpose::Password);
            assert_eq!(purpose_from_wire(cp::PIN), InputPurpose::Pin);
            assert_eq!(purpose_from_wire(cp::DATE), InputPurpose::Date);
            assert_eq!(purpose_from_wire(cp::TIME), InputPurpose::Time);
            assert_eq!(purpose_from_wire(cp::DATETIME), InputPurpose::Date);
            assert_eq!(purpose_from_wire(cp::TERMINAL), InputPurpose::Text);
        }

        eprintln!("phase purpose_from_wire_unknown_falls_back_to_text");
        {
            // A value the protocol doesn't define (or a future addition) must not
            // panic and must degrade to the safe free-text layout.
            assert_eq!(purpose_from_wire(99), InputPurpose::Text);
            assert_eq!(purpose_from_wire(u32::MAX), InputPurpose::Text);
        }

        eprintln!("phase reducer_starts_hidden_and_text");
        {
            let s = ImState::new();
            assert!(!s.visible());
            assert_eq!(s.purpose(), InputPurpose::Text);
        }
    }

    #[test]
    fn reducer_reports_each_visibility_transition_once() {
        eprintln!("phase activate_shows_and_reports_the_transition");
        {
            let mut s = ImState::new();
            let t = s.reduce(ImEvent::Activate);
            assert!(s.visible());
            assert_eq!(t.visibility_changed, Some(true));
            // No content type yet → still the default text layout.
            assert_eq!(s.purpose(), InputPurpose::Text);
            assert_eq!(t.purpose_changed, None);
        }

        eprintln!("phase deactivate_hides_and_reports_the_transition");
        {
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            let t = s.reduce(ImEvent::Deactivate);
            assert!(!s.visible());
            assert_eq!(t.visibility_changed, Some(false));
        }

        eprintln!("phase repeated_activate_does_not_re_report_visibility");
        {
            // Defensive: a second activate while already visible shouldn't claim a
            // visibility change (the window is already shown).
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            let t = s.reduce(ImEvent::Activate);
            assert_eq!(t.visibility_changed, None);
            assert!(s.visible());
        }

        eprintln!("phase full_focus_cycle_number_field");
        {
            // The exact sequence a compositor sends when focusing a number field,
            // then blurring it.
            let mut s = ImState::new();

            let t = s.reduce(ImEvent::Activate);
            assert_eq!(t.visibility_changed, Some(true));

            s.reduce(purpose_only(content_purpose::NUMBER));
            let t = s.reduce(ImEvent::Done);
            assert!(s.visible());
            assert_eq!(s.purpose(), InputPurpose::Number);
            assert_eq!(t.purpose_changed, Some(InputPurpose::Number));

            let t = s.reduce(ImEvent::Deactivate);
            assert!(!s.visible());
            assert_eq!(t.visibility_changed, Some(false));
        }
    }

    #[test]
    fn reducer_applies_content_type_purpose_only_on_done() {
        eprintln!("phase content_type_is_pending_until_done");
        {
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            // content_type alone does NOT change the applied purpose...
            let t = s.reduce(purpose_only(content_purpose::NUMBER));
            assert_eq!(s.purpose(), InputPurpose::Text);
            assert_eq!(t.purpose_changed, None);
            // ...the following `done` applies it.
            let t = s.reduce(ImEvent::Done);
            assert_eq!(s.purpose(), InputPurpose::Number);
            assert_eq!(t.purpose_changed, Some(InputPurpose::Number));
        }

        eprintln!("phase done_without_pending_purpose_is_a_noop");
        {
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            let t = s.reduce(ImEvent::Done);
            assert_eq!(t.purpose_changed, None);
            assert_eq!(s.purpose(), InputPurpose::Text);
        }

        eprintln!("phase activate_clears_a_stale_purpose_between_fields");
        {
            // Focus an email field, then move to a plain text field. Re-activation
            // must reset to text so the email layout doesn't linger before the new
            // field's content_type (if any) arrives.
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            s.reduce(purpose_only(content_purpose::EMAIL));
            s.reduce(ImEvent::Done);
            assert_eq!(s.purpose(), InputPurpose::Email);

            // New field gains focus.
            let t = s.reduce(ImEvent::Activate);
            assert_eq!(s.purpose(), InputPurpose::Text);
            assert_eq!(t.purpose_changed, Some(InputPurpose::Text));
        }

        eprintln!("phase switching_purpose_while_visible_reports_only_purpose");
        {
            let mut s = ImState::new();
            s.reduce(ImEvent::Activate);
            s.reduce(purpose_only(content_purpose::NUMBER));
            s.reduce(ImEvent::Done);

            // Field re-reports a different purpose without a re-activate.
            s.reduce(purpose_only(content_purpose::EMAIL));
            let t = s.reduce(ImEvent::Done);
            assert_eq!(t.visibility_changed, None);
            assert_eq!(t.purpose_changed, Some(InputPurpose::Email));
        }
    }
}
