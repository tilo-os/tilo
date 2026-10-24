//! Desktop painting: the bar, notices, workspace sections, strip chrome and dropdowns.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn draw_desktop_extras(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_real_peers(scene, transform, text_renderer, theme, presence);
        self.draw_desktop_notices(scene, transform, text_renderer, theme, presence);
    }

    /// What the desktop shows over its windows where it is on screen: the
    /// follow chip, the permission prompt, notifications and the visual
    /// alert.
    pub(super) fn draw_desktop_notices(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use ui::scene_parts::{PartKey, part_drawn};
        part_drawn(scene, PartKey::new("shell.follow-chip", ()), |scene| {
            self.draw_follow_chip(scene, transform, text_renderer, theme);
            // Everything is inside a layer clipped to the chip, which slides
            // down 10 px as it opens.
            let chip = self.follow_chip_rect;
            if chip.is_zero_area() {
                return Vec::new();
            }
            vec![transform.transform_rect_bbox(Rect::new(
                chip.x0,
                chip.y0 - 10.0,
                chip.x1,
                chip.y1,
            ))]
        });
        // Below the overlay layer, so an open modal occludes it: act on
        // the modal first, then come back to the prompt.
        let area = self.overlay_area();
        let desktop = transform.transform_rect_bbox(self.rect);
        overlay_part(scene, "shell.permission-prompt", desktop, |scene| {
            if self.permission_prompt.is_some() {
                let surface = self.permission_prompt_surface();
                if let Some(prompt) = self.permission_prompt.as_mut() {
                    prompt.set_rect(surface);
                    prompt.draw(scene, transform, text_renderer, theme, presence);
                }
                // The seal is shell chrome. `PermissionPrompt` itself cannot
                // paint it, so an app that builds that widget does not get the frame.
                crate::shell::trusted_ui::paint_seal(
                    scene,
                    transform,
                    surface,
                    theme,
                    f64::from(theme.tokens.radii.control),
                );
            }
            None
        });

        // Above desktop content, below modal overlays and the permission
        // prompt, so a prompt always wins focus. Top inset keeps toasts
        // off the clock/tray.
        let window_bars = self.window_bar_rects();
        if let Some(chrome) = self.chrome.as_ref() {
            let viewport = Rect::new(
                area.x0,
                area.y0 + f64::from(self.chrome_top_y),
                area.x1,
                area.y1,
            );
            self.notification_stack.set_viewport(viewport);
            // Banners share the transients' zone with the follow bar: below
            // it while it is up, and below any window bar they would cover.
            let follow = self.follow_chip_rect;
            self.notification_stack
                .set_clear_top(if follow.is_zero_area() {
                    0.0
                } else {
                    follow.y1
                });
            self.notification_stack.set_window_bars(window_bars);
            let stack = &mut self.notification_stack;
            part_drawn(scene, PartKey::new("shell.notifications", ()), |scene| {
                stack.draw(
                    &ChromeNotifications::new(chrome),
                    scene,
                    transform,
                    text_renderer,
                    theme,
                );
                // `rect` is what the cards cover at rest. A card slides in
                // from up to its own width to the right and casts the
                // floating shadow.
                let rest = stack.rect();
                if rest.is_zero_area() {
                    return Vec::new();
                }
                let shadow = &theme.tokens.elevation.floating;
                let reach = f64::from(shadow.blur) * 2.0 + f64::from(shadow.offset_y).abs();
                let travel = Rect::new(rest.x0, rest.y0, rest.x1 + rest.width(), rest.y1);
                vec![transform.transform_rect_bbox(travel.inflate(reach, reach))]
            });
        }

        self.draw_machine_notices(scene, transform, text_renderer, theme, desktop);

        // A visual alert is deliberately an edge treatment rather than a
        // full-screen luminance flash: it stays obvious without obscuring
        // content or creating an unnecessarily harsh photosensitivity risk.
        let visual_alert = self.chrome.as_ref().is_some_and(|chrome| {
            chrome
                .borrow()
                .visual_alert_active(std::time::Instant::now())
        });
        // Along every display's own edges: the desktop's bounding box puts
        // a shorter display's bottom edge, or a stacked one's sides, on no
        // display at all.
        let displays = self.layer_outputs();
        overlay_part(scene, "shell.visual-alert", desktop, |scene| {
            if visual_alert {
                for display in &displays {
                    ui::chrome::shell::paint_display_edge(
                        scene,
                        transform,
                        *display,
                        ui::chrome::shell::DisplayEdge::Alert,
                        theme,
                    );
                }
            }
            None
        });
    }

    /// The machine's banners and a ringing call's edge on the displays
    /// other than the one in use.
    fn draw_machine_notices(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        desktop: Rect,
    ) {
        // The state of the whole machine is on every display. Banners about
        // the machine (critical battery) repeat, at rest, on the displays
        // other than the one in use, where the stack takes clicks and keys;
        // a ringing call, whose card is on the display in use, gives the
        // others a quiet edge. Steady, so a ringing call keeps the loop idle.
        let area = self.overlay_area();
        let others: Vec<Rect> = if self.is_partitioned() {
            self.layer_outputs()
                .into_iter()
                .filter(|display| *display != area)
                .collect()
        } else {
            Vec::new()
        };
        self.machine_banners.clear();
        self.call_edges.clear();
        if let Some(chrome) = self.chrome.as_ref()
            && !others.is_empty()
        {
            let ringing = NotificationStack::call_ringing(&ChromeNotifications::new(chrome));
            let top = f64::from(self.chrome_top_y);
            let window_bars = self.window_bar_rects();
            let banners = &mut self.machine_banners;
            let edges = &mut self.call_edges;
            overlay_part(scene, "shell.machine-notices", desktop, |scene| {
                for display in &others {
                    let viewport = Rect::new(display.x0, display.y0 + top, display.x1, display.y1);
                    let banner = NotificationStack::draw_machine_wide(
                        &ChromeNotifications::new(chrome),
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        (viewport, &window_bars),
                    );
                    if !banner.is_zero_area() {
                        banners.push(banner);
                    }
                    if ringing {
                        edges.push(*display);
                        ui::chrome::shell::paint_display_edge(
                            scene,
                            transform,
                            *display,
                            ui::chrome::shell::DisplayEdge::Ringing,
                            theme,
                        );
                    }
                }
                None
            });
        }
    }

    /// The production root has no painting container of its own, so its three typed children
    /// may be recorded separately without changing the final compositor layer order. Unrecognised roots are
    /// recorded as one opaque section.
    pub(super) fn draw_workspace_sections(
        &mut self,
        backdrop_scene: &mut Scene,
        topbar_scene: &mut Scene,
        workspace_scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        let slide = self.workspace_slide_poses(theme);
        if let Some((from, leaving, _)) = slide {
            self.draw_leaving_workspace(
                from,
                leaving,
                workspace_scene,
                transform,
                (text_renderer, theme, presence),
            );
        }
        let clip = self.rect;
        let Some(surface) = self.desktop_surface_mut() else {
            self.content
                .draw(backdrop_scene, transform, text_renderer, theme, presence);
            return None;
        };
        let bounds = if let Some((_, _, arriving)) = slide {
            // A workspace switch by pointer: the windows arrive along the
            // slide's axis; the bar and the backdrop stay.
            let bounds = surface.draw_chrome_sections(
                backdrop_scene,
                topbar_scene,
                transform,
                text_renderer,
                theme,
                presence,
            )?;
            workspace_scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                arriving.opacity,
                transform,
                &clip,
            );
            let drawn = surface.draw_workspace_area(
                workspace_scene,
                transform.pre_translate(arriving.transform.translation()),
                text_renderer,
                theme,
                presence,
            );
            workspace_scene.pop_layer();
            drawn.map(|()| bounds)?
        } else {
            surface.draw_sections(
                backdrop_scene,
                topbar_scene,
                workspace_scene,
                transform,
                text_renderer,
                theme,
                presence,
            )?
        };
        // The compact bars of the other displays share the top bar's scene.
        let bars = surface
            .display_chrome()
            .and_then(Widget::as_any)
            .and_then(|layer| layer.downcast_ref::<crate::shell::display_bars::DisplayBars>())
            .map(crate::shell::display_bars::DisplayBars::bar_rects)
            .unwrap_or_default();
        Some(bars.into_iter().fold(bounds, |all, (_, bar)| {
            all.union(Rect::new(bar.x0, bar.y0, bar.x1, bar.y1 + 28.0))
        }))
    }

    /// Ribbon, the strip switcher's card and the rail sit below Quick
    /// Settings and every popover.
    pub(super) fn draw_strip_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
        quick_settings_transition_visible: bool,
    ) {
        if flags.overview {
            return;
        }
        // Keep the retained section recording identical to the dense
        // composition: Quick Settings visually replaces the ribbon for its
        // complete open/close transition, while the rail remains.
        let hidden_by_qs = flags.quick_settings || quick_settings_transition_visible;
        self.draw_strip_navigation(scene, transform, (text_renderer, theme), hidden_by_qs);
    }

    /// Visual extent of the retained strip chrome. It is folded into the
    /// Quick Settings damage region because that section owns the chrome in
    /// both its open and closed states.
    pub(super) fn strip_chrome_bounds(
        &self,
        flags: crate::shell::chrome_state::OverlayFlags,
        quick_settings_transition_visible: bool,
    ) -> Option<Rect> {
        if flags.overview {
            return None;
        }
        self.strip_navigation_bounds(flags.quick_settings || quick_settings_transition_visible)
    }

    /// Who is here, as the bar's faces open it: it arrives from them and
    /// goes back to them on the standard motion (`motion::Appear`), as the
    /// bar's workspace menu and profile card do; a key lands it at once.
    /// Closed, it takes no input and offers no target at once, and what it
    /// last showed is drawn leaving until it has gone.
    pub(super) fn draw_collaborators_dropdown(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        self.collaborators_appearing
            .follow(flags.collaborators, theme.motion());
        if flags.collaborators {
            let mut painted = std::mem::take(&mut self.collaborators_painted);
            painted.reset();
            self.paint_collaborators_roster(&mut painted, text_renderer, theme, presence);
            self.collaborators_painted = painted;
            self.collaborators_painted_panel = self.collaborators_overlay.panel_rect();
        } else {
            self.collaborators_overlay.clear();
            if !self.collaborators_appearing.is_drawn() {
                self.collaborators_painted.reset();
                self.collaborators_painted_panel = Rect::ZERO;
                return;
            }
        }
        let panel = self.collaborators_painted_panel;
        // From the faces that opened it; from the bar above it when they
        // are not shown.
        let faces = self.main_bar_rect(tilo_desktop::topbar::TopBarChrome::collaborators_rect);
        let anchor = if faces.is_zero_area() {
            Point::new(panel.center().x, panel.y0)
        } else {
            faces.center()
        };
        let pose = self.collaborators_appearing.pose(panel, anchor);
        if flags.collaborators {
            self.collaborators_overlay.set_drawn_pose(pose);
        }
        let bounds = ui::widget::floating_surface::painted_bounds(panel, theme);
        if let Some(placed) = pose.begin(scene, transform, bounds) {
            scene.append(&self.collaborators_painted, Some(placed));
            pose.end(scene);
        }
    }

    /// Paint the roster at rest, in the host's coordinates, and lay out its
    /// targets.
    #[allow(
        clippy::too_many_lines,
        reason = "the single paint pass keeps roster decoration, action geometry, and overlay rendering synchronized"
    )]
    fn paint_collaborators_roster(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let transform = Affine::IDENTITY;
        // The name this person goes by, numbered like anyone else's when
        // someone here shares it; never "You", never their key.
        let own_name = ui::local_identity::load()
            .map(|identity| identity.display_name)
            .filter(|name| !name.trim().is_empty());
        let (mut rows, workspace, unavailable, exposure) = self.chrome.as_ref().map_or_else(
            || (Vec::new(), String::new(), None, None),
            |chrome| {
                let state = chrome.borrow();
                let local_user = state.active_local_user();
                let (mut rows, _) = tilo_desktop::collaborators::collaborator_roster_rows_named(
                    &state.tree().desktop,
                    &local_user,
                    own_name.as_deref(),
                );
                in_their_apps(&mut rows, &state.tree().desktop);
                let workspace = state
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.name.clone())
                    .unwrap_or_default();
                (
                    rows,
                    workspace,
                    state.collab_unavailable.clone(),
                    collaborators_screen_exposure(&state),
                )
            },
        );
        let now = std::time::SystemTime::now();
        let interactions = presence.visible_interactions(std::time::Instant::now());
        let history = self.room_activity.entries();
        for row in &mut rows {
            if let Some(status) = crate::shell::desk_widgets::team::status_line(presence, row, now)
            {
                row.status = Some(status);
            }
            if let Some(id) = crate::shell::desk_widgets::team::presence_id(presence, None, row) {
                row.availability = presence.availability_of(&id);
                row.call = presence.call_of(&id).map(str::to_owned);
            }
            // Where they are: the document or app in front of them.
            if let Some(interaction) = interactions.iter().find(|interaction| {
                interaction.actor.id == row.id
                    || presence
                        .names
                        .get(&ui::permissions::UserId(interaction.actor.id.clone()))
                        .is_some_and(|name| name.eq_ignore_ascii_case(&row.label))
            }) {
                row.activity = Some(interaction.resource.label.clone());
            }
            row.recent = history
                .iter()
                .rev()
                .filter(|entry| entry.author.name.eq_ignore_ascii_case(&row.label))
                .take(tilo_desktop::collaborators::RECENT_CHANGES_SHOWN)
                .map(|entry| tilo_desktop::collaborators::RecentChange {
                    text: entry.summary.clone(),
                    when: ui::widget::history::format_relative_time(entry.timestamp, now),
                    entry: Some(*entry.id.as_bytes()),
                })
                .collect();
        }
        // Who is in which voice channel, as the room's calls say, and the
        // Join each row offers.
        self.decorate_calls(&mut rows, presence);
        let may_invite = self.may_invite_here();
        let following_pairs = self.roster_following_pairs(presence, &rows);
        self.collaborators_overlay.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &CollaboratorsModel {
                host: self.overlay_area(),
                workspace: &workspace,
                rows: &rows,
                unavailable: unavailable.as_deref(),
                exposure: exposure.as_deref(),
                may_invite,
                followed_id: self.followed_peer.as_deref(),
                following_pairs: &following_pairs,
                expanded_id: self.expanded_collaborator.as_deref(),
                // Native presence has no spectator role yet.
                watching: 0,
                waved: &self.self_presence.waved_recently(std::time::Instant::now()),
                spectating: false,
            },
            self.last_cursor,
        );
    }
}

/// Where each of `rows` is when no window here shows it, as in a room
/// whose layout is personal: "In Notes", from the app their live presence
/// focuses, named as this device knows it.
fn in_their_apps(
    rows: &mut [tilo_desktop::collaborators::RosterRow],
    desktop: &crate::shell::state::DesktopTree,
) {
    let present = desktop
        .active_workspace()
        .map(|workspace| workspace.collaborators.as_slice())
        .unwrap_or_default();
    for row in rows.iter_mut().filter(|row| row.activity.is_none()) {
        row.activity = present
            .iter()
            .find(|participant| participant.id == row.id)
            .and_then(|participant| participant.in_app.as_deref())
            .and_then(crate::shell::launcher::display_name_for_app_id_without_waiting)
            .map(|app| tilo_desktop::collaborators::in_app(&app));
    }
}
