//! Test control's typing: which keys type a character on the seat's
//! keymap, and the typing that waits for a character committed as text.

use super::*;

/// How test control presses or releases the key for one character.
pub(super) enum CharKey {
    /// These key events, with the physical keys that type the character on
    /// the seat's keymap.
    Keys(Vec<WindowEvent>),
    /// No key of the seat's keymap types it and a window has the keyboard:
    /// it reaches the window's field as text, as an input method's would.
    Text,
}

/// The key events that type `ch` (press or release, as `pressed` says) the
/// way a person at this seat would: the key the seat's keymap puts it on,
/// with Shift or `AltGr` held as that keymap needs. A Wayland client reads
/// the keycode through that same keymap, so it types `ch` whatever the
/// layout. A character no key types goes to the focused window as text,
/// and to the shell's own fields (which read the event's text) as a key
/// without a position.
pub(super) fn type_char_key(
    ch: char,
    pressed: bool,
    state: &mut State,
    fb: &FrameBuilder,
) -> CharKey {
    if let Some(stroke) = state.seat_stroke(ch) {
        return CharKey::Keys(stroke_events(ch, &stroke, pressed));
    }
    if client_input_allowed(fb)
        && !crate::shell::input_policy::chrome_captures_input(fb)
        && state.client_input.keyboard_focus().is_some()
    {
        return CharKey::Text;
    }
    CharKey::Keys(vec![keyboard_input_event(
        &Key::Character(ch.to_string().into()),
        pressed,
    )])
}

/// The events of `stroke` typing `ch`: its modifiers down, then the key, on
/// a press; the key up, then its modifiers in reverse, on a release.
pub(super) fn stroke_events(
    ch: char,
    stroke: &crate::compositor::keymap_typing::Stroke,
    pressed: bool,
) -> Vec<WindowEvent> {
    use crate::compositor::keymap_typing::Level;
    use winit::platform::scancode::PhysicalKeyExtScancode as _;
    let at = |mut event: WindowEvent, code: u32| {
        if let WindowEvent::KeyboardInput { event: key, .. } = &mut event {
            key.physical_key = winit::keyboard::PhysicalKey::from_scancode(code);
        }
        event
    };
    let modifier = |modifier: &crate::compositor::keymap_typing::StrokeModifier| {
        let key = Key::Named(match modifier.level {
            Level::Shift => NamedKey::Shift,
            Level::Level3 => NamedKey::AltGraph,
        });
        at(keyboard_input_event(&key, pressed), modifier.key)
    };
    let key = at(
        keyboard_input_event(&Key::Character(ch.to_string().into()), pressed),
        stroke.key,
    );
    if pressed {
        stroke
            .modifiers
            .iter()
            .map(modifier)
            .chain(std::iter::once(key))
            .collect()
    } else {
        std::iter::once(key)
            .chain(stroke.modifiers.iter().rev().map(modifier))
            .collect()
    }
}

/// Test-control typing held behind a character committed as text: the
/// actions after it, who they act for, and the command's answer.
pub(super) struct PendingTyping {
    pub(super) held: std::collections::VecDeque<PlannedAction>,
    pub(super) participant: Option<crate::network::protocol::PeerId>,
    pub(super) response: test_control::Response,
}

/// Type what `typing` holds once the field has the text it waits for,
/// stopping again at the next character that goes as text. Returns the
/// command's answer once everything is typed; `None` while it still waits
/// (held on `conn` again).
pub(super) fn continue_typing(
    mut typing: PendingTyping,
    conn: &mut TestControlConn,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    session: &SessionManager,
    state: &mut State,
) -> Option<test_control::Response> {
    if session.is_locked() {
        // The lock screen took the keyboard in the meantime: the rest is not
        // typed into a window it no longer shows.
        return Some(test_control::Response::Err {
            code: "session_locked".into(),
            message: test_control::SESSION_LOCKED.into(),
        });
    }
    if state.text_commit_pending() {
        conn.pending_typing = Some(typing);
        return None;
    }
    state.client_input.set_participant(typing.participant);
    while let Some(action) = typing.held.pop_front() {
        let _ = execute_planned_action(&action, fb, ctx, Some(state));
        if std::mem::take(&mut ctx.typing_commit_requested) {
            state.client_input.set_participant(None);
            conn.pending_typing = Some(typing);
            return None;
        }
    }
    state.client_input.set_participant(None);
    Some(typing.response)
}
