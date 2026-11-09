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

impl PersonSeen {
    /// Whether this is still how `user` on `tab` looks: one person as the
    /// bar drew them against the same person now, without building the
    /// list of everyone again.
    fn matches(&self, tab: u64, user: &crate::permissions::UserId, presence: &PresenceCtx) -> bool {
        self.tab == tab
            && &self.user == user
            && self.colour == presence.color_for(user)
            && self.dims == presence.availability_of(user).dims()
            && self.agent == presence.is_agent(user)
            && self.name.as_deref() == presence.names.get(user).map(String::as_str)
    }
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
        // A selection move that has arrived is forgotten: the pills it was between stand at
        // rest, and the strip asks for no more frames for it.
        if self.selection_move.is_some_and(|(_, _, started)| {
            crate::time::now().saturating_duration_since(started) >= fade_window
                || !theme.motion().moves()
        }) {
            self.selection_move = None;
        }

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
        self.draw_shared_chip(scene, transform, tr, theme, presence);
        if self.window_controls {
            // The window's own controls: one pill, and the app's History
            // toggle standing on the window's ground before it.
            let now = crate::motion::frame_now();
            let motion = self.motion;
            for (control, _) in self.controls.iter() {
                let on =
                    self.hover_control == Some(control) || self.pressed_control == Some(control);
                self.control_lifts[control.index()].set(on, motion, now);
            }
            self.controls.paint(
                scene,
                transform,
                theme,
                crate::widget::window_controls::ControlsState {
                    hovered: self.hover_control,
                    pressed: self.pressed_control,
                    maximized: self.maximized,
                    lifts: [
                        self.control_lifts[0].level(now),
                        self.control_lifts[1].level(now),
                        self.control_lifts[2].level(now),
                    ],
                },
            );
            self.draw_history_action(scene, transform, tr, theme);
        }
        if self.window_controls {
            // A frame of a tab switch or of the 1↔2 tabs motion moves the
            // slots: follow them here, where every frame passes.
            if self.tabbed_in_flight() {
                let rect = self.toolbar_row_rect();
                self.toolbar_row.lay_out(rect);
            }
            // The app's slots change on their own (a document's edited mark
            // comes and goes with each save): a damage part held inside the
            // bar, so they repaint the bar and not the window's lane.
            let area = self.rect.union(self.toolbar_row.row()).inflate(2.0, 2.0);
            let key = crate::scene_parts::PartKey::new("ui.window-toolbar-row", &self.group_id_tag);
            let row = &mut self.toolbar_row;
            crate::scene_parts::part(
                scene,
                key,
                vec![transform.transform_rect_bbox(area)],
                |scene| {
                    scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &area);
                    row.paint(scene, transform, tr, theme, presence);
                    scene.pop_layer();
                },
            );
        }
        self.draw_closing_tabs(scene, transform, tr, theme);
        self.draw_drop_indicator(scene, transform, theme);
        self.draw_drag_ghost(scene, transform, tr, theme, presence);
    }

    /// The window's History toggle: the app's action, so it wears the app's
    /// face on the window's ground — the pill beside it is the window's.
    fn draw_history_action(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.history_action.area() <= 0.0 {
            return;
        }
        let open = self
            .history
            .get(&self.active_id)
            .is_some_and(|control| control.is_open());
        crate::widget::window_controls::paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (self.history_action, icons::Icon::History),
            crate::widget::window_controls::ButtonFace {
                hovered: self.history_action_hover,
                pressed: self.history_action_pressed,
                on: open,
                enabled: true,
                destructive: false,
                primary: false,
            },
        );
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

    /// Whether the bar's held drawing is the drawing it would make now: the
    /// bar is at rest, and nothing it draws from changed since it was made.
    /// Answers without building the list of people again.
    pub(super) fn replays(&self, transform: Affine, theme: &Theme, presence: &PresenceCtx) -> bool {
        self.painted.as_ref().is_some_and(|painted| {
            painted.epoch == self.paint_epoch
                && painted.transform == transform
                && painted.theme == *theme
                && self.at_rest()
                && self.same_people(&painted.seen, presence)
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
        // The faces the share mark shows draw as their people look now.
        self.shared.for_each_person(&self.tabs, |tab, user| {
            seen.push(PersonSeen {
                tab,
                colour: presence.color_for(user),
                dims: presence.availability_of(user).dims(),
                agent: presence.is_agent(user),
                name: presence.names.get(user).cloned(),
                user: user.clone(),
            });
        });
        seen
    }

    /// [`Widget::needs_redraw`] — the bar is clean only while the drawing it
    /// holds is the drawing it would make now — its own state unmoved
    /// ([`TabBar::touch`]), still at rest, and the same people on its tabs.
    pub(super) fn painted_bar_needs_redraw(&self) -> bool {
        self.painted.as_ref().is_none_or(|painted| {
            painted.epoch != self.paint_epoch
                || !self.at_rest()
                || crate::presence::frame::frame(|presence| {
                    !self.same_people(&painted.seen, presence)
                })
                .unwrap_or(false)
        })
    }

    /// Whether `seen` — the people a drawing was made of — are the people on
    /// the bar's tabs now. Walks the same order [`TabBar::people_seen`]
    /// builds, so it answers without building that list.
    fn same_people(&self, seen: &[PersonSeen], presence: &PresenceCtx) -> bool {
        let mut at = 0;
        for tab in &self.tabs {
            for user in presence.peers_on(&tab_presence_key(tab.id)) {
                if !seen
                    .get(at)
                    .is_some_and(|drawn| drawn.matches(tab.id, user, presence))
                {
                    return false;
                }
                at += 1;
            }
        }
        let mut same = true;
        let mut shared = 0;
        self.shared.for_each_person(&self.tabs, |tab, user| {
            same &= seen
                .get(at + shared)
                .is_some_and(|drawn| drawn.matches(tab, user, presence));
            shared += 1;
        });
        same && at + shared == seen.len()
    }

    /// [`Widget::draw`] — a bar at rest whose own state, theme, placement
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
        if self.replays(transform, theme, presence) {
            if let Some(painted) = self.painted.as_ref() {
                crate::scene_parts::append(scene, &painted.scene, painted.parts.as_ref(), None);
            }
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
        let seen = self.people_seen(presence);
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
