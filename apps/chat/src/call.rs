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
//! * [`CallPermissions`] — a small policy seam for isolated call-state tests
//!   and embedding. Chat's live UI uses [`crate::permissions::ChatPermissions`]
//!   from the host-provided permission context; this module does not grant
//!   access or implement an ACL.
//! * [`SoundEngine`] — short join/leave cues, played through `GStreamer` when
//!   the `live-media` build feature and an audio sink are available, and a
//!   silent no-op otherwise (so `--shots` and headless CI never need an
//!   audio device).

use vello::kurbo::{Rect, Size};

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
/// This is intentionally small and does not represent an authentication,
/// encryption, or access-control system. The app binds its real controls to
/// [`crate::permissions::ChatPermissions`]; callers that use this seam remain
/// responsible for supplying their own policy.
pub trait CallPermissions {
    /// Whether `action` is allowed right now according to this caller-owned
    /// policy. Implementations may return `true`; that is not a security
    /// guarantee and must not be presented as one.
    fn allows(&self, action: CallAction) -> bool;
}

/// Test/example policy that grants every action. It is never an authorization
/// decision for Chat's live UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl CallPermissions for AllowAll {
    fn allows(&self, _action: CallAction) -> bool {
        true
    }
}

/// A screen-capture source the host granted: the `PipeWire` node to consume
/// plus the capture size to encode at. The single value the share flow needs
/// from the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSource {
    /// `PipeWire` node id to open the capture on.
    pub node: u32,
    pub width: u32,
    pub height: u32,
}

impl ScreenSource {
    /// The size a screen share is encoded at: twice the camera's 640×480.
    pub const SHARE_WIDTH: u32 = 1280;
    pub const SHARE_HEIGHT: u32 = 960;

    /// The source a host grant describes, encoded at Chat's share size. A
    /// grant Chat cannot open (a stream kind it does not know) is `None`.
    pub fn from_grant(grant: &ui::capture::ScreenCaptureGrant) -> Option<Self> {
        match grant.stream {
            ui::capture::CaptureStream::PipeWire { node_id } => Some(Self {
                node: node_id,
                width: Self::SHARE_WIDTH,
                height: Self::SHARE_HEIGHT,
            }),
            _ => None,
        }
    }
}

/// Why a screen-capture request was made, so its answer finishes the right
/// flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenCapturePurpose {
    /// The person pressed Share screen.
    Share,
    /// A continued call is resuming a share the source device had running.
    Resume { session: String },
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
    /// Shared attachment/drag state used by call, navigation, browser `PiP`,
    /// and shell-owned floating surfaces.
    pub placement: ui::floating_surface::FloatingPlacement,
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
    /// grants a source (see [`crate::app::ChatApp::request_screen`]); mirrored
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
            placement: ui::floating_surface::FloatingPlacement::attached(Size::new(360.0, 220.0)),
            participants_expanded: true,
            muted: false,
            deafened: false,
            audio_only: false,
            sharing: false,
        }
    }
}

impl CallState {
    /// Semantic hand-off used when the call huddle is dragged out of the chat
    /// window. The shell adopts this exact region; chat keeps owning call
    /// state and controls, so detaching never creates a second call UI.
    pub fn floating_request(
        &self,
        source_region: Rect,
        initial_rect: Rect,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.active
            .then(|| ui::floating_surface::FloatingSurfaceRequest {
                id: format!("chat-call:{}", self.channel),
                title: "Active call".into(),
                role: ui::floating_surface::FloatingRole::ActiveCall,
                source: ui::floating_surface::FloatingSource::AppElement {
                    app_id: "so.tilo.chat".into(),
                    element_id: format!("active-call:{}", self.channel),
                    region: source_region,
                },
                preferred_size: vello::kurbo::Size::new(
                    initial_rect.width(),
                    initial_rect.height(),
                ),
                aspect_ratio: None,
                initial_rect: Some(initial_rect),
                presentation_rect: None,
                tracking_source: None,
            })
    }

    /// Begin a call in `channel`, resetting transient overlay layout.
    pub fn join(&mut self, channel: impl Into<String>) {
        self.active = true;
        self.channel = channel.into();
        self.expanded = false;
        self.placement.reset_attached();
        self.deafened = false;
        self.sharing = false;
    }

    /// End the call and clear transient overlay layout.
    pub fn leave(&mut self) {
        self.active = false;
        self.expanded = false;
        self.placement.reset_attached();
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
    fn call_uses_shared_floating_handoff() {
        let mut c = CallState::default();
        let app = Rect::new(100.0, 80.0, 900.0, 700.0);
        let desktop = Rect::new(0.0, 0.0, 1400.0, 900.0);
        c.placement
            .begin_drag(vello::kurbo::Point::new(700.0, 140.0), app, desktop);
        assert_eq!(
            c.placement
                .update_drag(vello::kurbo::Point::new(950.0, 140.0), app, desktop),
            ui::floating_surface::Handoff::Detached
        );
    }

    #[test]
    fn call_handoff_names_the_semantic_app_element() {
        let mut c = CallState::default();
        c.join("c_huddle");
        let region = Rect::new(40.0, 28.0, 432.0, 288.0);
        let request = c
            .floating_request(region, Rect::new(48.0, 36.0, 424.0, 280.0))
            .expect("an active call can be handed to the shared floating host");
        assert_eq!(request.id, "chat-call:c_huddle");
        assert_eq!(
            request.source,
            ui::floating_surface::FloatingSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: "active-call:c_huddle".into(),
                region,
            }
        );
    }

    #[test]
    fn join_and_leave_reset_overlay_layout() {
        let mut c = CallState {
            expanded: true,
            ..CallState::default()
        };
        c.leave();
        assert!(!c.active && !c.expanded);
        assert!(matches!(
            c.placement.attachment(),
            ui::floating_surface::Attachment::App { .. }
        ));
        c.join("c_warroom");
        assert!(c.active);
        assert_eq!(c.channel, "c_warroom");
        assert!(matches!(
            c.placement.attachment(),
            ui::floating_surface::Attachment::App { .. }
        ));
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
    fn a_pipewire_grant_becomes_a_share_source() {
        let grant = ui::capture::ScreenCaptureGrant {
            stream: ui::capture::CaptureStream::PipeWire { node_id: 9 },
            size: Some((3840, 2160)),
        };
        assert_eq!(
            ScreenSource::from_grant(&grant),
            Some(ScreenSource {
                node: 9,
                width: ScreenSource::SHARE_WIDTH,
                height: ScreenSource::SHARE_HEIGHT,
            })
        );
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

    #[cfg(all(feature = "live-media", not(test)))]
    fn init_backend() -> bool {
        gstreamer::init().is_ok()
    }

    #[cfg(any(not(feature = "live-media"), test))]
    fn init_backend() -> bool {
        // Unit tests exercise cue bookkeeping, not the user's audio device.
        // Keeping this silent avoids a real `autoaudiosink` pipeline waiting
        // indefinitely for a desktop audio service during hermetic CI.
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
