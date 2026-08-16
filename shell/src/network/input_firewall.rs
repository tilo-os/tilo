//! The firewall every remote-input event crosses before it reaches the seat.
//!
//! A peer with write access to a shared surface is driving the owner's
//! physical seat. Without a filter that is a full keyboard and mouse on the
//! host's machine: a guest could type a TTY-switch chord, move the pointer
//! anywhere on the output, click through whatever the host has drawn on top,
//! or hold the input queue open at line rate. The share record says *whether*
//! a peer may drive a surface; this module says *what* it may drive, *where*,
//! and *how fast*.
//!
//! Checks, in the order they run. Every one of them refuses when it cannot
//! answer — an unobservable subject is a denial, never a pass.
//!
//! | Check | Refuses |
//! |---|---|
//! | Kill switch | everything, until the host clears it |
//! | Physical priority | everything for [`PHYSICAL_PRIORITY_WINDOW`] after any local input |
//! | Region | a coordinate outside the shared surface's content rect |
//! | Z-order | a coordinate where that surface is not the topmost thing on screen |
//! | Scrubber | system chords, and any key while the surface lacks keyboard focus |
//! | Rate limit | more than [`MAX_EVENTS_PER_SECOND`], or a click burst |
//!
//! The read-only flag and surface ownership are checked before this, by the
//! share registry, in [`crate::compositor::state::State::apply_remote_input`]
//! — the whole pipeline reads top to bottom there.
//!
//! # Coordinates
//!
//! A peer sees only the surface it was shared, so it sends **surface-local**
//! logical pixels (see [`crate::shell::peer_view::PeerView`]). The seat wants
//! global output coordinates. The region check is what makes that translation
//! safe: an accepted event is re-emitted with its coordinate moved into the
//! surface's on-screen rect, so a peer can only ever address points inside
//! the rectangle it was actually given.
//!
//! # Why the shell's own chord table
//!
//! The scrubber does not carry its own list of forbidden chords. It asks
//! [`crate::compositor::client_input::shell_owns_chord`] — the same table that
//! decides whether the *local* user's keystroke belongs to the shell rather
//! than to the focused client. A chord the shell grabs for itself is exactly
//! one a guest must not be able to synthesise, so the two cannot drift. On top
//! of that table sit the chords the shell never sees because the kernel or the
//! session takes them first: `Ctrl+Alt+F1..F12` and `Ctrl+Alt+Delete`.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};

use crate::compositor::client_input::{ChordMods, chord_key_for_evdev, shell_owns_chord};
use crate::network::protocol::{PeerId, SurfaceId};
use crate::network::streaming::InputEvent;

/// How long local input keeps the remote side out. The host reaching for
/// their own mouse or keyboard is an unambiguous "I am taking over"; a
/// guest whose motion interleaves with the host's fights the cursor.
pub const PHYSICAL_PRIORITY_WINDOW: Duration = Duration::from_millis(500);

/// Sustained ceiling on events a single peer may deliver, per second.
pub const MAX_EVENTS_PER_SECOND: usize = 120;

/// The window [`MAX_EVENTS_PER_SECOND`] is measured over.
pub const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Clicks inside [`CLICK_BURST_WINDOW`] that are still tolerated. The
/// *next* one is a burst.
pub const CLICK_BURST_LIMIT: usize = 10;

/// The window a click burst is measured over.
pub const CLICK_BURST_WINDOW: Duration = Duration::from_millis(100);

/// How long a peer stays blocked after tripping the click burst. The
/// design calls for "a temporary block" without naming a duration; one
/// second is long enough to break an automated click storm and short
/// enough that a human who trackpad-fumbled is not locked out.
pub const CLICK_BURST_BLOCK: Duration = Duration::from_secs(1);

/// evdev keycodes the scrubber names directly.
mod keycode {
    pub const DELETE: u32 = 111;
    pub const TAB: u32 = 15;
    pub const LEFT_META: u32 = 125;
    pub const RIGHT_META: u32 = 126;
    pub const LEFT_CTRL: u32 = 29;
    pub const RIGHT_CTRL: u32 = 97;
    pub const LEFT_SHIFT: u32 = 42;
    pub const RIGHT_SHIFT: u32 = 54;
    pub const LEFT_ALT: u32 = 56;
    pub const RIGHT_ALT: u32 = 100;
}

/// Why an event did not reach the seat.
///
/// Carried out of the firewall rather than collapsed to a bool so the
/// compositor can log which rule fired and tests can assert on the
/// specific denial rather than merely on "something dropped it".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    /// The host hit the kill switch.
    KillSwitch,
    /// The host used their own keyboard or mouse within the last
    /// [`PHYSICAL_PRIORITY_WINDOW`].
    PhysicalPriority,
    /// The share exists but the owner marked it read-only.
    ReadOnly,
    /// No share record for this `(peer, surface)` pair at all.
    NotShared,
    /// The compositor has not told the firewall what is on screen, so
    /// neither the region nor the z-order question can be answered.
    NoView,
    /// The surface is shared but is not currently on screen.
    NotOnScreen,
    /// The coordinate falls outside the shared surface's content rect.
    OutsideRegion,
    /// Something else is on top at that point — the clickjacking refusal.
    Occluded,
    /// A button arrived before any motion established where the peer's
    /// pointer is, so there is no coordinate to check.
    NoPointerPosition,
    /// A system chord the shell (or the kernel) reserves.
    SystemChord,
    /// The shared surface does not hold the seat's keyboard focus.
    FocusMuted,
    /// The event named a surface other than the one the share covers.
    SurfaceMismatch,
    /// More than [`MAX_EVENTS_PER_SECOND`] in the last second.
    RateLimited,
    /// More than [`CLICK_BURST_LIMIT`] clicks inside
    /// [`CLICK_BURST_WINDOW`], or still inside the resulting block.
    ClickBurst,
}

impl Denial {
    /// A short stable token for logs and metrics.
    pub fn reason(self) -> &'static str {
        match self {
            Denial::KillSwitch => "kill-switch",
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
        }
    }
}

/// What the compositor last told the firewall about the screen.
///
/// A default-constructed view has observed nothing, and every check that
/// needs it refuses. That is deliberate: a firewall that silently passes
/// events because nobody published a layout is worse than one that blocks
/// a feature until the compositor wires it up.
#[derive(Debug, Clone, Default)]
pub struct ShellView {
    /// Foreign tiles front-to-back, exactly as
    /// [`crate::compositor::client_input::foreign_tiles`] enumerates them.
    tiles: Vec<(SurfaceId, Rect)>,
    /// The surface holding the seat's keyboard focus, or `None` when the
    /// shell's own chrome owns the keyboard.
    keyboard_focus: Option<SurfaceId>,
    /// Shell chrome (overlay, palette, permission prompt) is drawn over
    /// the tiles and is taking input.
    chrome_captures: bool,
    /// An overlay is open, so the shell grabs `Escape`.
    overlay_open: bool,
    /// Set by every real publish. Distinguishes "nothing is on screen"
    /// from "nobody has looked".
    observed: bool,
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
            keyboard_focus,
            chrome_captures,
            overlay_open,
            observed: true,
        }
    }

    /// Whether the compositor has published anything at all.
    pub fn observed(&self) -> bool {
        self.observed
    }

    /// The on-screen rect of `surface`, if it is currently on screen.
    pub fn rect(&self, surface: SurfaceId) -> Option<Rect> {
        self.tiles
            .iter()
            .find(|(id, _)| *id == surface)
            .map(|(_, r)| *r)
    }

    /// The surface actually on top at `point` (global coordinates).
    ///
    /// Front-to-back order comes from the compositor's own enumeration,
    /// so this answers the same question local pointer routing does.
    pub fn topmost_at(&self, point: (f64, f64)) -> Option<SurfaceId> {
        let p = Point::new(point.0, point.1);
        self.tiles
            .iter()
            .find(|(_, rect)| rect.contains(p))
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
    /// Modifier keys this peer is currently holding, derived from the key
    /// stream it has actually sent.
    ///
    /// The wire carries a `modifiers` bitfield, but that field is written
    /// by the peer and a peer that wants a forbidden chord simply sends
    /// zero. Modifier state is therefore reconstructed from the keycodes
    /// the peer sends, and the declared bitfield is unioned on top — it
    /// can only ever add denials, never remove one.
    held: HeldMods,
    /// Where this peer's pointer was last accepted, and on which surface.
    cursor: Option<(SurfaceId, (f64, f64))>,
}

/// Modifier keys held down, tracked per side so a two-Ctrl release order
/// cannot leave a phantom modifier latched.
#[derive(Debug, Default, Clone, Copy)]
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
    /// Fold a key transition into the held set. Returns `true` when the
    /// keycode was a modifier.
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

/// The remote-input firewall.
///
/// One per compositor, living on
/// [`crate::compositor::state::State`]. Holds the screen view the
/// coordinate checks read, the host-override latches, and per-peer
/// budgets.
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

    /// Publish what is on screen. Called once per compositor tick, after
    /// keyboard focus has been reconciled, so the view a remote event is
    /// judged against is the one the local user is looking at.
    pub fn publish_view(&mut self, view: ShellView) {
        self.view = view;
    }

    /// Borrow the current view. Diagnostics and tests.
    pub fn view(&self) -> &ShellView {
        &self.view
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

    /// Release the kill switch.
    pub fn clear_kill_switch(&mut self) {
        self.kill_switch = false;
    }

    /// Whether remote input is currently killed.
    pub fn kill_switch_engaged(&self) -> bool {
        self.kill_switch
    }

    /// Forget a peer's budget and held modifiers — call when it disconnects.
    pub fn forget_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
    }

    /// The host overrides, checked before anything about the share. Both
    /// are absolute and neither needs to look at the event.
    pub fn host_override(&self, now: Instant) -> Option<Denial> {
        if self.kill_switch {
            return Some(Denial::KillSwitch);
        }
        if let Some(last) = self.last_local_input {
            if now.saturating_duration_since(last) < PHYSICAL_PRIORITY_WINDOW {
                return Some(Denial::PhysicalPriority);
            }
        }
        None
    }

    /// Run the region, z-order, scrubber and rate-limit checks.
    ///
    /// `surface` is the share the event arrived under; the share's
    /// existence and its read-only flag have already been checked by the
    /// caller. Returns the event to inject — pointer coordinates
    /// translated from surface-local into global output space — or the
    /// rule that refused it.
    pub fn review(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        event: &InputEvent,
        now: Instant,
    ) -> Result<InputEvent, Denial> {
        // Every event a peer offers is counted, whichever check ends up
        // refusing it. A flood of events that are individually denied is
        // still a flood, and it must not buy the attacker a fresh budget
        // for the one event that would have passed.
        self.note_offered(peer, now);

        if !self.view.observed {
            return Err(Denial::NoView);
        }

        let outcome = self.check(peer, surface, event, now);
        if let Ok(InputEvent::PointerMotion { x, y }) = &outcome {
            // Latch where the peer's pointer now is, so a button arriving
            // without its own coordinate is checked against a point that
            // itself passed region + z-order.
            self.peer_mut(peer).cursor = Some((surface, (*x, *y)));
        }
        outcome
    }

    /// The checks proper, split out so [`Self::review`] owns the
    /// bookkeeping that has to happen whatever the verdict.
    fn check(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        event: &InputEvent,
        now: Instant,
    ) -> Result<InputEvent, Denial> {
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
                // A button carries no coordinate, so it is checked where
                // the peer's last accepted motion put the pointer. No
                // motion yet means there is no point to check, and an
                // unanswerable check refuses.
                let Some((cursor_surface, local)) = self.peer_mut(peer).cursor else {
                    return Err(Denial::NoPointerPosition);
                };
                if cursor_surface != surface {
                    return Err(Denial::SurfaceMismatch);
                }
                self.locate(surface, local)?;
                self.spend_budget(peer, now, *pressed)?;
                Ok(InputEvent::PointerButton {
                    button: *button,
                    pressed: *pressed,
                })
            }
            InputEvent::PointerEnter { surface: target } => {
                // The inner surface is what the focus grab is applied to.
                // Left unchecked it is a way to point the host's keyboard
                // at a surface the peer was never shared.
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
                self.spend_budget(peer, now, false)?;
                Ok(event.clone())
            }
            InputEvent::Key {
                keysym,
                pressed,
                modifiers,
            } => {
                let verdict = self.scrub_key(peer, surface, *keysym, *pressed, *modifiers);
                verdict?;
                self.spend_budget(peer, now, false)?;
                Ok(event.clone())
            }
        }
    }

    /// Region + z-order for one surface-local coordinate. Returns the
    /// global point to inject.
    fn locate(&self, surface: SurfaceId, local: (f64, f64)) -> Result<(f64, f64), Denial> {
        let Some(rect) = self.view.rect(surface) else {
            return Err(Denial::NotOnScreen);
        };
        // Region: the coordinate has to name a point inside the content
        // rect the peer was actually shown. Half-open on the far edge so
        // a point on the boundary belongs to exactly one tile.
        if !(local.0 >= 0.0 && local.1 >= 0.0 && local.0 < rect.width() && local.1 < rect.height())
        {
            return Err(Denial::OutsideRegion);
        }
        let global = (rect.x0 + local.0, rect.y0 + local.1);
        // Z-order: chrome drawn over the tiles takes the whole screen, so
        // nothing beneath it is topmost anywhere.
        if self.view.chrome_captures {
            return Err(Denial::Occluded);
        }
        // Otherwise ask the live tile order what is really on top there.
        // Trusting the share record instead is the clickjacking hole:
        // a surface can be shared, and simultaneously be underneath
        // something the host put in front of it.
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
        // Track the peer's own modifier stream first, so a modifier this
        // event presses counts towards the chord it is part of.
        let is_modifier = state.held.note(keycode, pressed);
        let mods = state.held.mods().union(ChordMods::from_xkb_bits(declared_modifiers));

        // Focus guard. A keystroke only injects while the shared surface
        // is the one the seat's keyboard is pointed at; the moment the
        // host focuses anything else — another window, or the shell's own
        // chrome — the remote keyboard stream goes silent. Checked before
        // the chord table so a peer cannot type *anything* into a private
        // window, chord or not.
        if chrome_captures || focus != Some(surface) {
            return Err(Denial::FocusMuted);
        }

        // The Super key is refused outright rather than only in
        // combination: it opens the shell's own launcher on its own, and
        // holding it is the first half of every compositor chord.
        if matches!(keycode, keycode::LEFT_META | keycode::RIGHT_META) {
            return Err(Denial::SystemChord);
        }

        // A bare modifier transition is not a chord; it has to reach the
        // seat or the client's own xkb state drifts from the compositor's.
        if is_modifier {
            return Ok(());
        }

        let chord = chord_key_for_evdev(keycode);

        // Everything the shell grabs from the local user. One table, so a
        // chord added to the shell is refused here without anyone
        // remembering to update a second list.
        if shell_owns_chord(chord, mods, overlay_open) {
            return Err(Denial::SystemChord);
        }

        // Chords the shell never classifies because something below it
        // takes them first: the kernel's VT switch, and the session's
        // interrupt.
        if mods.ctrl && mods.alt {
            if matches!(chord, crate::compositor::client_input::ChordKey::Function(_)) {
                return Err(Denial::SystemChord);
            }
            if keycode == keycode::DELETE {
                return Err(Denial::SystemChord);
            }
        }
        // `Alt+Tab` is a window-switch convention everywhere; the shell
        // does not bind it today, and a guest must still not be able to
        // send it.
        if mods.alt && keycode == keycode::TAB {
            return Err(Denial::SystemChord);
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

        // `note_offered` already pruned and pushed this event, so the
        // window holds it. More than the ceiling means this one is over.
        if state.events.len() > MAX_EVENTS_PER_SECOND {
            return Err(Denial::RateLimited);
        }

        if is_click {
            prune(&mut state.clicks, now, CLICK_BURST_WINDOW);
            state.clicks.push_back(now);
            if state.clicks.len() > CLICK_BURST_LIMIT {
                state.blocked_until = Some(now + CLICK_BURST_BLOCK);
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
    fn a_firewall_that_has_seen_no_layout_refuses_everything() {
        let mut fw = RemoteInputFirewall::new();
        let verdict = fw.review(
            PEER,
            SHARED,
            &InputEvent::PointerMotion { x: 1.0, y: 1.0 },
            Instant::now(),
        );
        assert_eq!(verdict, Err(Denial::NoView));
    }

    #[test]
    fn motion_inside_the_region_is_translated_into_output_space() {
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

    #[test]
    fn motion_outside_the_region_is_refused() {
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

    #[test]
    fn motion_is_refused_where_another_surface_is_on_top() {
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

    #[test]
    fn chrome_drawn_over_the_tiles_occludes_every_shared_surface() {
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

    #[test]
    fn a_button_without_a_prior_motion_has_no_coordinate_and_is_refused() {
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

    #[test]
    fn a_focus_grab_naming_a_surface_the_share_does_not_cover_is_refused() {
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

    #[test]
    fn keys_are_muted_while_the_host_focuses_another_surface() {
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

    #[test]
    fn an_ordinary_key_reaches_the_seat_when_the_shared_surface_has_focus() {
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

    /// The wire's `modifiers` field is written by the peer. A guest that
    /// wants a forbidden chord simply declares zero, so the scrubber has
    /// to reconstruct modifier state from the key stream itself.
    #[test]
    fn a_vt_switch_chord_is_refused_even_with_the_modifier_field_zeroed() {
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

    #[test]
    fn ctrl_alt_delete_is_refused() {
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

    #[test]
    fn alt_tab_is_refused() {
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

    #[test]
    fn the_super_key_never_reaches_the_seat() {
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

    /// A chord the *shell* grabs is refused by the same table, without
    /// this module naming it: `Ctrl+K` opens the search palette.
    #[test]
    fn a_chord_the_shell_grabs_is_refused_from_the_shared_table() {
        let mut fw = firewall();
        let now = Instant::now();
        let _ = fw.review(
            PEER,
            SHARED,
            &InputEvent::Key {
                keysym: keycode::LEFT_CTRL,
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
                    keysym: 37, // 'k'
                    pressed: true,
                    modifiers: 0
                },
                now
            ),
            Err(Denial::SystemChord),
        );
    }

    #[test]
    fn the_hundred_and_twenty_first_event_in_a_second_is_dropped() {
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

    #[test]
    fn an_eleventh_click_inside_a_hundred_milliseconds_blocks_the_peer() {
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
        // And it lifts.
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

    #[test]
    fn local_input_blocks_the_remote_side_for_five_hundred_milliseconds() {
        let mut fw = firewall();
        let t0 = Instant::now();
        fw.note_local_input(t0);
        assert_eq!(
            fw.host_override(t0 + Duration::from_millis(499)),
            Some(Denial::PhysicalPriority),
        );
        assert_eq!(fw.host_override(t0 + PHYSICAL_PRIORITY_WINDOW), None);
    }

    #[test]
    fn the_kill_switch_latches_until_it_is_cleared() {
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

    #[test]
    #[allow(clippy::bool_assert_comparison)]
    fn a_denied_event_still_costs_the_peer_its_rate_budget() {
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
        // The budget is spent, so a legitimate event is refused too — a
        // flood of junk cannot be used to reset the meter.
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

/// Denials proven through the production injection path.
///
/// Everything here drives
/// [`crate::compositor::state::State::review_remote_input`] — the one door
/// between the network and the seat — against a real `State` with a real
/// smithay `Seat`, and asserts both the rule that refused the event *and*
/// that the seat did not move. A test that only asserted the return value
/// would still pass if the refusal happened after injection.
#[cfg(test)]
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

    /// A `State` with a live seat, one writable share, and one shared
    /// surface on screen holding keyboard focus.
    fn staged() -> State {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state
            .share_registry
            .insert(PEER, SHARED, StreamingShare::new());
        state
            .remote_input_firewall
            .publish_view(ShellView::new(vec![(SHARED, TILE)], Some(SHARED), false, false));
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
            .any(|k| u32::from(*k) == keycode)
    }

    /// Put the peer's pointer somewhere legitimate so button events have
    /// a coordinate to be judged at.
    fn establish_cursor(state: &mut State) {
        state
            .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 10.0 })
            .expect("a point inside the shared tile is injectable");
    }

    /// Read-only: the share exists, the owner took write access away.
    #[test]
    fn a_read_only_share_refuses_input_and_the_pointer_does_not_move() {
        let mut state = staged();
        state.share_registry.set_read_only(PEER, SHARED, true);
        let before = pointer_at(&state);

        let verdict =
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 10.0 });

        assert_eq!(verdict, Err(Denial::ReadOnly));
        assert_eq!(pointer_at(&state), before, "a refused event moved the seat");
        assert!(state.last_remote_input.is_none());
    }

    /// Surface ownership: a peer naming a surface it was never shared.
    #[test]
    fn a_surface_the_peer_was_never_shared_is_refused() {
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

    /// Region: a coordinate outside the rect the guest was actually shown.
    ///
    /// The tile is 400x300, so `(700, 20)` is off its right-hand side. It
    /// is nonetheless a real point on the output — under the previous
    /// code, which injected a peer's surface-local coordinates as if they
    /// were output-space ones, that is a pointer the guest could park
    /// anywhere on the host's screen.
    #[test]
    fn a_coordinate_outside_the_shared_region_never_moves_the_host_pointer() {
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

    /// An accepted coordinate is translated out of the guest's
    /// surface-local space into the host's output space — the guest can
    /// only ever address points inside the rectangle it was given.
    #[test]
    fn an_accepted_coordinate_lands_inside_the_shared_tile() {
        let mut state = staged();
        state
            .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 20.0 })
            .expect("inside the tile");
        assert_eq!(pointer_at(&state), (110.0, 70.0));
    }

    /// Z-order: the shared surface is underneath something else at that
    /// point. This is the clickjacking refusal — the share record still
    /// says the guest may drive this surface, and the answer is still no.
    #[test]
    fn a_click_aimed_at_an_occluded_surface_is_dropped() {
        let mut state = staged();
        establish_cursor(&mut state);
        let before = pointer_at(&state);

        // The host raises a private window over the shared one. The share
        // is untouched; only the screen changed.
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

    /// Scrubber: `Ctrl+Alt+F2` is the kernel's VT switch. A guest holding
    /// it would drop the host to a text console.
    ///
    /// Sent the way a peer really sends a chord — modifier keycodes in
    /// their own events, with the wire's `modifiers` field left at zero,
    /// which is exactly what a guest after a forbidden chord would do.
    #[test]
    fn ctrl_alt_f2_from_a_guest_never_reaches_a_client() {
        let mut state = staged();
        const F2: u32 = 60;

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

    /// Scrubber, focus guard: the host moves focus to a private window
    /// and the guest's keyboard goes silent mid-stream.
    #[test]
    fn keystrokes_stop_the_instant_the_host_focuses_a_private_window() {
        let mut state = staged();
        const A: u32 = 30;

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
            vec![(SHARED, TILE), (PRIVATE, Rect::new(600.0, 50.0, 900.0, 350.0))],
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
        assert!(!key_is_down(&state, A), "a muted keystroke reached the seat");
    }

    /// Rate limit: 120 events a second sustained, and the next one is
    /// refused.
    #[test]
    fn the_hundred_and_twenty_first_event_in_a_second_is_dropped() {
        let mut state = staged();
        let started = std::time::Instant::now();

        for i in 0..MAX_EVENTS_PER_SECOND {
            let y = 10.0 + (i % 100) as f64;
            state
                .review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y })
                .unwrap_or_else(|d| panic!("event {i} refused as {d:?}, inside the budget"));
        }

        let verdict =
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 11.0, y: 11.0 });

        // The rate window is wall-clock. If this machine took longer than
        // the window to deliver the burst, the test never asked the
        // question it claims to — say so rather than passing quietly.
        let elapsed = started.elapsed();
        assert!(
            elapsed < RATE_WINDOW,
            "the burst took {elapsed:?}, longer than the {RATE_WINDOW:?} window, \
             so the rate limit was never exercised",
        );
        assert_eq!(verdict, Err(Denial::RateLimited));
    }

    /// Physical priority: the host's own input blocks the remote side.
    #[test]
    fn remote_input_within_the_physical_priority_window_is_dropped() {
        let mut state = staged();
        let before = pointer_at(&state);

        state
            .remote_input_firewall
            .note_local_input(std::time::Instant::now());

        let verdict =
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 10.0, y: 10.0 });

        assert_eq!(verdict, Err(Denial::PhysicalPriority));
        assert_eq!(pointer_at(&state), before);
    }

    /// Kill switch: one press stops every peer, and keeps them stopped.
    #[test]
    fn the_kill_switch_stops_every_peer_and_drops_their_write_access() {
        let mut state = staged();
        const OTHER_PEER: PeerId = 4;
        state
            .share_registry
            .insert(OTHER_PEER, SHARED, StreamingShare::new());
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

        // The latch is what makes it a panic key rather than a policy
        // tweak: republishing the screen does not undo it, and neither
        // does anything the guest can send.
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![(SHARED, TILE)],
            Some(SHARED),
            false,
            false,
        ));
        assert_eq!(
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 20.0, y: 20.0 }),
            Err(Denial::KillSwitch),
        );
        // And write access is gone underneath it, so clearing the latch
        // alone does not hand control back.
        state.remote_input_firewall.clear_kill_switch();
        assert_eq!(
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 20.0, y: 20.0 }),
            Err(Denial::ReadOnly),
        );
    }

    /// A guest cannot outrun the kill switch: the latch is checked before
    /// anything about the share, ahead of the per-peer budget, so a peer
    /// flooding the queue is refused on the same rule as a quiet one.
    #[test]
    fn a_flooding_peer_cannot_suppress_the_kill_switch() {
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

    /// A focus grab is a way to point the host's keyboard somewhere. A
    /// guest must not be able to aim it at a surface outside its share.
    #[test]
    fn a_focus_grab_naming_an_unshared_surface_is_refused() {
        let mut state = staged();
        let verdict = state.review_remote_input(
            PEER,
            SHARED,
            InputEvent::PointerEnter { surface: PRIVATE },
        );
        assert_eq!(verdict, Err(Denial::SurfaceMismatch));
        assert!(
            state.remote_seats.seat(PEER).is_none(),
            "a refused grab still moved the peer's seat",
        );
    }

    /// The deny-biased default: a compositor that has not published a
    /// screen cannot answer where an event would land, so it refuses.
    #[test]
    fn a_compositor_that_has_published_no_screen_refuses_remote_input() {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state
            .share_registry
            .insert(PEER, SHARED, StreamingShare::new());

        let verdict =
            state.review_remote_input(PEER, SHARED, InputEvent::PointerMotion { x: 1.0, y: 1.0 });

        assert_eq!(verdict, Err(Denial::NoView));
        assert_eq!(pointer_at(&state), (0.0, 0.0));
    }
}
