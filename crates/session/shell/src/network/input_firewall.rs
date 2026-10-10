//! The firewall every remote-input event crosses before it reaches the seat.
//!
//! Order: kill switch, session lock, physical priority, region, z-order,
//! scrubber, rate limit. A check that cannot observe its answer denies
//! rather than passes. The read-only flag and surface ownership are
//! checked earlier, by the share registry in
//! [`crate::compositor::state::State::apply_remote_input`].
//!
//! Peers send surface-local pixels; the region check is what makes
//! translating them into global coordinates safe. The scrubber reuses
//! [`crate::shell::input_policy::shell_owns_chord`] rather than its own
//! chord list, so local and remote denials cannot drift; `Ctrl+Alt+F1..F12`
//! and `Ctrl+Alt+Delete` never reach the shell — the kernel/session takes
//! them first.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};

use crate::network::protocol::{PeerId, SurfaceId};
use crate::network::streaming::InputEvent;
use crate::shell::input_policy::{ChordMods, chord_key_for_evdev, shell_owns_chord};

/// How long local input keeps the remote side out — the host reaching for their own input is an unambiguous takeover.
pub const PHYSICAL_PRIORITY_WINDOW: Duration = Duration::from_millis(500);

/// Sustained ceiling on events a single peer may deliver, per second.
pub const MAX_EVENTS_PER_SECOND: usize = 120;

/// The window [`MAX_EVENTS_PER_SECOND`] is measured over.
pub const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Clicks inside [`CLICK_BURST_WINDOW`] still tolerated before the next one is a burst.
pub const CLICK_BURST_LIMIT: usize = 10;

/// The window a click burst is measured over.
pub const CLICK_BURST_WINDOW: Duration = Duration::from_millis(100);

/// How long a peer stays blocked after a click burst — long enough to break an automated storm, short enough not to lock out a trackpad fumble.
pub const CLICK_BURST_BLOCK: Duration = Duration::from_secs(1);

/// Most text one remote input-method commit may carry: a dictated paragraph
/// fits, a pasted document does not (paste goes through the clipboard).
pub const MAX_REMOTE_TEXT_BYTES: usize = 4096;

/// evdev keycodes the scrubber names directly.
mod keycode {
    pub const DELETE: u32 = 111;
    #[cfg(test)]
    pub const TAB: u32 = 15;
    #[cfg(test)]
    pub const PRINT_SCREEN: u32 = 99;
    pub const LEFT_META: u32 = 125;
    pub const RIGHT_META: u32 = 126;
    pub const LEFT_CTRL: u32 = 29;
    pub const RIGHT_CTRL: u32 = 97;
    pub const LEFT_SHIFT: u32 = 42;
    pub const RIGHT_SHIFT: u32 = 54;
    pub const LEFT_ALT: u32 = 56;
    pub const RIGHT_ALT: u32 = 100;
}

/// Why an event did not reach the seat — an enum, not a bool, so callers can log which rule fired and tests can assert the specific denial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    /// The host hit the kill switch.
    KillSwitch,
    /// The session is locked; nobody drives this seat remotely.
    SessionLocked,
    /// The host used their own keyboard or mouse within [`PHYSICAL_PRIORITY_WINDOW`].
    PhysicalPriority,
    /// The share exists but the owner marked it read-only.
    ReadOnly,
    /// No share record for this `(peer, surface)` pair at all.
    NotShared,
    /// The compositor has not published a screen, so region and z-order can't be answered.
    NoView,
    /// The surface is shared but is not currently on screen.
    NotOnScreen,
    /// The coordinate falls outside the shared surface's content rect.
    OutsideRegion,
    /// Something else is on top at that point — the clickjacking refusal.
    Occluded,
    /// A button arrived before any motion established the peer's pointer position.
    NoPointerPosition,
    /// A system chord the shell (or the kernel) reserves.
    SystemChord,
    /// The shared surface does not hold the seat's keyboard focus.
    FocusMuted,
    /// The event named a surface other than the one the share covers.
    SurfaceMismatch,
    /// More than [`MAX_EVENTS_PER_SECOND`] in the last second.
    RateLimited,
    /// More than [`CLICK_BURST_LIMIT`] clicks inside [`CLICK_BURST_WINDOW`], or still inside the resulting block.
    ClickBurst,
    /// A local client holds the pointer locked or confined; a peer's
    /// pointer would move the shared seat and break (or steer) it.
    PointerHeld,
    /// Committed text that is empty or longer than [`MAX_REMOTE_TEXT_BYTES`].
    TextSize,
}

impl Denial {
    /// A short stable token for logs and metrics.
    pub fn reason(self) -> &'static str {
        match self {
            Denial::KillSwitch => "kill-switch",
            Denial::SessionLocked => "session-locked",
            Denial::PhysicalPriority => "physical-priority",
            Denial::ReadOnly => "read-only",
            Denial::NotShared => "not-shared",
            Denial::NoView => "no-view",
            Denial::NotOnScreen => "not-on-screen",
            Denial::OutsideRegion => "outside-region",
            Denial::Occluded => "occluded",
            Denial::NoPointerPosition => "no-pointer-position",
            Denial::SystemChord => "system-chord",
            Denial::FocusMuted => "focus-muted",
            Denial::SurfaceMismatch => "surface-mismatch",
            Denial::RateLimited => "rate-limited",
            Denial::ClickBurst => "click-burst",
            Denial::PointerHeld => "pointer-held",
            Denial::TextSize => "text-size",
        }
    }
}

/// What the compositor last told the firewall about the screen; a default
/// view has observed nothing, so every check needing it refuses deliberately.
#[derive(Debug, Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent frame-observation flags, not a state machine; \
              splitting them into an enum would not make any check here clearer"
)]
pub struct ShellView {
    /// Foreign tiles front-to-back, as [`crate::shell::input_policy::foreign_tiles`] enumerates them.
    tiles: Vec<(SurfaceId, Rect)>,
    /// Compositor grip regions over these surfaces; never remote client input.
    chrome_regions: Vec<(SurfaceId, Rect)>,
    /// The surface holding keyboard focus, or `None` when shell chrome owns it.
    keyboard_focus: Option<SurfaceId>,
    /// Shell chrome (overlay, palette, permission prompt) is drawn over the tiles and taking input.
    chrome_captures: bool,
    /// An overlay is open, so the shell grabs `Escape`.
    overlay_open: bool,
    /// The session lock is up; nothing on screen belongs to anyone until the host authenticates.
    session_locked: bool,
    /// Set by every real publish, distinguishing "nothing on screen" from "nobody looked".
    observed: bool,
    /// A local client has the pointer locked or confined.
    pointer_held: bool,
}

impl ShellView {
    /// Build a view from what the compositor sees this frame.
    pub fn new(
        tiles: Vec<(SurfaceId, Rect)>,
        keyboard_focus: Option<SurfaceId>,
        chrome_captures: bool,
        overlay_open: bool,
    ) -> Self {
        Self {
            tiles,
            chrome_regions: Vec::new(),
            keyboard_focus,
            chrome_captures,
            overlay_open,
            session_locked: false,
            observed: true,
            pointer_held: false,
        }
    }

    /// Preserve surface geometry while excluding compositor-owned pointer grips.
    #[must_use]
    pub fn chrome_regions(mut self, regions: Vec<(SurfaceId, Rect)>) -> Self {
        self.chrome_regions = regions;
        self
    }

    /// Mark the session locked; the lock surface isn't part of the tiled tree, so this stops a remote peer driving what's underneath it directly.
    #[must_use]
    pub fn locked(mut self, locked: bool) -> Self {
        self.session_locked = locked;
        self
    }

    /// Mark the local pointer as held by a client's lock or confinement.
    #[must_use]
    pub fn pointer_held(mut self, held: bool) -> Self {
        self.pointer_held = held;
        self
    }

    /// Whether the session lock is up.
    pub fn session_locked(&self) -> bool {
        self.session_locked
    }

    /// Whether the compositor has published anything at all.
    pub fn observed(&self) -> bool {
        self.observed
    }

    /// Whether trusted compositor chrome currently occludes client content
    /// and owns the seat. Remote input must be denied while this is true.
    pub fn chrome_captures_input(&self) -> bool {
        self.chrome_captures
    }

    /// The on-screen rect of `surface`, if it is currently on screen.
    pub fn rect(&self, surface: SurfaceId) -> Option<Rect> {
        self.tiles
            .iter()
            .find(|(id, _)| *id == surface)
            .map(|(_, r)| *r)
    }

    /// The surface actually on top at `point` (global coordinates), from the compositor's own front-to-back enumeration.
    pub fn topmost_at(&self, point: (f64, f64)) -> Option<SurfaceId> {
        let p = Point::new(point.0, point.1);
        self.tiles
            .iter()
            .find(|(id, rect)| {
                rect.contains(p)
                    && !self
                        .chrome_regions
                        .iter()
                        .any(|(owner, grip)| owner == id && grip.contains(p))
            })
            .map(|(id, _)| *id)
    }
}

/// Per-peer budget and modifier bookkeeping.
#[derive(Debug, Default)]
struct PeerState {
    /// Arrival times inside [`RATE_WINDOW`].
    events: VecDeque<Instant>,
    /// Button-press times inside [`CLICK_BURST_WINDOW`].
    clicks: VecDeque<Instant>,
    /// Set when a click burst trips; every event is refused until it passes.
    blocked_until: Option<Instant>,
    /// Modifier keys this peer holds, derived from its key stream — the wire's
    /// `modifiers` field is peer-written, so it only adds denials, never removes one.
    held: HeldMods,
    /// Where this peer's pointer was last accepted, and on which surface.
    cursor: Option<(SurfaceId, (f64, f64))>,
}

/// Modifier keys held, tracked per side so a two-Ctrl release order can't leave a phantom modifier latched.
#[derive(Debug, Default, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool per physical modifier key, matched directly against keycodes below; \
              an enum set would not simplify the per-key toggle logic"
)]
struct HeldMods {
    ctrl_left: bool,
    ctrl_right: bool,
    alt_left: bool,
    alt_right: bool,
    shift_left: bool,
    shift_right: bool,
    logo_left: bool,
    logo_right: bool,
}

impl HeldMods {
    /// Fold a key transition into the held set; returns `true` when the keycode was a modifier.
    fn note(&mut self, keycode: u32, pressed: bool) -> bool {
        let slot = match keycode {
            keycode::LEFT_CTRL => &mut self.ctrl_left,
            keycode::RIGHT_CTRL => &mut self.ctrl_right,
            keycode::LEFT_ALT => &mut self.alt_left,
            keycode::RIGHT_ALT => &mut self.alt_right,
            keycode::LEFT_SHIFT => &mut self.shift_left,
            keycode::RIGHT_SHIFT => &mut self.shift_right,
            keycode::LEFT_META => &mut self.logo_left,
            keycode::RIGHT_META => &mut self.logo_right,
            _ => return false,
        };
        *slot = pressed;
        true
    }

    fn mods(self) -> ChordMods {
        ChordMods {
            shift: self.shift_left || self.shift_right,
            ctrl: self.ctrl_left || self.ctrl_right,
            alt: self.alt_left || self.alt_right,
            logo: self.logo_left || self.logo_right,
        }
    }
}

/// The remote-input firewall: one per compositor, living on
/// [`crate::compositor::state::State`]; holds the screen view, host-override latches, and per-peer budgets.
#[derive(Debug, Default)]
pub struct RemoteInputFirewall {
    view: ShellView,
    last_local_input: Option<Instant>,
    kill_switch: bool,
    peers: HashMap<PeerId, PeerState>,
}

impl RemoteInputFirewall {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish what is on screen. Called once per tick after keyboard focus is
    /// reconciled, so a remote event is judged against what the local user sees.
    pub fn publish_view(&mut self, view: ShellView) {
        self.view = view;
    }

    /// Borrow the current view. Diagnostics and tests.
    pub fn view(&self) -> &ShellView {
        &self.view
    }

    /// Reconcile the seat's focus after an accepted headless-host focus
    /// transition, without replacing any visibility or host-override flags.
    pub(crate) fn set_keyboard_focus(&mut self, focus: Option<SurfaceId>) {
        self.view.keyboard_focus = focus;
    }

    /// Re-resolve a participant's last accepted surface-local pointer
    /// position against the current compositor view. Button events carry no
    /// coordinates, so the shared Wayland pointer must be restored here when
    /// another participant moved it in the meantime.
    pub fn peer_cursor_global(&self, peer: PeerId, surface: SurfaceId) -> Option<(f64, f64)> {
        let (cursor_surface, local) = self.peers.get(&peer)?.cursor?;
        (cursor_surface == surface)
            .then(|| self.locate(surface, local).ok())
            .flatten()
    }

    /// Record that the host used their own keyboard or pointer.
    pub fn note_local_input(&mut self, now: Instant) {
        self.last_local_input = Some(now);
    }

    /// Engage the kill switch. Latches until [`Self::clear_kill_switch`].
    pub fn engage_kill_switch(&mut self) {
        self.kill_switch = true;
        // Budgets and latched modifiers belong to a session that is over.
        self.peers.clear();
    }

    /// Whether the kill switch is latched.
    pub fn kill_switch_engaged(&self) -> bool {
        self.kill_switch
    }

    /// Release the kill switch.
    pub fn clear_kill_switch(&mut self) {
        self.kill_switch = false;
    }

    /// Forget a peer's budget and held modifiers — call when it disconnects.
    pub fn forget_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
    }

    /// The host overrides, checked before anything about the share — both are absolute and don't need to look at the event.
    pub fn host_override(&self, now: Instant) -> Option<Denial> {
        if self.kill_switch {
            return Some(Denial::KillSwitch);
        }
        // The lock surface isn't part of the tiled tree, so the tile list alone
        // would show a client as topmost underneath it — refused here instead.
        if self.view.session_locked {
            return Some(Denial::SessionLocked);
        }
        if let Some(last) = self.last_local_input
            && now.saturating_duration_since(last) < PHYSICAL_PRIORITY_WINDOW
        {
            return Some(Denial::PhysicalPriority);
        }
        None
    }

    /// Run the region, z-order, scrubber and rate-limit checks; share existence
    /// and read-only are already checked by the caller. Returns the translated
    /// event to inject, or the rule that refused it.
    ///
    /// # Errors
    ///
    /// Returns the [`Denial`] naming the first check the event fails: no
    /// screen has been observed yet, the peer's pointer has no last-known
    /// position, the event targets a point/surface outside what the peer may
    /// currently reach, a system chord or focus-mute rule applies, or the
    /// peer has exceeded its event-rate or click-burst budget.
    pub fn review(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        event: &InputEvent,
        now: Instant,
    ) -> Result<InputEvent, Denial> {
        // Every offered event is counted, whichever check refuses it, so a flood
        // of denied events can't buy a fresh budget for one that would pass.
        self.note_offered(peer, now);

        if !self.view.observed {
            return Err(Denial::NoView);
        }

        let outcome = self.check(peer, surface, event, now);
        if outcome.is_ok() {
            match event {
                InputEvent::PointerMotion { x, y } => {
                    // Keep the coordinate in the same surface-local space it
                    // arrived in. A later coordinate-less button is checked
                    // against the current tile placement again.
                    self.peer_mut(peer).cursor = Some((surface, (*x, *y)));
                }
                InputEvent::PointerLeave { .. } => {
                    // A leave invalidates this peer's coordinate. Another
                    // participant may still keep the shared wl_pointer over a
                    // client, but this peer must move again before clicking.
                    self.peer_mut(peer).cursor = None;
                }
                InputEvent::PointerButton { .. }
                | InputEvent::PointerEnter { .. }
                | InputEvent::KeyboardFocus { .. }
                | InputEvent::Key { .. }
                | InputEvent::PointerAxis { .. }
                | InputEvent::ClipboardText { .. }
                | InputEvent::Close
                | InputEvent::Text { .. } => {}
            }
        }
        outcome
    }

    /// The checks proper, split out so [`Self::review`] owns the bookkeeping that always runs.
    fn check(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        event: &InputEvent,
        now: Instant,
    ) -> Result<InputEvent, Denial> {
        if self.view.pointer_held
            && matches!(
                event,
                InputEvent::PointerMotion { .. }
                    | InputEvent::PointerButton { .. }
                    | InputEvent::PointerEnter { .. }
            )
        {
            return Err(Denial::PointerHeld);
        }
        match event {
            InputEvent::PointerMotion { x, y } => {
                let global = self.locate(surface, (*x, *y))?;
                self.spend_budget(peer, now, false)?;
                Ok(InputEvent::PointerMotion {
                    x: global.0,
                    y: global.1,
                })
            }
            InputEvent::PointerButton { button, pressed } => {
                // A button carries no coordinate, so it's checked against the last
                // accepted motion; no motion means no point, and refuses.
                let Some((cursor_surface, local)) = self.peer_mut(peer).cursor else {
                    return Err(Denial::NoPointerPosition);
                };
                if cursor_surface != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                self.locate(surface, local)?;
                if *pressed {
                    self.spend_budget(peer, now, true)?;
                }
                Ok(InputEvent::PointerButton {
                    button: *button,
                    pressed: *pressed,
                })
            }
            InputEvent::PointerEnter { surface: target } => {
                // The inner surface is what the focus grab applies to — unchecked, it points the host's keyboard at a surface the peer was never shared.
                if *target != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                if self.view.rect(surface).is_none() {
                    return Err(Denial::NotOnScreen);
                }
                self.spend_budget(peer, now, false)?;
                Ok(event.clone())
            }
            InputEvent::PointerLeave { surface: target } => {
                if *target != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                Ok(event.clone())
            }
            InputEvent::KeyboardFocus {
                surface: target,
                focused,
            } => {
                if *target != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                if *focused && self.view.rect(surface).is_none() {
                    return Err(Denial::NotOnScreen);
                }
                if *focused {
                    self.spend_budget(peer, now, false)?;
                }
                Ok(event.clone())
            }
            InputEvent::Key {
                keysym,
                pressed,
                modifiers,
            } => {
                let verdict = self.scrub_key(peer, surface, *keysym, *pressed, *modifiers);
                verdict?;
                if *pressed {
                    self.spend_budget(peer, now, false)?;
                }
                Ok(event.clone())
            }
            InputEvent::PointerAxis { .. } => {
                // Scrolls where this peer's pointer last was, which must still
                // be over the window.
                let Some((cursor_surface, local)) = self.peer_mut(peer).cursor else {
                    return Err(Denial::NoPointerPosition);
                };
                if cursor_surface != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                self.locate(surface, local)?;
                self.spend_budget(peer, now, false)?;
                Ok(event.clone())
            }
            InputEvent::ClipboardText { .. } | InputEvent::Close => {
                // Each changes the session (its clipboard, the window's
                // life) as a press would.
                self.spend_budget(peer, now, true)?;
                Ok(event.clone())
            }
            InputEvent::Text { text } => {
                self.check_text(peer, surface, text, now)?;
                Ok(event.clone())
            }
        }
    }

    /// Committed text is typing, so the keyboard's focus guard applies: it
    /// lands only in the shared window, and only while that has focus.
    fn check_text(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        text: &str,
        now: Instant,
    ) -> Result<(), Denial> {
        if self.view.chrome_captures || self.view.keyboard_focus != Some(surface) {
            return Err(Denial::FocusMuted);
        }
        if text.is_empty() || text.len() > MAX_REMOTE_TEXT_BYTES {
            return Err(Denial::TextSize);
        }
        self.spend_budget(peer, now, true)
    }

    /// Region + z-order for one surface-local coordinate; returns the global point to inject.
    fn locate(&self, surface: SurfaceId, local: (f64, f64)) -> Result<(f64, f64), Denial> {
        let Some(rect) = self.view.rect(surface) else {
            return Err(Denial::NotOnScreen);
        };
        // Region: must fall inside the content rect shown; half-open on the far edge so a boundary point belongs to one tile.
        if !(local.0 >= 0.0 && local.1 >= 0.0 && local.0 < rect.width() && local.1 < rect.height())
        {
            return Err(Denial::OutsideRegion);
        }
        let global = (rect.x0 + local.0, rect.y0 + local.1);
        // Z-order: chrome drawn over the tiles takes the whole screen, so nothing beneath it is topmost anywhere.
        if self.view.chrome_captures {
            return Err(Denial::Occluded);
        }
        // Otherwise ask the live tile order — trusting the share record instead is
        // the clickjacking hole: a shared surface can sit underneath something the host put in front of it.
        match self.view.topmost_at(global) {
            Some(top) if top == surface => Ok(global),
            _ => Err(Denial::Occluded),
        }
    }

    /// The keyboard scrubber: system chords, plus the focus guard.
    fn scrub_key(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        keycode: u32,
        pressed: bool,
        declared_modifiers: u32,
    ) -> Result<(), Denial> {
        let overlay_open = self.view.overlay_open;
        let focus = self.view.keyboard_focus;
        let chrome_captures = self.view.chrome_captures;

        let state = self.peer_mut(peer);
        // Track the peer's own modifier stream first, so a modifier this event presses counts towards its own chord.
        let is_modifier = state.held.note(keycode, pressed);
        let mods = state
            .held
            .mods()
            .union(ChordMods::from_xkb_bits(declared_modifiers));

        // Focus guard: a keystroke injects only while the shared surface holds
        // keyboard focus, checked before the chord table so a peer can't type into a private window.
        if chrome_captures || focus != Some(surface) {
            return Err(Denial::FocusMuted);
        }

        // Super is refused outright: it opens the shell's launcher alone and is half of every compositor chord.
        if matches!(keycode, keycode::LEFT_META | keycode::RIGHT_META) {
            return Err(Denial::SystemChord);
        }

        // A bare modifier transition isn't a chord; it must still reach the seat or the client's xkb state drifts from the compositor's.
        if is_modifier {
            return Ok(());
        }

        let chord = chord_key_for_evdev(keycode);

        // Remote input intentionally does not invoke the local user's
        // configurable shortcuts: a sharing peer may type into the selected
        // client, but cannot operate the desktop or race a Settings rebind.
        // It still shares the immutable-system policy with local input (F1,
        // Print Screen, launcher/lock chords), so remote input cannot bypass
        // those compositor safety boundaries.
        if shell_owns_chord(chord, mods, overlay_open) {
            return Err(Denial::SystemChord);
        }

        // Chords the shell never classifies because the kernel/session takes them first: VT switch and interrupt.
        if mods.ctrl && mods.alt {
            if matches!(chord, crate::shell::input_policy::ChordKey::Function(_)) {
                return Err(Denial::SystemChord);
            }
            if keycode == keycode::DELETE {
                return Err(Denial::SystemChord);
            }
        }
        Ok(())
    }

    /// Record that a peer offered an event, for the sustained-rate window.
    fn note_offered(&mut self, peer: PeerId, now: Instant) {
        let state = self.peers.entry(peer).or_default();
        prune(&mut state.events, now, RATE_WINDOW);
        state.events.push_back(now);
    }

    /// Charge an event against the peer's budgets.
    fn spend_budget(&mut self, peer: PeerId, now: Instant, is_click: bool) -> Result<(), Denial> {
        let state = self.peers.entry(peer).or_default();

        if let Some(until) = state.blocked_until {
            if now < until {
                return Err(Denial::ClickBurst);
            }
            state.blocked_until = None;
        }

        // note_offered already pruned and pushed this event; more than the ceiling means this one is over.
        if state.events.len() > MAX_EVENTS_PER_SECOND {
            return Err(Denial::RateLimited);
        }

        if is_click {
            prune(&mut state.clicks, now, CLICK_BURST_WINDOW);
            state.clicks.push_back(now);
            if state.clicks.len() > CLICK_BURST_LIMIT {
                state.blocked_until = Some(now.checked_add(CLICK_BURST_BLOCK).unwrap_or(now));
                state.clicks.clear();
                return Err(Denial::ClickBurst);
            }
        }

        Ok(())
    }

    fn peer_mut(&mut self, peer: PeerId) -> &mut PeerState {
        self.peers.entry(peer).or_default()
    }
}

/// Drop timestamps older than `window`.
fn prune(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(front) = times.front() {
        if now.saturating_duration_since(*front) >= window {
            times.pop_front();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: PeerId = 9;
    const SHARED: SurfaceId = 100;
    const PRIVATE: SurfaceId = 200;

    /// One shared tile at (100, 50), 400x300, focused, nothing over it.
    fn view_with_shared_tile() -> ShellView {
        ShellView::new(
            vec![(SHARED, Rect::new(100.0, 50.0, 500.0, 350.0))],
            Some(SHARED),
            false,
            false,
        )
    }

    fn firewall() -> RemoteInputFirewall {
        let mut fw = RemoteInputFirewall::new();
        fw.publish_view(view_with_shared_tile());
        fw
    }

    #[test]
    fn pointer_motion_is_admitted_only_inside_visible_shared_regions() {
        // Phase `a_firewall_that_has_seen_no_layout_refuses_everything`.
        {
            let mut fw = RemoteInputFirewall::new();
            let verdict = fw.review(
                PEER,
                SHARED,
                &InputEvent::PointerMotion { x: 1.0, y: 1.0 },
                Instant::now(),
            );
            assert_eq!(verdict, Err(Denial::NoView));
        }
        // Phase `motion_inside_the_region_is_translated_into_output_space`.
        {
            let mut fw = firewall();
            let out = fw
                .review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
                    Instant::now(),
                )
                .expect("inside the tile");
            assert_eq!(out, InputEvent::PointerMotion { x: 110.0, y: 70.0 });
        }
        // Phase `motion_outside_the_region_is_refused`.
        {
            let mut fw = firewall();
            for (x, y) in [(-1.0, 10.0), (10.0, -1.0), (400.0, 10.0), (10.0, 300.0)] {
                assert_eq!(
                    fw.review(
                        PEER,
                        SHARED,
                        &InputEvent::PointerMotion { x, y },
                        Instant::now()
                    ),
                    Err(Denial::OutsideRegion),
                    "({x}, {y}) is outside a 400x300 tile and must not inject",
                );
            }
        }
        // Phase `motion_is_refused_where_another_surface_is_on_top`.
        {
            let mut fw = RemoteInputFirewall::new();
            // The private surface is enumerated first, so it is in front.
            fw.publish_view(ShellView::new(
                vec![
                    (PRIVATE, Rect::new(100.0, 50.0, 500.0, 350.0)),
                    (SHARED, Rect::new(100.0, 50.0, 500.0, 350.0)),
                ],
                Some(SHARED),
                false,
                false,
            ));
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
                    Instant::now()
                ),
                Err(Denial::Occluded),
            );
        }
        // Phase `chrome_drawn_over_the_tiles_occludes_every_shared_surface`.
        {
            let mut fw = RemoteInputFirewall::new();
            fw.publish_view(ShellView::new(
                vec![(SHARED, Rect::new(100.0, 50.0, 500.0, 350.0))],
                Some(SHARED),
                true,
                false,
            ));
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
                    Instant::now()
                ),
                Err(Denial::Occluded),
            );
        }
    }

    /// Text an input method committed is typing: it needs the shared
    /// window to hold the keyboard, and it is bounded like one commit.
    #[test]
    fn remote_text_lands_only_in_the_focused_shared_window() {
        let now = Instant::now();
        let text = InputEvent::Text {
            text: "grüße".into(),
        };
        assert_eq!(
            firewall().review(PEER, SHARED, &text, now),
            Ok(text.clone())
        );

        let mut unfocused = RemoteInputFirewall::new();
        unfocused.publish_view(ShellView::new(
            vec![(SHARED, Rect::new(100.0, 50.0, 500.0, 350.0))],
            Some(PRIVATE),
            false,
            false,
        ));
        assert_eq!(
            unfocused.review(PEER, SHARED, &text, now),
            Err(Denial::FocusMuted)
        );
        for size in [0, MAX_REMOTE_TEXT_BYTES + 1] {
            assert_eq!(
                firewall().review(
                    PEER,
                    SHARED,
                    &InputEvent::Text {
                        text: "a".repeat(size),
                    },
                    now,
                ),
                Err(Denial::TextSize)
            );
        }
    }

    /// The wheel scrolls where the peer's pointer is, so it needs one over
    /// the window, like a button; a paste and a close need none.
    #[test]
    fn the_wheel_needs_a_coordinate_and_a_paste_or_close_does_not() {
        let mut fw = firewall();
        let now = Instant::now();
        let scroll = InputEvent::PointerAxis { dx: 0.0, dy: 30.0 };
        assert_eq!(
            fw.review(PEER, SHARED, &scroll, now),
            Err(Denial::NoPointerPosition)
        );
        fw.review(
            PEER,
            SHARED,
            &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
            now,
        )
        .expect("accepted motion");
        assert_eq!(fw.review(PEER, SHARED, &scroll, now), Ok(scroll.clone()));
        assert_eq!(
            fw.review(PEER, PRIVATE, &scroll, now),
            Err(Denial::SurfaceMismatch)
        );
        let paste = InputEvent::ClipboardText {
            text: "hello".into(),
        };
        assert_eq!(fw.review(PEER, SHARED, &paste, now), Ok(paste.clone()));
        assert_eq!(
            fw.review(PEER, SHARED, &InputEvent::Close, now),
            Ok(InputEvent::Close)
        );
    }

    #[test]
    fn buttons_need_a_current_valid_coordinate() {
        // Phase `a_button_without_a_prior_motion_has_no_coordinate_and_is_refused`.
        {
            let mut fw = firewall();
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerButton {
                        button: 272,
                        pressed: true
                    },
                    Instant::now()
                ),
                Err(Denial::NoPointerPosition),
            );
        }
        // Phase `pointer_leave_invalidates_the_peers_click_coordinate`.
        {
            let mut fw = firewall();
            let now = Instant::now();
            fw.review(
                PEER,
                SHARED,
                &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
                now,
            )
            .expect("accepted motion");
            fw.review(
                PEER,
                SHARED,
                &InputEvent::PointerLeave { surface: SHARED },
                now + Duration::from_millis(1),
            )
            .expect("accepted leave");

            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerButton {
                        button: 272,
                        pressed: true,
                    },
                    now + Duration::from_millis(2),
                ),
                Err(Denial::NoPointerPosition)
            );
        }
        // Phase `button_rechecks_surface_local_position_after_nonzero_origin_motion`.
        {
            let mut fw = firewall();
            let now = Instant::now();
            let motion = fw
                .review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 20.0 },
                    now,
                )
                .expect("motion inside offset tile");
            assert_eq!(motion, InputEvent::PointerMotion { x: 110.0, y: 70.0 });
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerButton {
                        button: 272,
                        pressed: true,
                    },
                    now + Duration::from_millis(1),
                ),
                Ok(InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                }),
                "button must revalidate the original (10,20), not add the tile origin twice",
            );
        }
    }

    #[test]
    fn keys_reach_the_seat_only_while_the_shared_surface_has_focus() {
        // Phase `a_focus_grab_naming_a_surface_the_share_does_not_cover_is_refused`.
        {
            let mut fw = firewall();
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerEnter { surface: PRIVATE },
                    Instant::now()
                ),
                Err(Denial::SurfaceMismatch),
            );
        }
        // Phase `keys_are_muted_while_the_host_focuses_another_surface`.
        {
            let mut fw = RemoteInputFirewall::new();
            fw.publish_view(ShellView::new(
                vec![(SHARED, Rect::new(100.0, 50.0, 500.0, 350.0))],
                Some(PRIVATE),
                false,
                false,
            ));
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: 30,
                        pressed: true,
                        modifiers: 0
                    },
                    Instant::now()
                ),
                Err(Denial::FocusMuted),
            );
        }
        // Phase `an_ordinary_key_reaches_the_seat_when_the_shared_surface_has_focus`.
        {
            let mut fw = firewall();
            assert!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: 30,
                        pressed: true,
                        modifiers: 0
                    },
                    Instant::now()
                )
                .is_ok(),
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn host_reserved_chords_never_reach_the_seat() {
        // Phase `a_vt_switch_chord_is_refused_even_with_the_modifier_field_zeroed`.
        // The wire's `modifiers` field is peer-written; a guest wanting a
        // forbidden chord declares zero, so the scrubber reconstructs modifiers from the key stream.
        {
            let mut fw = firewall();
            let now = Instant::now();
            for code in [keycode::LEFT_CTRL, keycode::LEFT_ALT] {
                assert!(
                    fw.review(
                        PEER,
                        SHARED,
                        &InputEvent::Key {
                            keysym: code,
                            pressed: true,
                            modifiers: 0
                        },
                        now
                    )
                    .is_ok(),
                    "a bare modifier has to reach the seat or xkb state drifts",
                );
            }
            // F2 with Ctrl+Alt held is Ctrl+Alt+F2 — the VT switch.
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: 60,
                        pressed: true,
                        modifiers: 0
                    },
                    now
                ),
                Err(Denial::SystemChord),
            );
        }
        // Phase `ctrl_alt_delete_is_refused`.
        {
            let mut fw = firewall();
            let now = Instant::now();
            for code in [keycode::LEFT_CTRL, keycode::LEFT_ALT] {
                let _ = fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: code,
                        pressed: true,
                        modifiers: 0,
                    },
                    now,
                );
            }
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: keycode::DELETE,
                        pressed: true,
                        modifiers: 0
                    },
                    now
                ),
                Err(Denial::SystemChord),
            );
        }
        // Phase `alt_tab_is_refused`.
        {
            let mut fw = firewall();
            let now = Instant::now();
            let _ = fw.review(
                PEER,
                SHARED,
                &InputEvent::Key {
                    keysym: keycode::LEFT_ALT,
                    pressed: true,
                    modifiers: 0,
                },
                now,
            );
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: keycode::TAB,
                        pressed: true,
                        modifiers: 0
                    },
                    now
                ),
                Err(Denial::SystemChord),
            );
        }
        // Phase `print_screen_is_kept_on_the_host`.
        {
            let mut fw = firewall();
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: keycode::PRINT_SCREEN,
                        pressed: true,
                        modifiers: 0,
                    },
                    Instant::now()
                ),
                Err(Denial::SystemChord),
            );
        }
        // Phase `the_super_key_never_reaches_the_seat`.
        {
            let mut fw = firewall();
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: keycode::LEFT_META,
                        pressed: true,
                        modifiers: 0
                    },
                    Instant::now()
                ),
                Err(Denial::SystemChord),
            );
        }
        // Phase `immutable_chord_is_refused_from_the_shared_table`.
        // An immutable chord is refused by the same table without a second
        // remote-only list. Configurable desktop bindings deliberately remain
        // client input for a sharing peer.
        {
            let mut fw = firewall();
            let now = Instant::now();
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::Key {
                        keysym: 59, // F1
                        pressed: true,
                        modifiers: 0
                    },
                    now
                ),
                Err(Denial::SystemChord),
            );
        }
    }

    #[test]
    #[allow(
        clippy::bool_assert_comparison,
        clippy::too_many_lines,
        reason = "the rate-budget phase compares admission verdicts to literal booleans; each phase keeps one former test's body verbatim"
    )]
    fn rate_limits_local_priority_and_kill_switch_block_the_peer() {
        // Phase `tests_mod_the_hundred_and_twenty_first_event_in_a_second_is_dropped`.
        {
            let mut fw = firewall();
            let t0 = Instant::now();
            for i in 0..MAX_EVENTS_PER_SECOND {
                let at = t0 + Duration::from_micros(i as u64);
                assert!(
                    fw.review(
                        PEER,
                        SHARED,
                        &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                        at
                    )
                    .is_ok(),
                    "event {i} is inside the budget",
                );
            }
            let at = t0 + Duration::from_micros(MAX_EVENTS_PER_SECOND as u64);
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                    at
                ),
                Err(Denial::RateLimited),
            );
            // A second later the window has rolled and the peer is served again.
            let later = t0 + RATE_WINDOW + Duration::from_millis(1);
            assert!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                    later
                )
                .is_ok(),
            );
        }
        // Phase `an_eleventh_click_inside_a_hundred_milliseconds_blocks_the_peer`.
        {
            let mut fw = firewall();
            let t0 = Instant::now();
            // Establish a pointer position so the button has a coordinate.
            fw.review(
                PEER,
                SHARED,
                &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                t0,
            )
            .expect("motion inside the tile");

            for i in 0..CLICK_BURST_LIMIT {
                let at = t0 + Duration::from_micros(i as u64);
                assert!(
                    fw.review(
                        PEER,
                        SHARED,
                        &InputEvent::PointerButton {
                            button: 272,
                            pressed: true
                        },
                        at
                    )
                    .is_ok(),
                    "click {i} is inside the burst allowance",
                );
            }
            let at = t0 + Duration::from_micros(CLICK_BURST_LIMIT as u64);
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerButton {
                        button: 272,
                        pressed: true
                    },
                    at
                ),
                Err(Denial::ClickBurst),
            );
            // The block covers everything, not only further clicks.
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 11.0, y: 11.0 },
                    at + Duration::from_millis(10)
                ),
                Err(Denial::ClickBurst),
            );
            assert!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 11.0, y: 11.0 },
                    at + CLICK_BURST_BLOCK + Duration::from_millis(1)
                )
                .is_ok(),
            );
        }
        // Phase `local_input_blocks_the_remote_side_for_five_hundred_milliseconds`.
        {
            let mut fw = firewall();
            let t0 = Instant::now();
            fw.note_local_input(t0);
            assert_eq!(
                fw.host_override(t0 + Duration::from_millis(499)),
                Some(Denial::PhysicalPriority),
            );
            assert_eq!(fw.host_override(t0 + PHYSICAL_PRIORITY_WINDOW), None);
        }
        // Phase `the_kill_switch_latches_until_it_is_cleared`.
        {
            let mut fw = firewall();
            assert_eq!(fw.host_override(Instant::now()), None);
            fw.engage_kill_switch();
            assert_eq!(fw.host_override(Instant::now()), Some(Denial::KillSwitch));
            // Time does not lift it — only the host does.
            assert_eq!(
                fw.host_override(Instant::now() + Duration::from_secs(3600)),
                Some(Denial::KillSwitch),
            );
            fw.clear_kill_switch();
            assert_eq!(fw.host_override(Instant::now()), None);
        }
        // Phase `a_denied_event_still_costs_the_peer_its_rate_budget`.
        {
            let mut fw = firewall();
            let t0 = Instant::now();
            // Every one of these is refused on the region check.
            for i in 0..MAX_EVENTS_PER_SECOND {
                let at = t0 + Duration::from_micros(i as u64);
                assert_eq!(
                    fw.review(
                        PEER,
                        SHARED,
                        &InputEvent::PointerMotion {
                            x: 10_000.0,
                            y: 10_000.0
                        },
                        at
                    ),
                    Err(Denial::OutsideRegion),
                );
            }
            // The budget is spent, so a legitimate event is refused too — a flood of junk can't be used to reset the meter.
            let at = t0 + Duration::from_micros(MAX_EVENTS_PER_SECOND as u64);
            assert_eq!(
                fw.review(
                    PEER,
                    SHARED,
                    &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                    at
                ),
                Err(Denial::RateLimited),
            );
        }
    }
}

/// Denials proven through the production injection path, against a real
/// `State`/`Seat`, asserting both the refusal rule and that the seat didn't
/// move — a return-value-only check would pass even if the refusal happened after injection.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod production_path {
    use super::*;
    use crate::compositor::state::State;
    use crate::network::streaming::StreamingShare;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    const PEER: PeerId = 3;
    const SHARED: SurfaceId = 501;
    const PRIVATE: SurfaceId = 502;

    /// The tile the guest was shared: 400x300 at (100, 50).
    const TILE: Rect = Rect {
        x0: 100.0,
        y0: 50.0,
        x1: 500.0,
        y1: 350.0,
    };

    /// A `State` with a live seat, one writable share, and one shared surface on screen holding keyboard focus.
    fn staged() -> State {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state
            .share_registry
            .insert(PEER, SHARED, StreamingShare::new());
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![(SHARED, TILE)],
            Some(SHARED),
            false,
            false,
        ));
        let _ = state.remote_seats.seat_mut(PEER).enter(SHARED);
        state
    }

    fn pointer_at(state: &State) -> (f64, f64) {
        let p = state
            .seat
            .get_pointer()
            .expect("seat has a pointer")
            .current_location();
        (p.x, p.y)
    }

    fn key_is_down(state: &State, keycode: u32) -> bool {
        state
            .seat
            .get_keyboard()
            .expect("seat has a keyboard")
            .pressed_keys()
            .iter()
            .any(|k| u32::from(*k) == keycode.saturating_add(8))
    }

    /// Put the peer's pointer somewhere legitimate so button events have a coordinate to be judged at.
    fn establish_cursor(state: &mut State) {
        state
            .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 10.0 })
            .expect("a point inside the shared tile is injectable");
    }

    #[test]
    fn production_pointer_input_lands_only_on_visible_shared_tiles() {
        // Phase `a_read_only_share_refuses_input_and_the_pointer_does_not_move`.
        // Read-only: the share exists, the owner took write access away.
        {
            let mut state = staged();
            state.share_registry.set_read_only(PEER, SHARED, true);
            let before = pointer_at(&state);

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 10.0, y: 10.0 },
            );

            assert_eq!(verdict, Err(Denial::ReadOnly));
            assert_eq!(pointer_at(&state), before, "a refused event moved the seat");
            assert!(state.last_remote_input.is_none());
        }
        // Phase `a_surface_the_peer_was_never_shared_is_refused`.
        // Surface ownership: a peer naming a surface it was never shared.
        {
            let mut state = staged();
            let before = pointer_at(&state);

            let verdict = state.review_remote_input(
                PEER,
                PRIVATE,
                InputEvent::PointerMotion { x: 10.0, y: 10.0 },
            );

            assert_eq!(verdict, Err(Denial::NotShared));
            assert_eq!(pointer_at(&state), before);
        }
        // Phase `a_coordinate_outside_the_shared_region_never_moves_the_host_pointer`.
        // Region: a coordinate outside the shown rect — the tile is 400x300, so `(700, 20)` is off its right edge but still on the output.
        {
            let mut state = staged();
            let before = pointer_at(&state);

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 700.0, y: 20.0 },
            );

            assert_eq!(verdict, Err(Denial::OutsideRegion));
            assert_eq!(
                pointer_at(&state),
                before,
                "a coordinate outside the shared tile reached the seat",
            );
            assert_eq!(
                state.last_remote_input_denial,
                Some((PEER, SHARED, Denial::OutsideRegion)),
            );
        }
        // Phase `an_accepted_coordinate_lands_inside_the_shared_tile`.
        // An accepted coordinate is translated from the guest's surface-local
        // space into host output space — it can only address points inside the shared rect.
        {
            let mut state = staged();
            state
                .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 20.0 })
                .expect("inside the tile");
            assert_eq!(pointer_at(&state), (110.0, 70.0));
        }
        // Phase `a_click_aimed_at_an_occluded_surface_is_dropped`.
        // Z-order: the shared surface sits underneath something else at that point — the clickjacking refusal.
        {
            let mut state = staged();
            establish_cursor(&mut state);
            let before = pointer_at(&state);

            // The host raises a private window over the shared one; the share is untouched, only the screen changed.
            state.remote_input_firewall.publish_view(ShellView::new(
                vec![(PRIVATE, TILE), (SHARED, TILE)],
                Some(SHARED),
                false,
                false,
            ));

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                },
            );

            assert_eq!(verdict, Err(Denial::Occluded));
            assert_eq!(pointer_at(&state), before);
            assert!(
                state.share_registry.get(PEER, SHARED).is_some(),
                "the share is still live — occlusion is a screen fact, not a share fact",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn production_keys_from_a_guest_never_reach_host_or_private_windows() {
        // Phase `ctrl_alt_f2_from_a_guest_never_reaches_a_client`.
        // Scrubber: `Ctrl+Alt+F2` is the kernel's VT switch. Sent as a real peer
        // would — modifier keycodes in their own events, wire `modifiers` left at zero.
        {
            const F2: u32 = 60;
            let mut state = staged();

            for code in [29u32 /* Ctrl */, 56 /* Alt */] {
                state
                    .review_remote_input(
                        PEER,
                        SHARED,
                        InputEvent::Key {
                            keysym: code,
                            pressed: true,
                            modifiers: 0,
                        },
                    )
                    .expect("a bare modifier reaches the seat; xkb state depends on it");
            }

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::Key {
                    keysym: F2,
                    pressed: true,
                    modifiers: 0,
                },
            );

            assert_eq!(verdict, Err(Denial::SystemChord));
            assert!(
                !key_is_down(&state, F2),
                "the VT-switch chord reached the seat",
            );
        }
        // Phase `keystrokes_stop_the_instant_the_host_focuses_a_private_window`.
        // Scrubber, focus guard: the host moves focus to a private window and the guest's keyboard goes silent mid-stream.
        {
            const A: u32 = 30;
            let mut state = staged();

            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: A,
                        pressed: true,
                        modifiers: 0,
                    },
                )
                .expect("the shared surface has focus");
            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: A,
                        pressed: false,
                        modifiers: 0,
                    },
                )
                .expect("release mirrors the press");

            // The host clicks into a window that is not shared.
            state.remote_input_firewall.publish_view(ShellView::new(
                vec![
                    (SHARED, TILE),
                    (PRIVATE, Rect::new(600.0, 50.0, 900.0, 350.0)),
                ],
                Some(PRIVATE),
                false,
                false,
            ));

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::Key {
                    keysym: A,
                    pressed: true,
                    modifiers: 0,
                },
            );

            assert_eq!(verdict, Err(Denial::FocusMuted));
            assert!(
                !key_is_down(&state, A),
                "a muted keystroke reached the seat"
            );
        }
        // Phase `the_super_key_from_a_guest_never_reaches_a_client`.
        // Scrubber: `Super` opens the shell's launcher and is the first half of every compositor chord; a guest never gets to press it.
        {
            const LEFT_META: u32 = 125;
            let mut state = staged();

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::Key {
                    keysym: LEFT_META,
                    pressed: true,
                    modifiers: 0,
                },
            );

            assert_eq!(verdict, Err(Denial::SystemChord));
            assert!(
                !key_is_down(&state, LEFT_META),
                "the compositor modifier reached the seat",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn production_rate_limits_and_kill_switch_stop_every_peer() {
        // Phase `the_hundred_and_twenty_first_event_in_a_second_is_dropped`.
        // Rate limit: 120 events/sec sustained, and the next one is refused.
        {
            let mut state = staged();
            let started = std::time::Instant::now();

            for i in 0..MAX_EVENTS_PER_SECOND {
                let y = 10.0 + f64::from(u32::try_from(i % 100).unwrap_or(0));
                state
                    .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y })
                    .unwrap_or_else(|d| panic!("event {i} refused as {d:?}, inside the budget"));
            }

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 11.0, y: 11.0 },
            );

            // Scrubber, focus guard: focus moves to a private window and the guest's keyboard goes silent mid-stream.
            let elapsed = started.elapsed();
            assert!(
                elapsed < RATE_WINDOW,
                "the burst took {elapsed:?}, longer than the {RATE_WINDOW:?} window, \
                 so the rate limit was never exercised",
            );
            assert_eq!(verdict, Err(Denial::RateLimited));
        }
        // Phase `a_click_storm_is_blocked_before_the_eleventh_click_reaches_the_seat`.
        // Rate limit, burst half: a click storm trips a block covering everything the peer sends, not only further clicks.
        {
            let mut state = staged();
            establish_cursor(&mut state);
            let started = std::time::Instant::now();

            for i in 0..CLICK_BURST_LIMIT {
                state
                    .review_remote_input(
                        PEER,
                        SHARED,
                        InputEvent::PointerButton {
                            button: 272,
                            pressed: true,
                        },
                    )
                    .unwrap_or_else(|d| panic!("click {i} refused as {d:?}, inside the allowance"));
            }

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                },
            );

            // The burst window is wall-clock; a machine slow enough to spread eleven clicks over 100ms never asked the question.
            let elapsed = started.elapsed();
            assert!(
                elapsed < CLICK_BURST_WINDOW,
                "the burst took {elapsed:?}, longer than the {CLICK_BURST_WINDOW:?} window, \
                 so the click limit was never exercised",
            );
            assert_eq!(verdict, Err(Denial::ClickBurst));

            // Motion isn't a click either, and is refused too — the block is on the peer, not the button.
            assert_eq!(
                state.review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::PointerMotion { x: 20.0, y: 20.0 }
                ),
                Err(Denial::ClickBurst),
            );
        }
        // Phase `the_kill_switch_stops_every_peer_and_drops_their_write_access`.
        // Kill switch: one press stops every peer, and keeps them stopped.
        {
            const OTHER_PEER: PeerId = 4;
            let mut state = staged();
            state
                .share_registry
                .insert(OTHER_PEER, SHARED, StreamingShare::new());
            assert!(state.remote_seats.key_transition(PEER, 42, true));
            establish_cursor(&mut state);
            let before = pointer_at(&state);

            let revoked = state.engage_remote_input_kill_switch();
            assert_eq!(revoked, 2, "both peers' shares lose write access");

            for peer in [PEER, OTHER_PEER] {
                assert_eq!(
                    state.review_remote_input(
                        peer,
                        SHARED,
                        InputEvent::PointerMotion { x: 20.0, y: 20.0 }
                    ),
                    Err(Denial::KillSwitch),
                    "peer {peer} still reached the seat after the kill switch",
                );
            }
            assert_eq!(pointer_at(&state), before);
            assert!(
                state.remote_seats.is_empty(),
                "emergency teardown must release and remove every remote seat"
            );
            assert!(state.share_registry.get(PEER, SHARED).is_none());
            assert!(state.share_registry.get(OTHER_PEER, SHARED).is_none());

            // The latch makes this a panic key, not a policy tweak: republishing the screen doesn't undo it.
            state.remote_input_firewall.publish_view(ShellView::new(
                vec![(SHARED, TILE)],
                Some(SHARED),
                false,
                false,
            ));
            assert_eq!(
                state.review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::PointerMotion { x: 20.0, y: 20.0 }
                ),
                Err(Denial::KillSwitch),
            );
            // The shares themselves are gone too, so clearing the latch alone
            // cannot resurrect a session.
            state.remote_input_firewall.clear_kill_switch();
            assert_eq!(
                state.review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::PointerMotion { x: 20.0, y: 20.0 }
                ),
                Err(Denial::NotShared),
            );
        }
        // Phase `a_flooding_peer_cannot_suppress_the_kill_switch`.
        // A guest can't outrun the kill switch: checked before anything about the share, ahead of the per-peer budget.
        {
            let mut state = staged();
            state.engage_remote_input_kill_switch();
            for i in 0..500 {
                assert_eq!(
                    state.review_remote_input(
                        PEER,
                        SHARED,
                        InputEvent::PointerMotion { x: 10.0, y: 10.0 }
                    ),
                    Err(Denial::KillSwitch),
                    "flood event {i} was refused on some other rule",
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn production_remote_input_is_refused_while_locked_unpublished_or_physically_used() {
        // Phase `a_locked_session_refuses_remote_input_that_would_otherwise_pass`.
        // A locked session has no driver, remote least of all — the lock surface
        // isn't part of the tiled tree, so this isn't covered by the z-order check on its own.
        {
            let mut state = staged();

            // Exactly the event the unlocked staging accepts.
            state
                .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 10.0 })
                .expect("the same event is accepted while the session is unlocked");

            state.remote_input_firewall.publish_view(
                ShellView::new(vec![(SHARED, TILE)], Some(SHARED), false, false).locked(true),
            );

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 20.0, y: 20.0 },
            );

            assert_eq!(verdict, Err(Denial::SessionLocked));
            assert_eq!(
                pointer_at(&state),
                (110.0, 60.0),
                "the pointer is still where the accepted event left it; the refused one moved it",
            );
        }
        // Phase `remote_input_within_the_physical_priority_window_is_dropped`.
        // Physical priority: the host's own input blocks the remote side.
        {
            let mut state = staged();
            let before = pointer_at(&state);

            state
                .remote_input_firewall
                .note_local_input(std::time::Instant::now());

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 10.0, y: 10.0 },
            );

            assert_eq!(verdict, Err(Denial::PhysicalPriority));
            assert_eq!(pointer_at(&state), before);
        }
        // Phase `physical_priority_mutes_new_actions_but_allows_held_input_cleanup`.
        {
            const SHIFT: u32 = 42;
            const A: u32 = 30;
            const LEFT: u32 = 272;
            let mut state = staged();
            establish_cursor(&mut state);
            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: SHIFT,
                        pressed: true,
                        modifiers: 0,
                    },
                )
                .expect("remote Shift press before host takeover");
            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: true,
                    },
                )
                .expect("remote drag before host takeover");

            state
                .remote_input_firewall
                .note_local_input(std::time::Instant::now());
            assert_eq!(
                state.review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: A,
                        pressed: true,
                        modifiers: 0,
                    },
                ),
                Err(Denial::PhysicalPriority),
                "new remote actions remain muted"
            );
            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: SHIFT,
                        pressed: false,
                        modifiers: 0,
                    },
                )
                .expect("key release is cleanup, not a new action");
            state
                .review_remote_input(
                    PEER,
                    SHARED,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: false,
                    },
                )
                .expect("button release is cleanup, not a new action");
            assert!(!key_is_down(&state, SHIFT));
            assert!(!state.remote_seats.button_transition(PEER, LEFT, false));
        }
        // Phase `a_focus_grab_naming_an_unshared_surface_is_refused`.
        // A focus grab points the host's keyboard somewhere; a guest must not be able to aim it outside its share.
        {
            let mut state = staged();
            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerEnter { surface: PRIVATE },
            );
            assert_eq!(verdict, Err(Denial::SurfaceMismatch));
            assert_eq!(
                state
                    .remote_seats
                    .seat(PEER)
                    .and_then(|seat| seat.focused_surface),
                Some(SHARED),
                "a refused grab changed the peer's existing focus",
            );
        }
        // Phase `a_compositor_that_has_published_no_screen_refuses_remote_input`.
        // The deny-biased default: a compositor that hasn't published a screen can't answer where an event would land, so it refuses.
        {
            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            state
                .share_registry
                .insert(PEER, SHARED, StreamingShare::new());

            let verdict = state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::PointerMotion { x: 1.0, y: 1.0 },
            );

            assert_eq!(verdict, Err(Denial::NoView));
            assert_eq!(pointer_at(&state), (0.0, 0.0));
        }
    }

    #[test]
    fn a_locally_held_pointer_refuses_peer_pointer_input_but_not_keys() {
        let mut state = staged();
        state.remote_input_firewall.publish_view(
            ShellView::new(vec![(SHARED, TILE)], Some(SHARED), false, false).pointer_held(true),
        );
        for event in [
            InputEvent::PointerMotion { x: 10.0, y: 10.0 },
            InputEvent::PointerEnter { surface: SHARED },
            InputEvent::PointerButton {
                button: 0x110,
                pressed: true,
            },
        ] {
            assert_eq!(
                state.review_remote_input(PEER, SHARED, event),
                Err(Denial::PointerHeld),
                "a peer must neither steer nor break a local pointer lock"
            );
        }
        assert_ne!(
            state.review_remote_input(
                PEER,
                SHARED,
                InputEvent::Key {
                    keysym: 30,
                    pressed: true,
                    modifiers: 0,
                },
            ),
            Err(Denial::PointerHeld),
            "typing into the shared app does not touch the pointer"
        );
    }
}
