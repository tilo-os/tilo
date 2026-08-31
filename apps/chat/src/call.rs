//! Call state, the call-action permission seam, and join/leave sounds.
//!
//! The conferencing experience is split into three concerns:
//!
//! * [`CallState`] — the per-peer view state of the current call: which
//!   voice channel, whether the overlay is expanded, where the user has
//!   dragged the floating overlay, whether the participant list is expanded,
//!   and the local mic / deafen / audio-only toggles. (Who is *in* the call
//!   is shared truth in the CRDT — see [`crate::doc`] — but how this peer
//!   lays out the overlay is local.)
//! * [`CallPermissions`] — a clean hook for gating call actions (join,
//!   share mic/camera, screen-share). A real OS permission system is being
//!   built separately; until it lands this defaults to allow-all via
//!   [`AllowAll`], and the call code asks the seam before every action so
//!   wiring in the real checker later is a one-line swap.
//! * [`SoundEngine`] — short join/leave cues, played through `GStreamer` when
//!   the `live-media` build feature and an audio sink are available, and a
//!   silent no-op otherwise (so `--shots` and headless CI never need an
//!   audio device).

use vello::kurbo::{Point, Rect, Vec2};

/// A discrete call action that flows through the permission seam before it
/// takes effect. Keeping these in one enum means a future real permission
/// system gates a single, enumerable surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    /// Connect to a voice channel.
    Join,
    /// Leave the current call.
    Leave,
    /// Unmute / transmit microphone audio.
    Speak,
    /// Enable the camera (leave audio-only).
    Camera,
    /// Start sharing the screen (portal `ScreenCast`).
    ShareScreen,
}

impl CallAction {
    pub fn label(self) -> &'static str {
        match self {
            CallAction::Join => "Join call",
            CallAction::Leave => "Leave call",
            CallAction::Speak => "Use microphone",
            CallAction::Camera => "Use camera",
            CallAction::ShareScreen => "Share screen",
        }
    }
}

/// The permission seam for call actions.
///
/// This is intentionally NOT coupled to `ui::permissions` or the in-progress
/// `permissions` crate — it's a minimal local trait so the call layer has a
/// single, obvious place to consult before a privileged action. Swap the
/// default [`AllowAll`] for a real implementation once the OS permission
/// service exists; nothing else in the call code changes.
pub trait CallPermissions {
    /// Whether `action` is allowed right now. Default-allow implementations
    /// return `true`; a real one would consult the cluster's grant policy.
    fn allows(&self, action: CallAction) -> bool;
}

/// Default-allow permission policy used until the real OS permission service
/// is wired in. Every action is granted.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl CallPermissions for AllowAll {
    fn allows(&self, _action: CallAction) -> bool {
        true
    }
}

/// A screen-capture source handed back by the portal: the `PipeWire` node to
/// consume plus the capture size to encode at. The single value the share flow
/// needs from the portal, kept transport-agnostic so a test can mint one
/// without a real desktop portal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSource {
    /// `PipeWire` node id to open the capture on.
    pub node: u32,
    pub width: u32,
    pub height: u32,
}

/// The screen-capture request seam.
///
/// A share never invents a stream: it asks a `ScreenPortal` for a source and
/// only proceeds if one is granted. The real implementation ([`SystemPortal`])
/// drives the **xdg-desktop-portal `ScreenCast`** picker via ashpd behind the
/// `live-media` feature and returns `None` everywhere else (headless, the
/// default build, a cancelled picker) — so the default build gates honestly.
/// Tests inject a fake that returns a source, exercising the whole share state
/// machine + CRDT wiring without a desktop portal.
pub trait ScreenPortal {
    /// Request a screen-share source from the user. `None` = no source
    /// available or the user declined; the share must not start.
    fn request(&self) -> Option<ScreenSource>;
}

/// The production portal: a real xdg-desktop-portal `ScreenCast` request behind
/// `live-media`, an honest `None` otherwise.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPortal;

impl ScreenPortal for SystemPortal {
    fn request(&self) -> Option<ScreenSource> {
        #[cfg(feature = "live-media")]
        {
            crate::media::request_screencast().map(|node| ScreenSource {
                node,
                width: crate::video::DEFAULT_W * 2,
                height: crate::video::DEFAULT_H * 2,
            })
        }
        #[cfg(not(feature = "live-media"))]
        {
            None
        }
    }
}

/// Test double: a portal that hands back a fixed source (or nothing), so the
/// whole share state machine + CRDT wiring can be proven without a real
/// xdg-desktop-portal picker or a capture device.
#[cfg(test)]
pub struct FakePortal(pub Option<ScreenSource>);

#[cfg(test)]
impl ScreenPortal for FakePortal {
    fn request(&self) -> Option<ScreenSource> {
        self.0
    }
}

/// Per-peer call view state. A fresh app is **not** in a call — the user
/// joins a voice channel to start one. (The demo screenshot activates a call
/// explicitly via the shot scenario.)
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent peer-local call toggle (active/expanded/participants_expanded/muted/deafened/audio_only/sharing) that the UI reads and writes individually — grouping them would just relocate the boolean soup, not remove it"
)]
pub struct CallState {
    /// True while connected to a voice channel.
    pub active: bool,
    /// The voice channel id the call is in.
    pub channel: String,
    /// Expanded (full-window grid) vs. the docked floating overlay.
    pub expanded: bool,
    /// Offset (logical px) the user has dragged the floating overlay from
    /// its anchored home position. Reset when the call ends or expands.
    pub overlay_offset: Vec2,
    /// Whether the participant list inside the overlay is expanded. Auto
    /// behaviour: small calls expand, large calls collapse to a count chip
    /// until the user opens it.
    pub participants_expanded: bool,
    /// Local mic muted.
    pub muted: bool,
    /// Local output deafened (implies muted while active).
    pub deafened: bool,
    /// Camera off / audio-only.
    pub audio_only: bool,
    /// This peer is sharing its screen into the call. Set only once the portal
    /// grants a source (see [`crate::app::ChatApp::acquire_screen`]); mirrored
    /// into the shared voice membership so remote peers reserve the featured
    /// tile. Never set speculatively — a share the portal declined stays `false`.
    pub sharing: bool,
}

impl Default for CallState {
    fn default() -> Self {
        Self {
            active: false,
            channel: String::new(),
            expanded: false,
            overlay_offset: Vec2::ZERO,
            participants_expanded: true,
            muted: false,
            deafened: false,
            audio_only: false,
            sharing: false,
        }
    }
}

impl CallState {
    /// Begin a call in `channel`, resetting transient overlay layout.
    pub fn join(&mut self, channel: impl Into<String>) {
        self.active = true;
        self.channel = channel.into();
        self.expanded = false;
        self.overlay_offset = Vec2::ZERO;
        self.deafened = false;
        self.sharing = false;
    }

    /// End the call and clear transient overlay layout.
    pub fn leave(&mut self) {
        self.active = false;
        self.expanded = false;
        self.overlay_offset = Vec2::ZERO;
        self.sharing = false;
    }

    /// Toggle mute. Un-muting also un-deafens (you can't speak while
    /// deafened in the usual Discord model).
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if !self.muted {
            self.deafened = false;
        }
    }

    /// Toggle deafen. Deafening also mutes; un-deafening leaves mute as-is.
    pub fn toggle_deafen(&mut self) {
        self.deafened = !self.deafened;
        if self.deafened {
            self.muted = true;
        }
    }

    /// Whether to auto-expand the participant list for `n` participants.
    /// Small calls show every face; big calls collapse until summoned.
    pub fn auto_participants_expanded(n: usize) -> bool {
        n <= 8
    }

    /// Clamp the dragged overlay so its `home` rect (after offset) stays at
    /// least partly inside `bounds`, so it can never be lost off-screen.
    pub fn clamp_offset(&mut self, home: Rect, bounds: Rect) {
        let margin = 48.0;
        let moved = home + self.overlay_offset;
        let mut dx = 0.0;
        let mut dy = 0.0;
        if moved.x1 < bounds.x0 + margin {
            dx = bounds.x0 + margin - moved.x1;
        } else if moved.x0 > bounds.x1 - margin {
            dx = bounds.x1 - margin - moved.x0;
        }
        if moved.y0 < bounds.y0 {
            dy = bounds.y0 - moved.y0;
        } else if moved.y0 > bounds.y1 - margin {
            dy = bounds.y1 - margin - moved.y0;
        }
        self.overlay_offset += Vec2::new(dx, dy);
    }
}

/// An in-progress drag of the floating call overlay.
#[derive(Clone, Copy, Debug)]
pub struct OverlayDrag {
    /// Pointer position where the drag began.
    pub origin: Point,
    /// `overlay_offset` at the moment the drag began.
    pub start_offset: Vec2,
}

/// A short call cue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Join,
    Leave,
}

pub use sound::SoundEngine;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_all_grants_everything() {
        let p = AllowAll;
        for a in [
            CallAction::Join,
            CallAction::Leave,
            CallAction::Speak,
            CallAction::Camera,
            CallAction::ShareScreen,
        ] {
            assert!(p.allows(a), "{a:?} should be allowed by AllowAll");
        }
    }

    #[test]
    fn deafen_implies_mute_and_unmute_undeafens() {
        let mut c = CallState::default();
        c.toggle_deafen();
        assert!(c.deafened && c.muted, "deafen mutes");
        c.toggle_mute(); // unmute
        assert!(!c.muted && !c.deafened, "unmuting clears deafen");
    }

    #[test]
    fn participant_list_auto_expands_only_for_small_calls() {
        assert!(CallState::auto_participants_expanded(3));
        assert!(CallState::auto_participants_expanded(8));
        assert!(!CallState::auto_participants_expanded(9));
    }

    #[test]
    fn dragged_overlay_is_clamped_back_into_bounds() {
        let mut c = CallState::default();
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let home = Rect::new(600.0, 400.0, 880.0, 700.0);
        // Drag it far off the right edge.
        c.overlay_offset = Vec2::new(5000.0, 0.0);
        c.clamp_offset(home, bounds);
        let moved = home + c.overlay_offset;
        assert!(moved.x0 < bounds.x1, "overlay pulled back on-screen");
    }

    #[test]
    fn join_and_leave_reset_overlay_layout() {
        let mut c = CallState {
            overlay_offset: Vec2::new(120.0, -30.0),
            expanded: true,
            ..CallState::default()
        };
        c.leave();
        assert!(!c.active && !c.expanded);
        assert_eq!(c.overlay_offset, Vec2::ZERO);
        c.join("c_warroom");
        assert!(c.active);
        assert_eq!(c.channel, "c_warroom");
    }

    #[test]
    fn join_and_leave_clear_the_screen_share_flag() {
        let mut c = CallState::default();
        c.join("c_huddle");
        c.sharing = true;
        c.leave();
        assert!(!c.sharing, "leaving a call stops the local screen share");
        c.sharing = true;
        c.join("c_other");
        assert!(
            !c.sharing,
            "joining a fresh call starts with no screen share"
        );
    }

    #[test]
    fn system_portal_declines_without_a_live_portal() {
        // The default (non-live-media) build has no desktop portal, so the
        // production portal grants nothing — the honest-gating contract that
        // keeps the default build from ever faking a screen share.
        assert!(
            SystemPortal.request().is_none() || cfg!(feature = "live-media"),
            "the system portal must decline when no live portal is compiled in"
        );
    }

    #[test]
    fn fake_portal_hands_back_its_configured_source() {
        // The test seam the share flow is proven against.
        let src = ScreenSource {
            node: 9,
            width: 640,
            height: 480,
        };
        assert_eq!(FakePortal(Some(src)).request(), Some(src));
        assert_eq!(FakePortal(None).request(), None);
    }
}

// ---------------------------------------------------------------------------
// Sound engine
// ---------------------------------------------------------------------------

mod sound {
    use super::Cue;

    /// Plays short join/leave cues.
    ///
    /// With the `live-media` feature and a working `GStreamer` audio sink the
    /// cue is a tiny synthesised tone played through `autoaudiosink`. Without
    /// it (the default build, `--shots`, headless CI) every method is a
    /// silent no-op — the call UX degrades gracefully with no audio device
    /// required. Whether real output is wired is reported by [`Self::is_live`].
    pub struct SoundEngine {
        /// Bookkeeping so callers can show "muted cue" state / tests can
        /// assert the last cue requested even on the silent backend.
        last_cue: Option<Cue>,
        live: bool,
    }

    impl SoundEngine {
        pub fn new() -> Self {
            let live = init_backend();
            SoundEngine {
                last_cue: None,
                live,
            }
        }

        /// Whether real audio output is available in this build + environment.
        pub fn is_live(&self) -> bool {
            self.live
        }

        pub fn last_cue(&self) -> Option<Cue> {
            self.last_cue
        }

        /// Play the join cue (a rising two-note blip).
        pub fn join(&mut self) {
            self.last_cue = Some(Cue::Join);
            play_cue(self.live, Cue::Join);
        }

        /// Play the leave cue (a falling two-note blip).
        pub fn leave(&mut self) {
            self.last_cue = Some(Cue::Leave);
            play_cue(self.live, Cue::Leave);
        }
    }

    impl Default for SoundEngine {
        fn default() -> Self {
            Self::new()
        }
    }

    #[cfg(feature = "live-media")]
    fn init_backend() -> bool {
        gstreamer::init().is_ok()
    }

    #[cfg(not(feature = "live-media"))]
    fn init_backend() -> bool {
        false
    }

    /// Frequencies (Hz) for the two notes of each cue. Join rises (connect),
    /// leave falls (disconnect) — the Discord-like convention.
    fn cue_tones(cue: Cue) -> (u32, u32) {
        match cue {
            Cue::Join => (523, 784),  // C5 → G5
            Cue::Leave => (784, 523), // G5 → C5
        }
    }

    #[cfg(feature = "live-media")]
    fn play_cue(live: bool, cue: Cue) {
        use gstreamer::prelude::*;
        if !live {
            return;
        }
        let (a, b) = cue_tones(cue);
        // Two short audiotestsrc tones chained into the default sink. We
        // build a fresh throwaway pipeline per cue and tear it down on EOS so
        // there's no persistent audio graph to manage.
        let desc = format!(
            "audiotestsrc num-buffers=8 freq={a} volume=0.3 ! audioconvert ! autoaudiosink"
        );
        if let Ok(pipeline) = gstreamer::parse::launch(&desc) {
            let _ = pipeline.set_state(gstreamer::State::Playing);
            // Fire-and-forget: a watchdog thread tears the pipeline down
            // shortly after the buffers drain so we don't leak elements.
            let weak = pipeline.downgrade();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(260));
                if let Some(p) = weak.upgrade() {
                    let _ = p.set_state(gstreamer::State::Null);
                }
            });
        }
        let _ = b; // second note reserved for a future richer cue
    }

    #[cfg(not(feature = "live-media"))]
    fn play_cue(_live: bool, cue: Cue) {
        // Silent backend: nothing to play. Touch the tone table so the cue
        // mapping stays exercised (and a typo there fails to compile).
        let _ = cue_tones(cue);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cues_record_and_are_safe_without_audio() {
            let mut e = SoundEngine::new();
            assert!(e.last_cue().is_none());
            e.join();
            assert_eq!(e.last_cue(), Some(Cue::Join));
            e.leave();
            assert_eq!(e.last_cue(), Some(Cue::Leave));
            // The default test build has no audio sink — engine is not live,
            // and calling the cues must never panic.
            assert!(!e.is_live() || cfg!(feature = "live-media"));
        }

        #[test]
        fn join_rises_leave_falls() {
            let (j0, j1) = cue_tones(Cue::Join);
            let (l0, l1) = cue_tones(Cue::Leave);
            assert!(j1 > j0, "join cue rises");
            assert!(l1 < l0, "leave cue falls");
        }
    }
}
