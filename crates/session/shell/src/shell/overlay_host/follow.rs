//! Following a peer: reconciliation, the follow chip and peer cursors.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// How [`ShellOverlayHost::roster_following_pairs`] names this person,
/// who has no row of their own: no row's id is empty.
pub(super) const OWN_ROSTER_ID: &str = "";

impl ShellOverlayHost {
    /// While following someone: the shared "Following Ada" banner (a frame
    /// in their colour round the work area and a bar with Stop in the
    /// transients' zone). Escape stops too.
    pub(super) fn draw_follow_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let area = self.overlay_area();
        let bar_top = area.y0 + f64::from(tilo_desktop::topbar::topbar_height(theme));
        let window_bars = self.window_bar_rects();
        let following = self
            .followed_peer
            .is_some()
            .then(|| self.followed_roster_row());
        let place = tilo_desktop::follow_banner::BannerFrame {
            work_area: Rect::new(area.x0, bar_top, area.x1, area.y1),
            viewport: self.transient_viewport(),
            window_bars: &window_bars,
            cursor: self.last_cursor,
            motion: self.motion.get(),
        };
        let kept =
            self.follow_banner
                .draw(scene, transform, text_renderer, theme, place, following);
        if !kept {
            self.set_followed_peer(None);
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
            id: workspace.room().to_string(),
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
            id: workspace.room().to_string(),
        };
        let mut roster_id_for_presence = HashMap::<String, String>::new();
        for participant in &workspace.collaborators {
            roster_id_for_presence.insert(participant.id.clone(), participant.id.clone());
            if let Some(session) = participant.session {
                roster_id_for_presence.insert(
                    crate::shell::peer_presence::presence_id(session),
                    participant.id.clone(),
                );
            }
        }
        let relationships = presence
            .following_in(&room)
            .into_iter()
            .filter_map(|(follower, target)| {
                let follower = roster_id_for_presence.get(follower.0.as_str())?;
                // Someone following this person: "you", never a row.
                if presence.is_own_identity(&target.0) {
                    return Some((follower.clone(), OWN_ROSTER_ID.to_owned()));
                }
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
                .map(|w| w.room().to_string());
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
                        crate::shell::peer_presence::POINTER_STAYS,
                        fade,
                    ),
                    name_opacity: presence.pointer_name_opacity(&key, user, now, fade),
                    chat: presence.chats.bubble(user, now, motion),
                    reaction: presence.chats.reaction(user, now, motion),
                }
            })
            .collect()
    }

    /// The invite, join and pairing sheet, over the display it opened on.
    pub(super) fn draw_peer_sync(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.peer_sync_node.is_none() {
            return;
        }
        // The sheet's scrim covers the display it measures against.
        let outer = presence.viewport.replace(Some(self.overlay_area()));
        ui::widget::Widget::draw_overlay(
            &mut self.peer_sync_overlay,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        presence.viewport.set(outer);
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
                            .is_some_and(|room| room != &workspace.room())
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
            let row = self.followed_roster_row();
            self.follow_banner
                .started(row, self.motion.get(), self.input_source, ui::time::now());
        } else {
            self.follow_banner.ended(
                std::mem::take(&mut self.follow_ended_by_own_input),
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
                    let target = session
                        .map_or_else(|| id.to_string(), crate::shell::peer_presence::presence_id);
                    (workspace.room(), Some(target), session)
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
                            .map(|workspace| (workspace.room(), None));
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
            // The banner's Stop went with following (`FollowBanner::ended`),
            // so a released dock leaves no ghost hit target behind.
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
        self.follow_ended_by_own_input = self.followed_peer.is_some();
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
