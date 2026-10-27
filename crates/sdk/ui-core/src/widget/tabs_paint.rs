//! Drawing a window's tab bar only when it changed: a bar at rest shows
//! its last drawing again, and a busy arc turning in it repaints only its
//! square.

#[allow(
    clippy::wildcard_imports,
    reason = "the bar's drawing reads the bar module's own types and curves"
)]
use super::*;

/// A bar's drawing at rest and what it was drawn from ([`TabBar::replays`]).
pub(super) struct PaintedBar {
    epoch: u64,
    transform: Affine,
    theme: Theme,
    seen: Vec<PersonSeen>,
    scene: Scene,
    parts: Option<crate::scene_parts::SectionParts>,
}

/// One person on one of a bar's tabs, as its pill draws them.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PersonSeen {
    tab: u64,
    user: crate::permissions::UserId,
    colour: (u8, u8, u8),
    dims: bool,
    agent: bool,
    name: Option<String>,
}

impl TabBar {
    /// [`Widget::draw`] of everything the bar shows, at `transform`.
    pub(super) fn draw_fresh(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.drag_lift_duration = theme.tokens.motion.durations.fast();
        self.busy_drawn =
            (!self.busy_since.is_empty()).then(|| self.busy_pose_at(crate::motion::frame_now()));
        // Guards one stale frame after a detach/insert mutates `tabs` outside the
        // layout pass; next `apply_layout` fixes it.
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        self.face_metrics = FaceMetrics::of(theme);
        self.record_face_demand(presence);
        self.pill_faces = (0..self.tabs.len())
            .map(|i| self.fit_pill_faces(i, presence))
            .collect();
        // Only pass with a TextRenderer — shapes every title up front.
        self.visible_titles = (0..self.tabs.len())
            .map(|i| self.visible_title(i, tr, theme))
            .collect();

        // After a reorder/insert/remove, every surviving pill interpolates from
        // its previous rect to `tab_rects[i]` on the list change's move; a brand-new
        // tab (no snapshot) renders at target immediately.
        let layout_t = self.layout_anim_started.and_then(|t| {
            let elapsed = crate::time::now().saturating_duration_since(t);
            (elapsed < layout_anim())
                .then(|| tab_progress(ListChange::MOVE, Direction::Enter, elapsed))
        });

        let fade_window = insert_fade();
        self.inserted_at.retain(|_, started| {
            crate::time::now().saturating_duration_since(*started) < fade_window
        });

        self.draw_pills(scene, transform, tr, theme, presence, layout_t, fade_window);
        self.draw_rename(scene, transform, tr, theme, presence);
        // The tab list's focus ring comes and goes with keyboard focus: a
        // damage part around the active pill, not a repaint of the bar.
        let focus = crate::scene_parts::PartKey::new("ui.tab-focus", ());
        crate::scene_parts::part_drawn(scene, focus, |scene| {
            self.draw_keyboard_focus(scene, transform, theme)
        });
        self.draw_new_tab_chip(scene, transform, tr, theme);
        self.draw_desktop_control_chip(scene, transform, tr, theme);
        self.draw_dev_build_chip(scene, transform, tr, theme);
        if self.window_controls {
            self.controls.paint(
                scene,
                transform,
                tr,
                theme,
                crate::widget::window_controls::ControlsState {
                    hovered: self.hover_control,
                    pressed: self.pressed_control,
                    maximized: self.maximized,
                    history_open: self
                        .history
                        .get(&self.active_id)
                        .is_some_and(|control| control.is_open()),
                },
            );
        }
        if self.window_controls {
            // A frame of a tab switch or of the 1↔2 tabs motion moves the
            // slots: follow them here, where every frame passes.
            if self.tabbed_in_flight() {
                let rect = self.toolbar_row_rect();
                self.toolbar_row.lay_out(rect);
            }
            self.toolbar_row
                .paint(scene, transform, tr, theme, presence);
        }
        self.draw_closing_tabs(scene, transform, tr, theme);
        self.draw_drop_indicator(scene, transform, theme);
        self.draw_drag_ghost(scene, transform, tr, theme, presence);
    }

    /// The bar's own state changed: what it draws may have.
    pub(super) fn touch(&mut self) {
        self.paint_epoch = self.paint_epoch.wrapping_add(1);
    }

    /// Whether the bar draws the same at any moment: nothing of it moves on
    /// its own (curves, a turning busy arc), and no field with a caret is
    /// edited in it.
    pub(super) fn at_rest(&self) -> bool {
        !self.needs_animation_frame()
            && self.rename.is_none()
            && !self.toolbar_row.is_editing()
            && !self.toolbar_row.popup_open()
    }

    /// The drawing to show again: the bar is at rest, and nothing it draws
    /// from changed since it was made.
    pub(super) fn replays(
        &self,
        transform: Affine,
        theme: &Theme,
        seen: &[PersonSeen],
    ) -> Option<&PaintedBar> {
        self.painted.as_ref().filter(|painted| {
            painted.epoch == self.paint_epoch
                && painted.transform == transform
                && painted.seen.as_slice() == seen
                && painted.theme == *theme
                && self.at_rest()
        })
    }

    /// The people on the bar's tabs, as its pills draw them.
    pub(super) fn people_seen(&self, presence: &PresenceCtx) -> Vec<PersonSeen> {
        let mut seen = Vec::new();
        for tab in &self.tabs {
            for user in presence.peers_on(&tab_presence_key(tab.id)) {
                seen.push(PersonSeen {
                    tab: tab.id,
                    colour: presence.color_for(user),
                    dims: presence.availability_of(user).dims(),
                    agent: presence.is_agent(user),
                    name: presence.names.get(user).cloned(),
                    user: user.clone(),
                });
            }
        }
        seen
    }

    /// [`Widget::draw`]: a bar at rest whose own state, theme, placement
    /// and the people on its tabs are as when it was last drawn shows that
    /// drawing again ([`TabBar::replays`]); anything else draws it afresh.
    pub(super) fn paint_or_replay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let seen = self.people_seen(presence);
        if let Some(painted) = self.replays(transform, theme, &seen) {
            crate::scene_parts::append(scene, &painted.scene, painted.parts.as_ref(), None);
            return;
        }
        let _drawn = tilo_resource::profiling::Phase::start("ui.tab_bar.draw");
        let mut own = Scene::new();
        let ((), parts) = crate::scene_parts::record(&mut own, |own| {
            self.draw_fresh(own, transform, tr, theme, presence);
        });
        crate::scene_parts::append(scene, &own, parts.as_ref(), None);
        #[cfg(test)]
        {
            self.fresh_draws = self.fresh_draws.saturating_add(1);
        }
        // Read after the draw: drawing settles what it measures, and the
        // pose of a busy arc it shows.
        self.painted = self.at_rest().then(|| PaintedBar {
            epoch: self.paint_epoch,
            transform,
            theme: theme.clone(),
            seen,
            scene: own,
            parts,
        });
    }

    /// A tab's content mark in its leading slot: a turning arc while busy
    /// (a still three-quarter ring under reduced motion), a speaker while it
    /// plays or is muted, lifted under the pointer since a click on it
    /// mutes or unmutes.
    pub(super) fn draw_pill_mark(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        (id, mark): (u64, TabMark),
        ink: vello::peniko::Color,
    ) {
        match mark {
            TabMark::Busy => {
                let now = crate::time::now();
                let turn = if self.busy_turns(id, now) {
                    // In indicator steps: the arc turns 30 times a second.
                    let t = crate::animation::indicator_elapsed(busy_epoch(), now).as_secs_f64();
                    t.fract() * std::f64::consts::TAU
                } else {
                    0.0
                };
                let radius = rect.width().min(rect.height()) / 2.0 - 1.5;
                let arc = vello::kurbo::Arc::new(
                    rect.center(),
                    (radius, radius),
                    turn,
                    std::f64::consts::TAU * 0.75,
                    0.0,
                );
                // Its own damage part: a turning arc repaints its square,
                // not its pill.
                let key = crate::scene_parts::PartKey::new("ui.tab-busy", id);
                let bounds = vec![transform.transform_rect_bbox(rect.inflate(2.0, 2.0))];
                crate::scene_parts::part(scene, key, bounds, |scene| {
                    scene.stroke(
                        &vello::kurbo::Stroke::new(1.5).with_caps(vello::kurbo::Cap::Round),
                        transform,
                        ink,
                        None,
                        &arc,
                    );
                });
            }
            TabMark::Playing | TabMark::Muted => {
                if rect.inflate(2.0, 2.0).contains(self.last_cursor) {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme
                            .tokens
                            .colors
                            .lift_on(theme.tokens.colors.surfaces.content, 1.0),
                        None,
                        &RoundedRect::from_rect(
                            rect.inflate(2.0, 2.0),
                            f64::from(theme.tokens.radii.item),
                        ),
                    );
                }
                let icon = if mark == TabMark::Muted {
                    icons::Icon::VolumeX
                } else {
                    icons::Icon::Volume2
                };
                icons::draw_icon(
                    scene,
                    transform,
                    rect,
                    icon,
                    ink,
                    icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "tabs_paint_tests.rs"]
mod tests;
