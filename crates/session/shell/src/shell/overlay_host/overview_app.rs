//! The workspace overview ([`tilo_overview`]) as the shell hosts it: an
//! ordinary app mounted in this process, its actions published through the
//! session's broker. The shell lends it what the desktop holds (every
//! workspace, the strips and windows of the one you are in, who is where),
//! carries out what it asks (go somewhere, move or close a window, name or
//! move a strip, make, join or leave a workspace), and draws the pictures of
//! the windows it names ([`super::window_flight`]) on the overview's zoom.
//!
//! Super+Tab, a pinch, the palette and the top bar still say whether the
//! overview is up through the chrome's overview flag; the app's
//! presentation follows it, and the app taking itself down clears it.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use std::collections::{HashMap as Map, HashSet};

use tilo_overview::{OverviewDesk, OverviewPick, OverviewSeat};
use tilo_sheets::{Phase, Seat as _};

use super::sheet_apps::SheetHost;

impl Default for SheetHost<OverviewSeat> {
    fn default() -> Self {
        Self::new(
            LayerId::Overview,
            LayerId::Overview.surface_kind(),
            tilo_overview::definition,
        )
    }
}

impl ShellOverlayHost {
    /// Mount the overview's app in the room the person works in, as an app
    /// the session launched, so its actions are published there.
    pub(super) fn mount_overview(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let state = chrome.borrow();
        let room = state.tree().desktop.active_workspace().map_or_else(
            || "desktop".to_owned(),
            |workspace| workspace.room().to_string(),
        );
        let apps = state.hosted_inputs.in_process().cloned();
        drop(state);
        self.layers
            .overview
            .mount(&room, |definition, room, instance, seat| match &apps {
                Some(apps) => apps
                    .borrow()
                    .mount_sheet(definition, room, instance, |cx| cx.lend(seat)),
                None => crate::shell::in_process::mount_sheet(
                    definition,
                    &crate::shell::state::local_user_id(),
                    "this-device",
                    room,
                    instance,
                    |cx| cx.lend(seat),
                )
                .map(|component| (component, None)),
            });
        if self.layers.overview.is_mounted() && self.overview_desk_revision.is_none() {
            self.refresh_overview_desk(true);
        }
    }

    /// Keep the overview in step with the desktop and with the overview
    /// flag, carry out what it asked, and service its app; whether anything
    /// changed on screen.
    pub(super) fn pump_overview(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        // Its actions are published once it is mounted, so an agent
        // reaches them before the overview was ever opened.
        self.mount_overview();
        if !self.layers.overview.is_mounted() {
            return false;
        }
        self.refresh_overview_desk(false);
        self.follow_overview_flag();
        let shown = self.layers.overview.update(doc, path);
        let acted = self.settle_overview();
        shown | acted
    }

    /// Bring the app's presentation in line with the overview flag.
    fn follow_overview_flag(&mut self) {
        let open = self.overlay_flags().overview;
        let phase = self.layers.overview.seat().presentation().phase();
        match (open, phase) {
            (true, Phase::Closed) => {
                self.refresh_overview_desk(true);
                self.layers.overview.open();
                self.overview_closing = false;
            }
            // Asked up again while it was still leaving because the shell
            // took it down: it comes back from where it is.
            (true, Phase::Leaving) if self.overview_closing => {
                self.refresh_overview_desk(true);
                self.layers.overview.open();
                self.overview_closing = false;
            }
            (false, Phase::Open) => {
                self.overview_closing = self.layers.overview.close();
            }
            _ => {}
        }
    }

    /// Carry out what the overview asked since it was last asked; whether
    /// anything was.
    pub(super) fn settle_overview(&mut self) -> bool {
        let seat = self.layers.overview.seat().clone();
        let asked = seat.take_asked();
        let mut acted = !asked.is_empty();
        for pick in asked {
            self.carry_out_overview_pick(pick);
        }
        let Some(chrome) = self.chrome.clone() else {
            return acted;
        };
        if seat.take_open_asked() && !chrome.borrow().overlays.overview {
            {
                let mut state = chrome.borrow_mut();
                state.overlays.overview = true;
                state.bump();
            }
            self.latch_overview_transition();
            self.follow_overview_flag();
            acted = true;
        }
        // The overview took itself down: a choice, Escape, a press on
        // nothing.
        let phase = seat.presentation().phase();
        if phase != Phase::Open && !self.overview_closing && chrome.borrow().overlays.overview {
            {
                let mut state = chrome.borrow_mut();
                state.overlays.overview = false;
                state.bump();
            }
            // Its flag is down now, so the flag decides from here: asked up
            // again before it has finished leaving, it comes back.
            self.overview_closing = true;
            self.latch_overview_transition();
            acted = true;
        }
        if acted {
            self.refresh_overview_desk(false);
        }
        acted
    }

    /// Lend the overview what the desktop holds now, when it changed (or
    /// always, `force`).
    pub(super) fn refresh_overview_desk(&mut self, force: bool) {
        if !self.layers.overview.is_mounted() {
            return;
        }
        let revision = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.borrow().revision);
        let key = (revision, self.layers.window_switcher.is_active());
        if !force && self.overview_desk_revision == Some(key) {
            return;
        }
        self.overview_desk_revision = Some(key);
        let desk = self.overview_desk();
        self.layers.overview.seat().set_desk(desk);
    }

    /// What the overview shows: every workspace as the overview projects
    /// it, the one you are in with its windows in the order the strips show
    /// them and only those the switcher lists, and what only the shell
    /// knows (each window's size, the strips this machine names, who wears
    /// which colour, the workspaces held through a site).
    fn overview_desk(&self) -> OverviewDesk {
        let Some(chrome) = self.chrome.as_ref() else {
            return OverviewDesk::default();
        };
        let listed = self.overview_listed_windows();
        let Ok(state) = chrome.try_borrow() else {
            return OverviewDesk::default();
        };
        let local_user = state.active_local_user();
        let tree = state.tree();
        let mut cards = crate::shell::overview::room_cards(&tree, &local_user);
        let mut renamable = HashSet::new();
        let mut colors: Map<String, (u8, u8, u8)> = Map::new();
        for workspace in &tree.desktop.workspaces {
            colors.extend(tilo_desktop::collaborators::roster_colors_with_local_user(
                &workspace.collaborators,
                &state.local_user_id,
            ));
        }
        let rooms: Vec<(WorkspaceId, crate::shell::state::RoomId)> = tree
            .desktop
            .workspaces
            .iter()
            .map(|workspace| (workspace.id, workspace.room()))
            .collect();
        if let (Some(here), Some(workspace)) = (
            cards.iter_mut().find(|card| card.is_current),
            tree.desktop.active_workspace(),
        ) {
            for (shown, strip) in here.strips.iter_mut().zip(&workspace.strips) {
                if tree.may_rename_strip(strip.id) {
                    renamable.insert(strip.id);
                }
                let order: Vec<GroupId> =
                    tilo_desktop::composition::display_ordered(&tree.desktop.outputs, strip)
                        .into_iter()
                        .map(|group| group.id)
                        .collect();
                shown.groups.sort_by_key(|group| {
                    order
                        .iter()
                        .position(|id| *id == group.id)
                        .unwrap_or(usize::MAX)
                });
                for group in &mut shown.groups {
                    group.tabs.retain(|tab| listed.contains_key(&tab.id));
                }
                shown.groups.retain(|group| !group.tabs.is_empty());
            }
        }
        drop(tree);
        drop(state);
        let mut group_sizes = Map::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see the field doc on `group_row_ptrs`; read-only.
            let row: &ui::widget::GroupRow = unsafe { &*ptr };
            for group in &row.groups {
                let rect = ui::widget::Widget::get_rect(group);
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    group_sizes.insert(group.id, rect.size());
                }
            }
        }
        let site_rooms = self.site_rooms();
        let mut sites = Map::new();
        let mut leavable = HashSet::new();
        for (index, (id, room)) in rooms.iter().enumerate() {
            if let Some(line) = self.overview_site_line(index) {
                sites.insert(*id, line);
            }
            if site_rooms.iter().any(|site| site.room == *room) {
                leavable.insert(*id);
            }
        }
        OverviewDesk {
            cards,
            group_sizes,
            renamable,
            colors,
            sites,
            leavable,
            pictured: listed.keys().copied().collect(),
            can_add: true,
            can_join: true,
            hide_windows: self.layers.window_switcher.is_active(),
        }
    }

    /// Carry out one thing the overview asked.
    pub(super) fn carry_out_overview_pick(&mut self, pick: OverviewPick) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match pick {
            OverviewPick::Window(tab) => {
                self.go_to_overview_window(tab);
            }
            OverviewPick::Workspace(id) => {
                let mut state = chrome.borrow_mut();
                state.tree_mut().set_active(id);
                state.overlays.overview = false;
                state.bump();
                drop(state);
                self.latch_overview_transition();
            }
            OverviewPick::NewWorkspace => {
                let mut state = chrome.borrow_mut();
                state.overlays.overview = false;
                state.add_workspace("Untitled");
                drop(state);
                self.latch_overview_transition();
            }
            OverviewPick::JoinWorkspace => {
                let mut state = chrome.borrow_mut();
                state.overlays.overview = false;
                state.push_peer_sync_request(PeerSyncAction::BrowseDirectory);
                state.bump();
                drop(state);
                self.latch_overview_transition();
            }
            OverviewPick::LeaveWorkspace(id) => {
                let mut state = chrome.borrow_mut();
                let room = state
                    .tree()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == id)
                    .map(crate::shell::state::Workspace::room);
                if let Some(room) = room {
                    state.push_peer_sync_request(PeerSyncAction::LeaveSite { room });
                    state.bump();
                }
            }
            // Through the one tab lifecycle, so it can be reopened.
            OverviewPick::CloseWindow(tab) => {
                let _ = crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab);
            }
            // A client window asks first what crosses, on the transfer
            // sheet; the overview stays up to show it leave.
            OverviewPick::MoveWindow { tab, to } => {
                let mut state = chrome.borrow_mut();
                if let Err(err) =
                    crate::shell::window_actions::move_tab_to_workspace(&mut state, tab, to)
                {
                    state.push_warning_notification(
                        t("overview-window-move-failed"),
                        err.user_message(),
                    );
                }
            }
            OverviewPick::MoveWindowToStrip { tab, strip } => {
                move_window_to_strip(&chrome, tab, strip);
            }
            OverviewPick::RenameStrip { strip, name } => {
                let mut state = chrome.borrow_mut();
                if state.rename_strip(strip, &name).is_err() {
                    state.push_warning_notification(
                        t("overview-strip-rename-failed"),
                        String::new(),
                    );
                }
            }
            OverviewPick::MoveStrip { strip, before } => {
                let mut state = chrome.borrow_mut();
                if state.move_strip(strip, before).is_err() {
                    state.push_warning_notification(t("overview-strip-move-failed"), String::new());
                }
            }
            OverviewPick::Dismiss => {
                let mut state = chrome.borrow_mut();
                state.overlays.overview = false;
                state.bump();
                drop(state);
                self.latch_overview_transition();
            }
        }
    }

    /// Go to window `tab` and leave the overview: in the workspace you are
    /// in it is focused where it stands, in another the workspace is
    /// entered on it.
    fn go_to_overview_window(&mut self, tab: crate::shell::state::TabId) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let found = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|group| group.tabs.iter().any(|t| t.id == tab))
                        .map(|group| (workspace.id, strip.id, group.id))
                })
            })
        };
        let Some((workspace, strip, group)) = found else {
            return false;
        };
        let here = chrome.borrow().tree().desktop.active == workspace;
        let focused = if here {
            self.focus_switcher_window(tilo_desktop::window_switcher::WindowAddress {
                strip,
                group,
                tab,
            })
        } else {
            let mut state = chrome.borrow_mut();
            state.tree_mut().set_active(workspace);
            state.select_tab(workspace, strip, group, tab);
            true
        };
        {
            let mut state = chrome.borrow_mut();
            state.overlays.overview = false;
            state.bump();
        }
        self.latch_overview_transition();
        focused
    }

    /// A key, or text an input method committed, while the overview is up:
    /// the overview's own, ahead of the shell's chords. Super chords stay
    /// the shell's (Super+Tab closes the overview) unless a strip's name is
    /// being typed. Whether the overview took it.
    pub(super) fn route_overview_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let flags = self.overlay_flags();
        if !flags.overview
            || self.palette_visible
            || flags.quick_settings
            || flags.shortcut_help
            || self.strip_navigator.takes_keys()
            || self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().editing_workspace_name.is_some())
        {
            return false;
        }
        // A key that follows Super+Tab at once is the overview's, before a
        // frame has put it up.
        self.mount_overview();
        self.follow_overview_flag();
        let typing = self.layers.overview.seat().holds_keyboard();
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && !typing
            && (ctx.modifiers.state().super_key()
                || matches!(key.logical_key, Key::Named(NamedKey::F1)))
        {
            return false;
        }
        let taken = self.layers.overview.offer_event(event, ctx);
        if taken {
            self.settle_overview();
            self.bump_chrome();
        }
        taken
    }

    /// Pointer and touch input while the overview is up: a window pressed,
    /// carried or middle-clicked, a room, a row, a strip's name; any other
    /// press leaves the overview. Whether the overview took it.
    pub(super) fn route_overview_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.overlay_flags().overview {
            return false;
        }
        // A press lands where the pointer last was, whoever saw it move.
        if let (WindowEvent::MouseInput { .. }, Some(at)) = (event, self.last_cursor) {
            let moved =
                ui::synthetic_input::cursor_moved(at.x * ctx.scale_factor, at.y * ctx.scale_factor);
            let handled = *ctx.handled;
            let _ = self.layers.overview.offer_event(&moved, ctx);
            *ctx.handled = handled;
        }
        let taken = self.layers.overview.offer_event(event, ctx);
        if taken {
            self.settle_overview();
            self.bump_chrome();
        }
        taken
    }

    /// The overview, drawn at its zoom: the app over the display it covers,
    /// then the pictures it names, each from the window it pictures.
    pub(super) fn draw_overview_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        overview_open: bool,
    ) {
        let progress =
            self.sync_overview_zoom(overview_open, ui::motion::input_source(), theme.motion());
        let flight = self.layers.overview.seat().flight().clone();
        let drawn = self.layers.overview.is_drawn();
        // The app stands over the display it covers, in its own
        // coordinates from that display's corner.
        let origin = self.layers.overview.area().origin().to_vec2();
        let moved = flight.progress().to_bits() != progress.to_bits();
        if drawn {
            let listed = self.overview_listed_windows();
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Rect ± Vec2 is float geometry translation and cannot panic"
            )]
            let sources = self
                .overview_window_sources(&listed)
                .into_iter()
                .map(|(tab, rect)| (tab, rect - origin))
                .collect();
            flight.set_clock(progress, sources);
            // Its titles and panel move on the same clock: it draws again.
            if moved {
                self.layers.overview.seat().presentation().wake();
            }
        } else {
            flight.set_clock(progress, Map::new());
        }
        // One keyed part, drawn even while it paints nothing, so a fade
        // damages only the display the overview covers: a part that appears
        // or disappears changes the section's structure, which is a dense
        // frame of every display.
        let key = ui::scene_parts::PartKey::new("shell.overview", ());
        ui::scene_parts::part_drawn(scene, key, |scene| {
            if !drawn {
                self.overview_row_drawn.clear();
                return Vec::new();
            }
            if progress <= 0.0 {
                // The instant it was asked up: nothing of it shows yet, but
                // the app runs its frame, so what it names is ready.
                self.layers.overview.draw_app(
                    &mut Scene::new(),
                    transform,
                    text_renderer,
                    theme,
                    &PresenceCtx::default(),
                );
                self.overview_row_drawn.clear();
                return Vec::new();
            }
            let output = self.overview_output_rect();
            // Nothing of the overview (its panel's shadow, a window flying
            // in from the next display) is drawn on another display.
            scene.push_clip_layer(Fill::NonZero, transform, &output);
            self.layers.overview.draw_app(
                scene,
                transform,
                text_renderer,
                theme,
                &PresenceCtx::default(),
            );
            self.draw_window_flight(
                scene,
                transform,
                text_renderer,
                theme,
                (&flight, progress, origin),
            );
            scene.pop_layer();
            vec![transform.transform_rect_bbox(output)]
        });
        // Leaving, and every picture has landed back on its strip: nothing
        // of the overview is left on screen.
        if !overview_open && progress <= 0.0 && !self.overview_zoom_moving(ui::time::now()) {
            let presentation = self.layers.overview.seat().presentation();
            if presentation.phase() == Phase::Leaving {
                presentation.gone();
                self.overview_closing = false;
            }
        }
    }

    /// The two full-display surfaces drawn over the workspace: the overview
    /// and the workspace-settings sheet. Their text is laid out at the
    /// theme's own scale, so the shared renderer's is set aside meanwhile.
    pub(super) fn draw_overview_and_ws_settings_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_overview_overlay(scene, transform, text_renderer, theme, flags.overview);
        self.draw_ws_settings_overlay(scene, transform, text_renderer, theme, flags.ws_settings);
        text_renderer.set_text_scale(renderer_scale);
    }

    /// Fast-forward the overview's zoom so a single-frame render sees the
    /// overview settled open (or gone).
    pub fn force_overview_settled(&mut self, target: bool) {
        self.mount_overview();
        self.follow_overview_flag();
        self.overview_zoom.snap(target);
        let flight = self.layers.overview.seat().flight().clone();
        flight.set_clock(if target { 1.0 } else { 0.0 }, Map::new());
        if !target {
            let presentation = self.layers.overview.seat().presentation();
            if presentation.phase() != Phase::Open {
                presentation.gone();
                self.overview_closing = false;
            }
        }
    }

    /// The overview's semantic tree as its app publishes it, while it is
    /// up and laid out: the dialog a screen reader enters.
    pub fn overview_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        fn find(node: ui::a11y::AccessNode, name: &str) -> Option<ui::a11y::AccessNode> {
            if node.role == ui::a11y::Role::Dialog && node.name.as_deref() == Some(name) {
                return Some(node);
            }
            node.children
                .into_iter()
                .find_map(|child| find(child, name))
        }
        if !self.overlay_flags().overview {
            return None;
        }
        let tree = self.layers.overview.widget()?.accessibility()?;
        find(tree, &tilo_desktop::i18n::t("overview-title"))
    }

    /// Whether the overview is up, as its app shows it.
    pub fn overview_open(&self) -> bool {
        self.layers.overview.requested()
    }

    /// What the overview's app names by `name` (its rooms
    /// `overview_card[i]`, `add_workspace`, each pictured window
    /// `overview-window:<tab>`, each strip `overview-strip:<id>`), while it
    /// is laid out.
    pub fn overview_targets(&self) -> Vec<(String, Rect)> {
        if !self.overlay_flags().overview {
            return Vec::new();
        }
        self.layers
            .overview
            .widget()
            .map(Widget::test_rects)
            .unwrap_or_default()
    }

    /// The overview's target named `name`, if it shows one.
    pub fn overview_target(&self, name: &str) -> Option<Rect> {
        self.overview_targets()
            .into_iter()
            .find(|(shown, _)| shown == name)
            .map(|(_, rect)| rect)
    }

    /// One rect per workspace in the overview, in tree order; empty while
    /// it is closed.
    pub fn overview_row_rects_for_tests(&self) -> Vec<Rect> {
        let targets = self.overview_targets();
        (0..)
            .map_while(|index| {
                let name = format!("overview_card[{index}]");
                targets
                    .iter()
                    .find(|(shown, _)| *shown == name)
                    .map(|(_, rect)| *rect)
            })
            .collect()
    }

    /// The "New workspace" row while the overview is up; `Rect::ZERO`
    /// otherwise.
    pub fn add_workspace_rect_for_tests(&self) -> Rect {
        self.overview_target("add_workspace").unwrap_or(Rect::ZERO)
    }
}

/// Move the window group holding `tab` to `strip` of the active workspace,
/// or say the window could not be moved.
fn move_window_to_strip(
    chrome: &crate::shell::chrome_state::Chrome,
    tab: tilo_desktop_model::TabId,
    strip: tilo_desktop_model::StripId,
) {
    let mut state = chrome.borrow_mut();
    let group = state
        .tree()
        .desktop
        .active_workspace()
        .and_then(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|t| t.id == tab))
                .map(|group| group.id)
        });
    match group {
        Some(group) => state.request_window(
            crate::shell::chrome_state::WindowRequest::MoveGroupToStrip { group, strip },
        ),
        None => {
            state.push_warning_notification(t("overview-window-move-failed"), String::new());
        }
    }
}
