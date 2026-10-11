//! Damage for the collaborators an app window shows.
//!
//! Presence reaches a window's pixels two ways, and each gets the damage it
//! can prove:
//!
//! - **Over the widgets**, the framework paints peer pointers, their name
//!   pills and edge mist, and semantic action activity. [`PresenceLayer`]
//!   records that layer as keyed [`scene_parts`](crate::scene_parts) and
//!   compares it with the layer on screen, so a peer's pointer moving
//!   damages where it was and where it is, and a pointer that sits still
//!   damages nothing.
//! - **Inside the widgets**, carets, selections, typing marks, focus rings
//!   and presence dots are painted by whichever widget owns the content. No
//!   widget reports damage for them, so a change to anything a widget can
//!   paint from ([`WidgetPresence`]) repaints the window, exactly as before.
//!   While widgets show marks placed from their content (a caret and its
//!   name chip), a frame in which any widget changed repaints the window
//!   too ([`WidgetPresence::marks_follow_content`]).
//!
//! Peer pointers are the frequent case (a moving pointer publishes at
//! ~30 Hz), and those frames keep to the pointer's own pixels.

use std::collections::{HashMap, HashSet};

use vello::Scene;
use vello::kurbo::Rect;

use super::{PresenceCtx, PresenceKey, SelectionRange, TypingMark};
use crate::permissions::UserId;
use crate::scene_parts::SectionParts;
use crate::time::Instant;

/// Everything in a [`PresenceCtx`] a widget may paint from, as of one frame.
///
/// Peer pointers are left out on purpose: the window paints them over its
/// widgets through a [`PresenceLayer`], which bounds their damage itself.
/// A widget painting from [`PresenceCtx::remote_cursors`] must report its
/// own damage when they change.
#[derive(Clone, Debug, PartialEq)]
pub struct WidgetPresence {
    here: HashMap<PresenceKey, Vec<UserId>>,
    selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
    following: HashMap<(PresenceKey, UserId), UserId>,
    color_overrides: HashMap<UserId, (u8, u8, u8)>,
    names: HashMap<UserId, String>,
    participant_kinds: HashMap<UserId, super::ParticipantKind>,
    statuses: HashMap<UserId, crate::live_collaboration::LiveStatus>,
    typing: HashMap<(UserId, PresenceKey), Vec<TypingMark>>,
    views: HashMap<UserId, crate::view::ViewSubset>,
    own: HashSet<UserId>,
    self_participant: Option<(UserId, String)>,
    surface: Option<String>,
    /// A typing mark is fading or semantic activity is on screen: what
    /// widgets paint from them changes with the clock, not with this state.
    animating: bool,
}

impl WidgetPresence {
    /// Whether widgets paint marks placed from their own content: a peer's
    /// caret, selection or fresh typing, or semantic activity. Each can
    /// carry a name chip hanging outside the widget, and a local change to
    /// the content moves it without any widget reporting where it was.
    pub fn marks_follow_content(&self) -> bool {
        self.animating
            || self.carets.values().any(|list| !list.is_empty())
            || self.selections.values().any(|list| !list.is_empty())
            || self.typing.values().any(|marks| !marks.is_empty())
    }
}

impl PresenceCtx {
    /// What widgets may paint from at `now`; see [`WidgetPresence`].
    pub fn widget_presence(&self, now: Instant) -> WidgetPresence {
        WidgetPresence {
            here: self.here.clone(),
            selections: self.selections.clone(),
            carets: self.carets.clone(),
            following: self.following.clone(),
            color_overrides: self.color_overrides.clone(),
            names: self.names.clone(),
            participant_kinds: self.participant_kinds.clone(),
            statuses: self.statuses.clone(),
            typing: self.typing.clone(),
            views: self.views.clone(),
            own: self.own.clone(),
            self_participant: self.self_participant.clone(),
            surface: self.surface.clone(),
            animating: self.has_active_typing(now) || self.has_visible_interactions(now),
        }
    }
}

/// The presence a window paints over its widgets, kept from frame to frame
/// so the next frame damages only what changed. Retains draw caches only.
///
/// Per frame: [`Self::widgets_need_full_repaint`] for the presence the
/// widgets were drawn from, then [`Self::record`] the layer, append
/// [`Self::scene`] over the widgets and add [`Self::damage`]; once the frame
/// is submitted, [`Self::presented`].
#[derive(Default)]
pub struct PresenceLayer {
    /// The layer the last submitted frame showed, with its parts.
    shown: Scene,
    shown_parts: Option<SectionParts>,
    shown_widgets: Option<WidgetPresence>,
    /// This frame's layer, until it is submitted.
    next: Scene,
    next_parts: Option<SectionParts>,
    next_widgets: Option<WidgetPresence>,
}

impl PresenceLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the widgets' presence differs from the last submitted
    /// frame's, or is animating. Widgets do not report damage for the
    /// carets, selections and marks they paint, so the window must repaint.
    pub fn widgets_need_full_repaint(&mut self, presence: &PresenceCtx, now: Instant) -> bool {
        let widgets = presence.widget_presence(now);
        let full = widgets.animating || self.shown_widgets.as_ref() != Some(&widgets);
        self.next_widgets = Some(widgets);
        full
    }

    /// Whether this frame's widgets paint marks placed from their content
    /// ([`WidgetPresence::marks_follow_content`]). `true` until
    /// [`Self::widgets_need_full_repaint`] has seen the frame.
    pub fn widgets_place_content_marks(&self) -> bool {
        self.next_widgets
            .as_ref()
            .is_none_or(WidgetPresence::marks_follow_content)
    }

    /// Record this frame's layer. `draw` paints into an empty scene and must
    /// put every command in a [`scene_parts::part`](crate::scene_parts::part)
    /// with honest bounds, the same parts in the same order every frame.
    pub fn record(&mut self, draw: impl FnOnce(&mut Scene)) {
        let ((), parts) = crate::scene_parts::record(&mut self.next, |scene| {
            scene.reset();
            draw(scene);
        });
        self.next_parts = parts;
    }

    /// This frame's layer, to append over the widgets.
    pub fn scene(&self) -> &Scene {
        &self.next
    }

    /// Whether the layer on screen or this frame's paints anything over the
    /// widgets.
    pub fn paints(&self) -> bool {
        !self.shown.encoding().draw_tags.is_empty() || !self.next.encoding().draw_tags.is_empty()
    }

    /// Damage, in the layer's scene coordinates, between the layer on
    /// screen and this frame's: the old and new bounds of every part that
    /// changed. `None` when that cannot be proven (no frame shown yet, a
    /// part left unbounded, commands outside every part), and the window
    /// must repaint.
    pub fn damage(&self) -> Option<Vec<Rect>> {
        crate::scene_parts::damage(
            &self.shown,
            self.shown_parts.as_ref()?,
            &self.next,
            self.next_parts.as_ref()?,
        )
        .ok()
    }

    /// The frame recorded since the last call was submitted: it is what the
    /// next frame's damage is measured against.
    pub fn presented(&mut self) {
        std::mem::swap(&mut self.shown, &mut self.next);
        self.shown_parts = self.next_parts.take();
        if let Some(widgets) = self.next_widgets.take() {
            self.shown_widgets = Some(widgets);
        }
        self.next.reset();
    }
}
