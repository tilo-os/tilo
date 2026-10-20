//! Following a peer: reconciliation, the follow chip and peer cursors.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// While following someone: a 2 px frame in their colour around the
    /// work area, and a small bar at the top centre with their face,
    /// "Following Ada" and Stop (Escape stops too). When the person's own
    /// input ends it, the bar says "Stopped following" for a moment.
    pub(super) fn draw_follow_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let stopped = self
            .follow_stopped
            .filter(|at| now.saturating_duration_since(*at) < FOLLOW_STOPPED_FOR);
        if stopped.is_none() {
            self.follow_stopped = None;
        }
        if !self.follow_transition.is_visible(now) && stopped.is_none() {
            self.follow_chip_rect = Rect::ZERO;
            self.follow_stop_rect = Rect::ZERO;
            self.follow_visual = None;
            return;
        }
        let Some(row) = self
            .followed_roster_row()
            .or_else(|| self.follow_visual.clone())
        else {
            self.set_followed_peer(None);
            return;
        };
        self.follow_visual = Some(row.clone());
        let following = self.followed_peer.is_some();
        let label = if following {
            t_args("follow-label", &[("person", row.label.as_str().into())])
        } else {
            t("follow-stopped")
        };
        let area = self.overlay_area();
        let bar_top = area.y0 + f64::from(tilo_desktop::topbar::topbar_height(theme));
        let openness = if following {
            ui::motion::unit_opacity(self.follow_transition.progress(now))
        } else {
            stopped.map_or(0.0, |at| {
                ui::motion::Transient::opacity(
                    ui::motion::Transient::CONFIRMATION,
                    (at, at.checked_add(FOLLOW_STOPPED_FOR).unwrap_or(at)),
                    now,
                    self.motion.get(),
                )
            })
        };
        // The frame says whose view this is, all the way round.
        if following {
            ui::presence::paint_follow_frame(
                scene,
                transform,
                Rect::new(area.x0, bar_top, area.x1, area.y1),
                row.color,
                openness,
                theme,
            );
        }
        let stop_label = t("follow-stop");
        let (chip, stop) = Self::follow_chip_rects(
            text_renderer,
            theme,
            (&label, &stop_label),
            Point::new(area.center().x, bar_top),
            following,
        );
        self.follow_chip_rect = chip;
        self.follow_stop_rect = if following { stop } else { Rect::ZERO };
        // The bar comes down from the top bar it hangs under.
        let pose = ui::motion::Appear::pose(
            f64::from(openness),
            Point::new(chip.center().x, chip.y0),
            self.motion.get(),
        );
        let placed = pose.applied_to(transform);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity.min(openness),
            placed,
            &ui::widget::floating_surface::painted_bounds(chip, theme),
        );
        ui::widget::floating_surface::paint_card(scene, placed, chip, theme);
        self.paint_follow_chip_contents(
            &mut ui::paint::PaintCtx::new(scene, placed, text_renderer, theme),
            &row,
            (&label, &stop_label),
            (chip, stop),
            following,
        );
        scene.pop_layer();
    }

    /// Where the follow bar and its Stop go, the bar centred on `top`'s x
    /// just under it: their face, `label` and, while following, Stop.
    fn follow_chip_rects(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (label, stop_label): (&str, &str),
        top: Point,
        following: bool,
    ) -> (Rect, Rect) {
        let spacing = &theme.tokens.spacing;
        let face = f64::from(theme.tokens.metrics.avatar_inline);
        let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let (label_w, _) = text_renderer.measure_style(label, None, &label_style);
        let (stop_w, _) =
            text_renderer.measure_style(stop_label, None, &theme.text(TextRole::Body));
        let pad = f64::from(spacing.s3);
        let stop_width = if following {
            f64::from(stop_w) + 2.0 * f64::from(spacing.s3)
        } else {
            0.0
        };
        let width = pad
            + face
            + f64::from(spacing.s2)
            + f64::from(label_w)
            + if following {
                f64::from(spacing.s3) + stop_width
            } else {
                0.0
            }
            + pad;
        let y0 = top.y + f64::from(spacing.s3);
        let chip = Rect::new(top.x - width / 2.0, y0, top.x + width / 2.0, y0 + 40.0);
        let stop = Rect::new(
            chip.x1 - pad - stop_width,
            chip.center().y - 14.0,
            chip.x1 - pad,
            chip.center().y + 14.0,
        );
        (chip, stop)
    }

    /// The follow bar's contents: their face, `label` and, while following,
    /// Stop.
    fn paint_follow_chip_contents(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        row: &RosterRow,
        (label, stop_label): (&str, &str),
        (chip, stop): (Rect, Rect),
        following: bool,
    ) {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s3);
        let face = f64::from(theme.tokens.metrics.avatar_inline);
        let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let (_, label_h) = ctx.text.measure_style(label, None, &label_style);
        let mut their_face = ui::presence::Face::new(&row.label, &row.id, row.color);
        their_face.is_agent = row.is_agent;
        ui::presence::FaceStack {
            side: face - 6.0,
            ..ui::presence::FaceStack::BAR
        }
        .paint(
            ctx,
            (chip.x0 + pad + face, chip.center().y),
            (&[their_face], 0),
            ui::widget::floating_surface::card_ground(theme),
        );
        ctx.text(
            label,
            &label_style,
            theme.tokens.colors.text_primary,
            Point::new(
                chip.x0 + pad + face + f64::from(spacing.s2),
                chip.center().y - f64::from(label_h) / 2.0,
            ),
            None,
        );
        if following {
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &ButtonFace::new(stop, stop_label)
                    .role(ui::widget::ButtonRole::Plain)
                    .hovered(self.last_cursor.is_some_and(|p| stop.contains(p))),
            );
        }
    }

    /// Paints every real peer's live pointer for the active workspace, via
    /// the same [`ui::presence::draw_peer_cursors`] call an app makes for
    /// peers inside its own window. One call for the whole set, since
    /// names have to be arranged against each other.
    ///
    /// One damage part ([`ui::scene_parts`]) bounded by what it paints: a
    /// peer's pointer moving repaints where it was and where it is, not the
    /// desktop.
    pub(super) fn draw_real_peers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let peers = self.workspace_peers(presence, theme.reduce_motion);
        let cursors = self.painted_person_cursors(presence, theme, &peers);
        self.follow_person_composer_anchor(&cursors);
        self.person_markers
            .publish(cursors, &t("person-quick-message"));
        let key = ui::scene_parts::PartKey::new("shell.peer-presence", ());
        ui::scene_parts::part_drawn(scene, key, |scene| {
            let mut painted = ui::presence::draw_peer_presence(
                scene,
                transform,
                self.rect,
                text_renderer,
                theme,
                &peers,
            );
            painted.extend(self.draw_follow_cursor_affordances(
                scene,
                transform,
                presence,
                theme.reduce_motion,
            ));
            painted.extend(self.draw_cursor_chat(scene, transform, text_renderer, theme));
            painted
        });
    }

    /// Rebuild the direct-manipulation targets over remote cursor anchors and
    /// paint a quiet orbital ring. The ring is an action signifier, not a
    /// label: click it to attach to the person already visible at that work.
    ///
    /// Returns the bounds, in scene coordinates, of what it painted.
    pub(super) fn draw_follow_cursor_affordances(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        presence: &PresenceCtx,
        reduce_motion: bool,
    ) -> Vec<Rect> {
        self.follow_cursor_targets.clear();
        let mut painted = Vec::new();
        let anchors = self.workspace_follow_anchors(presence, reduce_motion);
        // Following shows as the frame in their colour and the follow bar,
        // nothing else: no line from anyone's pointer to anyone else's runs
        // across the work, neither from this person's own pointer to whom
        // they follow nor between other people following each other (the
        // roster says who follows whom).
        for (id, point, color) in anchors {
            if !self.rect.contains(point) {
                continue;
            }
            let rect = Rect::new(
                point.x - 15.0,
                point.y - 15.0,
                point.x + 15.0,
                point.y + 15.0,
            );
            self.follow_cursor_targets.push((id.clone(), rect));
            // Only the person followed is ringed: a ring on every pointer
            // is noise over the work. Every pointer is still a target.
            let active = self.followed_peer.as_deref() == Some(id.as_str());
            if !active {
                continue;
            }
            // The ring (radius 13, stroke 2.5) and its satellite stay within
            // the target; two more pixels hold their antialiased edges.
            painted.push(transform.transform_rect_bbox(rect.inflate(2.0, 2.0)));
            ui::presence::paint_followed_pointer(scene, transform, point, color);
        }
        painted
    }

    /// Each followable peer's pointer as [`Self::workspace_peers`] paints it,
    /// so the ring sits on the cursor rather than where it is gliding to.
    pub(super) fn workspace_follow_anchors(
        &self,
        presence: &PresenceCtx,
        reduce_motion: bool,
    ) -> Vec<(String, Point, Color)> {
        let now = std::time::Instant::now();
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let local_user = state.active_local_user();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let key = PresenceKey::Workspace {
            id: workspace.room.to_string(),
        };
        let rows =
            tilo_desktop::collaborators::collaborator_roster_rows(&tree.desktop, &local_user);
        let Some(pointers) = presence.pointers.get(&key) else {
            return Vec::new();
        };
        let anchors = pointers
            .iter()
            .filter_map(|(user, heard)| {
                let row = rows.iter().find(|row| row.followable && row.id == user.0)?;
                let (x, y) = if reduce_motion {
                    *heard
                } else {
                    presence
                        .pointer_position_at(&key, user, now)
                        .unwrap_or(*heard)
                };
                Some((
                    user.0.clone(),
                    Point::new(f64::from(x), f64::from(y)),
                    row.color,
                ))
            })
            .collect();
        drop(tree);
        drop(state);
        anchors
    }

    /// Convert authenticated session ids in room presence into the durable
    /// roster ids painted by the People overlay. Both endpoints must still
    /// be live roster entries; a relation never creates a speculative row.
    pub(super) fn roster_following_pairs(
        &self,
        presence: &PresenceCtx,
        rows: &[RosterRow],
    ) -> Vec<(String, String)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let room = PresenceKey::Workspace {
            id: workspace.room.to_string(),
        };
        let mut roster_id_for_presence = HashMap::<String, String>::new();
        for participant in &workspace.collaborators {
            roster_id_for_presence.insert(participant.id.clone(), participant.id.clone());
            if let Some(session) = participant.session {
                roster_id_for_presence.insert(format!("peer@{session}"), participant.id.clone());
            }
        }
        let relationships = presence
            .following_in(&room)
            .into_iter()
            .filter_map(|(follower, target)| {
                let follower = roster_id_for_presence.get(follower.0.as_str())?;
                let target = roster_id_for_presence.get(target.0.as_str())?;
                rows.iter()
                    .any(|row| row.id == follower.as_str())
                    .then_some(())?;
                rows.iter()
                    .any(|row| row.id == target.as_str())
                    .then_some(())?;
                (follower != target).then(|| (follower.clone(), target.clone()))
            })
            .collect();
        drop(tree);
        drop(state);
        relationships
    }

    /// Exactly the peers [`Self::draw_real_peers`] is about to paint:
    /// who, where, and what colour. Separated from the paint so the
    /// selection can be asserted on directly — "no peer arrived" and "a
    /// peer arrived and was filtered out" must be distinguishable in a
    /// test, not just an empty screen both ways.
    #[cfg(test)]
    pub(crate) fn visible_peers(&self, presence: &PresenceCtx) -> Vec<ui::presence::PeerCursor> {
        self.workspace_peers(presence, false)
            .into_iter()
            .filter(|peer| self.rect.contains(Point::new(peer.pos.0, peer.pos.1)))
            .collect()
    }

    /// The active workspace's peer pointers as painted: each where its
    /// glide between samples has reached
    /// ([`PresenceCtx::pointer_position_at`]) at the opacity its fade has
    /// reached. With `reduce_motion` neither moves: a pointer is shown where
    /// it was heard and goes, rather than fading, when retired.
    pub(super) fn workspace_peers(
        &self,
        presence: &PresenceCtx,
        reduce_motion: bool,
    ) -> Vec<ui::presence::PeerCursor> {
        let fade = if reduce_motion {
            std::time::Duration::ZERO
        } else {
            ui::presence::POINTER_FADE
        };
        let motion = self.motion.get();
        let Some(chrome_rc) = self.chrome.as_ref() else {
            return Vec::new();
        };
        // Scope by the workspace's room, not local `WorkspaceId` — the room
        // is the durable identity apps address collaboration to (stamped in
        // as `TILO_WORKSPACE_ID`); the local id differs per machine.
        let (room, local) = {
            let chrome = chrome_rc.borrow();
            let local = chrome.active_local_user();
            let room = chrome
                .tree()
                .desktop
                .active_workspace()
                .map(|w| w.room.to_string());
            (room, local)
        };
        let Some(room) = room else {
            return Vec::new();
        };
        let key = PresenceKey::Workspace { id: room };
        let Some(peers) = presence.pointers.get(&key) else {
            return Vec::new();
        };

        // Settle colours over the whole room (including the local user)
        // before filtering to what's on screen, so every host computes
        // the same assignment and a person is one colour everywhere.
        let mut room_ids: Vec<&str> = peers.iter().map(|(u, _)| u.0.as_str()).collect();
        room_ids.push(local.as_str());
        let reserved: Vec<(u8, u8, u8)> = presence.color_overrides.values().copied().collect();
        let assigned = ui::presence::assign_peer_colors(&room_ids, &reserved);
        let now = std::time::Instant::now();

        peers
            .iter()
            .map(|(user, pos)| {
                // An explicit roster colour outranks anything computed.
                let (r, g, b) = presence
                    .color_overrides
                    .get(user)
                    .copied()
                    .or_else(|| assigned.get(user.0.as_str()).copied())
                    .unwrap_or_else(|| presence.color_for(user));
                let pos = if reduce_motion {
                    *pos
                } else {
                    presence
                        .pointer_position_at(&key, user, now)
                        .unwrap_or(*pos)
                };
                let is_agent = presence.is_agent(user);
                ui::presence::PeerCursor {
                    pos: (f64::from(pos.0), f64::from(pos.1)),
                    // Falls back to the id when the roster has no name. An
                    // agent's pill carries the agent mark, so its name
                    // stands alone.
                    label: if is_agent {
                        presence
                            .names
                            .get(user)
                            .cloned()
                            .unwrap_or_else(|| presence.display_name(user))
                    } else {
                        presence.display_name(user)
                    },
                    color: Color::from_rgb8(r, g, b),
                    is_agent,
                    opacity: presence.pointer_opacity(
                        &key,
                        user,
                        now,
                        ui::presence::POINTER_IDLE,
                        fade,
                    ),
                    name_opacity: presence.pointer_name_opacity(&key, user, now, fade),
                    chat: presence.chats.bubble(user, now, motion),
                    reaction: presence.chats.reaction(user, now, motion),
                }
            })
            .collect()
    }

    pub(super) fn draw_peer_sync(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.peer_sync_overlay
            .draw(self.rect, scene, transform, text_renderer, theme);
    }
}

impl ShellOverlayHost {
    /// Keep local focus attached to the followed participant's current tab.
    /// A missing participant or closed target releases every app in the room.
    /// Until a first live location arrives, the existing view stays in place.
    pub(super) fn reconcile_followed_peer(&mut self) -> bool {
        let Some(peer_id) = self.followed_peer.as_deref() else {
            return false;
        };
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let context_changed = {
            let state = chrome.borrow();
            self.followed_tab
                .is_some_and(|tab| state.active_tab_id() != Some(tab))
                || state
                    .tree()
                    .desktop
                    .active_workspace()
                    .is_none_or(|workspace| {
                        self.followed_room
                            .as_ref()
                            .is_some_and(|room| room != &workspace.room)
                    })
        };
        if context_changed {
            // A semantic focus command takes control just like a pointer or
            // key. Release the original room before reconciliation can undo
            // local navigation or attach its apps to a different workspace.
            self.set_followed_peer(None);
            return true;
        }
        let (target, unavailable) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let roster: &[crate::shell::state::Collaborator] = match tree.desktop.active_workspace()
            {
                Some(workspace) if !workspace.collaborators.is_empty() => &workspace.collaborators,
                _ => &tree.desktop.collaborators,
            };
            let participant = roster
                .iter()
                .find(|participant| participant.id == peer_id && !participant.invited);
            let tab = participant.and_then(|participant| participant.active_tab);
            let target = tab.and_then(|tab| {
                tree.desktop
                    .active_workspace()?
                    .strips
                    .iter()
                    .find_map(|strip| {
                        strip.window_groups.iter().find_map(|group| {
                            group
                                .tabs
                                .iter()
                                .any(|candidate| candidate.id == tab)
                                .then_some((strip.id, group.id, tab))
                        })
                    })
            });
            // A window the followed person just opened reaches this
            // device's desktop after where they are does: the view waits
            // for it rather than letting go. The window it followed them
            // into closing is the end of it.
            let unavailable = participant.is_none()
                || (tab.is_some() && target.is_none() && tab == self.followed_tab)
                || (self.followed_tab.is_some() && tab.is_none())
                || self.followed_session.is_some_and(|session| {
                    participant.is_none_or(|participant| participant.session != Some(session))
                });
            drop(tree);
            drop(state);
            (target, unavailable)
        };
        if unavailable {
            self.set_followed_peer(None);
            return true;
        }
        let Some((strip, group, tab)) = target else {
            return false;
        };
        let changed = self.focus_window_on_strip(strip, group, tab);
        // A replicated tab can reach the tree before its live row. Until
        // that row accepts focus, the old local tab is not navigation intent.
        // Preserve the last successfully followed target and retry next frame.
        if chrome.borrow().active_tab_id() == Some(tab) {
            self.followed_tab = Some(tab);
        }
        changed
    }

    /// Change Follow once, then notify every local app in the room using the
    /// participant id its presence stream actually carries (`peer@<handle>`).
    /// Shell focus keeps using the durable roster id, so names and capability
    /// rows remain stable while sessions reconnect.
    pub(super) fn set_followed_peer(&mut self, peer_id: Option<String>) {
        if self.followed_peer == peer_id {
            return;
        }
        let previous_room = self.followed_room.take();
        if let Some(chrome) = self.chrome.as_ref()
            && let Ok(mut state) = chrome.try_borrow_mut()
        {
            state.search_context.following = peer_id.is_some();
        }
        self.followed_peer = peer_id;
        self.followed_tab = None;
        self.followed_session = None;
        if self.followed_peer.is_some() {
            self.follow_stopped = None;
            // The follow bar takes the top centre.
            self.dismiss_presence_notice();
            self.follow_visual = self.followed_roster_row();
            self.follow_transition.show(
                true,
                ui::motion::Appear::SPEC,
                self.motion.get(),
                self.input_source,
                ui::time::now(),
            );
        } else {
            self.follow_transition.show(
                false,
                ui::motion::Appear::SPEC,
                self.motion.get(),
                self.input_source,
                ui::time::now(),
            );
        }
        let active_command = match (self.followed_peer.as_deref(), self.chrome.as_ref()) {
            (Some(id), Some(chrome)) => {
                let state = chrome.borrow();
                let tree = state.tree();
                let command = tree.desktop.active_workspace().map(|workspace| {
                    let session = workspace
                        .collaborators
                        .iter()
                        .find(|participant| participant.id == id)
                        .and_then(|participant| participant.session);
                    let target =
                        session.map_or_else(|| id.to_string(), |session| format!("peer@{session}"));
                    (workspace.room, Some(target), session)
                });
                drop(tree);
                drop(state);
                command
            }
            _ => None,
        };
        if let Some((room, _, session)) = &active_command {
            self.followed_room = Some(*room);
            self.followed_session = *session;
        }
        let command = active_command
            .map(|(room, target, _)| (room, target))
            .or_else(|| {
                previous_room.map(|room| (room, None)).or_else(|| {
                    self.chrome.as_ref().and_then(|chrome| {
                        let state = chrome.borrow();
                        let tree = state.tree();
                        let command = tree
                            .desktop
                            .active_workspace()
                            .map(|workspace| (workspace.room, None));
                        drop(tree);
                        drop(state);
                        command
                    })
                })
            });
        if let Some((room, target)) = command {
            if let Some(bridge) = self.follow_bridge.as_ref() {
                bridge(room, target.clone());
            }
            if let Some(presence) = self.follow_presence_bridge.as_ref() {
                presence(room, target);
            }
        }
        if self.followed_peer.is_some() {
            self.reconcile_followed_peer();
        } else {
            // Keep the previous geometry only while the exit is visible so
            // a released dock cannot leave a ghost hit target behind.
            self.follow_stop_rect = Rect::ZERO;
        }
    }

    /// Local keyboard intent always takes control back before the key can
    /// change the local document or viewport. Escape is the discoverable
    /// exit; the broader rule also prevents a followed view from snapping
    /// back over an intentional local edit.
    pub(super) fn release_follow_for_local_key(&mut self, event: &WindowEvent) -> bool {
        let pressed = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        );
        if pressed && self.followed_peer.is_some() {
            let escape = matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                        ..
                    },
                    ..
                }
            );
            // Escape is asking to stop; any other key is the person taking
            // back their own view, and the bar says so for a moment.
            if escape {
                self.set_followed_peer(None);
            } else {
                self.stop_following_for_local_input();
            }
            return true;
        }
        false
    }

    /// The person did something of their own in the followed view: stop
    /// following, and say so on the bar for a moment.
    pub(super) fn stop_following_for_local_input(&mut self) {
        if self.followed_peer.is_some() {
            self.follow_stopped = Some(Instant::now());
        }
        self.set_followed_peer(None);
    }

    pub(super) fn followed_roster_row(&self) -> Option<RosterRow> {
        let peer_id = self.followed_peer.as_deref()?;
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let local_user = state.active_local_user();
        tilo_desktop::collaborators::collaborator_roster_rows(&state.tree().desktop, &local_user)
            .into_iter()
            .find(|row| row.id == peer_id && row.followable)
    }
}
