//! Call state and the call-action permission seam.
//!
//! The conferencing experience is split into two concerns:
//!
//! * [`CallState`] — the per-peer view state of the current call: which
//!   voice channel, whether the overlay is expanded, where the user has
//!   dragged the floating overlay, whether the participant list is expanded,
//!   and local presentation controls. Current stream state comes from the
//!   native service; durable voice rows are replayable history.
//! * [`CallAction`] — the enumerable surface of call actions. Chat's live UI
//!   gates them through [`crate::permissions::ChatPermissions`] from the
//!   host-provided permission context; this module does not grant access or
//!   implement an ACL.

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

/// Per-peer call view state. A fresh app is **not** in a call — the user
/// joins a voice channel to start one. (The demo screenshot activates a call
/// explicitly via the shot scenario.)
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent peer-local call toggle (active/expanded/participants_expanded/muted/deafened/audio_only/sharing) that the UI reads and writes individually — grouping them would just relocate the boolean soup, not remove it"
)]
pub struct CallState {
    /// The call panel is open for an explicit local join attempt. This is
    /// view state, never connection readiness or capture authority.
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
    /// This peer is sharing its screen into the call. Chat's native voice path
    /// has no local screen-capture producer, so production never sets it; it
    /// is cleared on every join/leave and staged only by screenshot scenarios.
    pub sharing: bool,
    /// Who the docked call shows large: the last other person the call
    /// heard, kept until someone else speaks, so the tile does not flip
    /// back and forth between two people taking turns.
    pub featured: Option<String>,
}

impl Default for CallState {
    fn default() -> Self {
        Self {
            active: false,
            channel: String::new(),
            expanded: false,
            placement: ui::floating_surface::FloatingPlacement::attached(Size::new(360.0, 220.0)),
            participants_expanded: true,
            muted: true,
            deafened: false,
            audio_only: true,
            sharing: false,
            featured: None,
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
                title: crate::i18n::t("chat-call-active"),
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
        self.muted = true;
        self.audio_only = true;
        self.sharing = false;
        self.featured = None;
    }

    /// End the call and clear transient overlay layout.
    pub fn leave(&mut self) {
        self.active = false;
        self.expanded = false;
        self.placement.reset_attached();
        self.sharing = false;
        self.muted = true;
        self.audio_only = true;
        self.featured = None;
    }

    /// Whether to auto-expand the participant list for `n` participants.
    /// Small calls show every face; big calls collapse until summoned.
    pub fn auto_participants_expanded(n: usize) -> bool {
        n <= 8
    }
}

/// Each call participant's speaking ring, following who the call hears:
/// a ring is there the moment someone is heard and eases out when they
/// pause ([`ui::motion::Speaking`]). Settled quiet rings are dropped, so a
/// silent call costs nothing.
#[derive(Debug, Default)]
pub struct SpeakingRings {
    rings: std::collections::BTreeMap<String, ui::motion::Speaking>,
    heard: std::collections::BTreeSet<String>,
    /// Some ring is still easing: the next frame draws it further.
    pub animating: bool,
}

impl SpeakingRings {
    /// Follow `heard`, the people the call hears now, at `now`.
    pub fn reconcile(
        &mut self,
        heard: std::collections::BTreeSet<String>,
        motion: ui::tokens::EffectiveMotion,
        now: ui::time::Instant,
    ) {
        if heard != self.heard {
            for person in heard.difference(&self.heard) {
                self.rings
                    .entry(person.clone())
                    .or_default()
                    .set(true, motion, now);
            }
            for person in self.heard.difference(&heard) {
                if let Some(ring) = self.rings.get_mut(person) {
                    ring.set(false, motion, now);
                }
            }
            self.heard = heard;
        }
        let heard = &self.heard;
        self.rings
            .retain(|person, ring| heard.contains(person) || ring.is_moving(now));
        self.animating = self.rings.values().any(|ring| ring.is_moving(now));
    }

    /// How strongly `person`'s ring shows at `now`, `0..=1`.
    pub fn level(&self, person: &str, now: ui::time::Instant) -> f32 {
        self.rings.get(person).map_or(0.0, |ring| ring.level(now))
    }

    /// Everyone's rings gone at once, as when the call ends.
    pub fn clear(&mut self) {
        self.rings.clear();
        self.heard.clear();
        self.animating = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_rings_while_its_person_is_heard_and_settles_to_nothing() {
        let motion = ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false);
        let now = ui::time::now();
        let mut rings = SpeakingRings::default();
        rings.reconcile(["ada".to_owned()].into(), motion, now);
        assert_eq!(rings.level("ada", now), 1.0, "heard: ringed at once");
        assert_eq!(rings.level("bo", now), 0.0);
        rings.reconcile(std::collections::BTreeSet::new(), motion, now);
        assert!(rings.animating, "a pause eases out");
        let later = now + std::time::Duration::from_secs(1);
        rings.reconcile(std::collections::BTreeSet::new(), motion, later);
        assert_eq!(rings.level("ada", later), 0.0);
        assert!(!rings.animating, "a quiet call costs no frames");
    }

    #[test]
    fn native_voice_starts_unjoined_and_each_join_is_muted_audio_only() {
        let mut state = CallState::default();
        assert!(!state.active);
        assert!(state.muted);
        assert!(state.audio_only);
        state.join("first");
        state.muted = false;
        state.audio_only = false;
        state.join("second");
        assert!(state.muted);
        assert!(state.audio_only);
        state.muted = false;
        state.leave();
        assert!(!state.active);
        assert!(state.muted);
    }

    #[test]
    fn participant_list_auto_expands_only_for_small_calls() {
        assert!(CallState::auto_participants_expanded(3));
        assert!(CallState::auto_participants_expanded(8));
        assert!(!CallState::auto_participants_expanded(9));
    }

    #[test]
    fn join_leave_and_deafen_reset_audio_overlay_and_share_state() {
        // Phase: join_and_leave_reset_overlay_layout.
        {
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
        // Phase: join_and_leave_clear_the_screen_share_flag.
        {
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
    }

    #[test]
    fn call_hands_off_to_the_shared_floating_host_as_a_semantic_element() {
        // Phase: call_uses_shared_floating_handoff.
        {
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
        // Phase: call_handoff_names_the_semantic_app_element.
        {
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
    }
}
