//! The slots Tilo apps put in their windows' bars ([`ui::window_toolbar`]):
//! an app declares them on its shell-ui connection, the shell hands them to
//! the window group whose tab shows the app, and what the person does with
//! them goes back to that app on the same connection.
//!
//! The bar's own search and History control reach an app in its own process
//! the same way: the app says what its window offers them
//! ([`WindowOffer`]), and the query, steps, close and History presses go
//! back to it as toolbar events.
//!
//! An app the shell mounts in its own process (a Terminal tab) has no
//! connection: it declares its slots, and asks to close its tab, on the
//! window the shell lent it ([`ui::hosted_window::HostedWindow`]), which the
//! shell looks at every frame ([`ShellOverlayHost::sync_hosted_windows`]).

use ui::widget::{GroupRow, WindowGroup};
use ui::window_toolbar::{ToolbarEvent, WindowOffer, WindowToolbar};

use super::ShellOverlayHost;
use crate::shell::state::TabId;

/// Every tab's declared slots, kept as every desktop keeps them.
pub(crate) type WindowToolbars = ui::window_slots::WindowSlots;

impl ShellOverlayHost {
    /// `app_id` on shell-ui `connection`, launched as record `instance`,
    /// declared the slots of its window's bar. Returns whether the desktop
    /// shows something new.
    pub(crate) fn declare_app_toolbar(
        &mut self,
        connection: u64,
        app_id: &str,
        instance: Option<u64>,
        toolbar: Option<WindowToolbar>,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let anchor = {
            let state = chrome.borrow();
            crate::shell::app_tabs::anchor_for_app(&state.tree(), app_id, instance)
        };
        let Some(anchor) = anchor else {
            tracing::debug!(%app_id, "window toolbar for an app with no tab");
            return false;
        };
        let changed = self
            .window_toolbars
            .declare(anchor.tab, connection, toolbar);
        if changed {
            self.note_window_place(anchor.tab);
            self.sync_window_toolbars();
        }
        changed
    }

    /// Remember where window `tab` is now, as its app declared it to the
    /// bar: what the launcher's Continue lists. A window that declares no
    /// location is nowhere to continue.
    fn note_window_place(&self, tab: TabId) {
        let Some(location) = self
            .window_toolbars
            .toolbar(tab)
            .and_then(|toolbar| toolbar.location.as_ref())
        else {
            return;
        };
        let Some(chrome) = &self.chrome else {
            return;
        };
        let found = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|candidate| candidate.id == tab)
                    .and_then(|candidate| {
                        tilo_desktop_model::apps::canonical_app_id(&candidate.tile)
                    })
                    .map(|app| (app.as_str().to_owned(), workspace.room().to_string()))
            })
        };
        let Some((app_id, room)) = found else {
            return;
        };
        crate::shell::search::recent_places::visit(
            &app_id,
            &room,
            tab,
            &location.label,
            &location.value,
            location.anchor.as_deref(),
        );
    }

    /// `app_id` on shell-ui `connection`, launched as record `instance`, said
    /// what its window offers the bar's search and History control. Returns
    /// whether the desktop shows something new.
    pub(crate) fn offer_app_window(
        &mut self,
        connection: u64,
        app_id: &str,
        instance: Option<u64>,
        offer: WindowOffer,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let anchor = {
            let state = chrome.borrow();
            crate::shell::app_tabs::anchor_for_app(&state.tree(), app_id, instance)
        };
        let Some(anchor) = anchor else {
            tracing::debug!(%app_id, "window offer from an app with no tab");
            return false;
        };
        let changed = self.window_toolbars.offer(anchor.tab, connection, offer);
        if changed {
            self.sync_window_toolbars();
        }
        changed
    }

    /// `app_id` (launched as record `instance`) asked for its window's
    /// search: its Ctrl+F, or its Ctrl+H (`opening`). Only the tab the
    /// window shows searches.
    pub(crate) fn open_app_window_search(
        &mut self,
        app_id: &str,
        instance: Option<u64>,
        opening: ui::window_toolbar::SearchOpening,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let anchor = {
            let state = chrome.borrow();
            crate::shell::app_tabs::anchor_for_app(&state.tree(), app_id, instance)
        };
        let Some(anchor) = anchor else {
            return false;
        };
        self.sync_window_toolbars();
        let mut opened = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(group) = row
                .groups
                .iter_mut()
                .find(|group| group.active_tab_id() == anchor.tab)
            {
                opened = group.open_search_for_app(anchor.tab, opening);
                break;
            }
        }
        if opened {
            // An app with nothing to search in place hands on to the palette.
            self.harvest_pending_search_open();
            self.sync_window_toolbars();
        }
        opened
    }

    /// `app_id` (launched as record `instance`) asked to edit its location
    /// in its window's bar: its Ctrl+L. Only the tab the window shows edits;
    /// a request from a tab behind another does nothing.
    pub(crate) fn edit_app_location(&mut self, app_id: &str, instance: Option<u64>) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let anchor = {
            let state = chrome.borrow();
            crate::shell::app_tabs::anchor_for_app(&state.tree(), app_id, instance)
        };
        let Some(anchor) = anchor else {
            return false;
        };
        self.sync_window_toolbars();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(group) = row
                .groups
                .iter_mut()
                .find(|group| group.active_tab_id() == anchor.tab)
            {
                return group.edit_location();
            }
        }
        false
    }

    /// Shell-ui `connection` closed.
    pub(crate) fn forget_app_toolbars(&mut self, connection: u64) -> bool {
        let changed = self.window_toolbars.disconnected(connection);
        if changed {
            self.sync_window_toolbars();
        }
        changed
    }

    /// Carry one message an app in its own process sent about its window's
    /// bar ([`ui::floating_surface::FloatingSurfaceClientMessage`]): its
    /// slots, what it offers the bar's search and History control, and its
    /// asks to edit the location or open the search. `app_id` is the one the
    /// launcher vouched for, so an app only ever reaches the bar of a tab
    /// that shows it. Returns `None` for a message about something else, else
    /// whether the desktop shows something new.
    pub fn receive_window_bar_message(
        &mut self,
        connection: u64,
        app_id: &str,
        message: &ui::floating_surface::FloatingSurfaceClientMessage,
    ) -> Option<bool> {
        use ui::floating_surface::FloatingSurfaceClientMessage as Message;
        Some(match message {
            Message::Toolbar { toolbar, instance } => {
                self.declare_app_toolbar(connection, app_id, *instance, toolbar.clone())
            }
            Message::EditLocation { instance } => self.edit_app_location(app_id, *instance),
            Message::WindowOffer { offer, instance } => {
                self.offer_app_window(connection, app_id, *instance, offer.clone())
            }
            Message::OpenSearch { instance, opening } => {
                self.open_app_window_search(app_id, *instance, *opening)
            }
            _ => return None,
        })
    }

    /// Show `toolbar` in tab `tab`'s bar as though its app declared it, for
    /// screenshots and tests that run no app.
    pub fn declare_tab_toolbar_for_tests(&mut self, tab: TabId, toolbar: Option<WindowToolbar>) {
        if self.window_toolbars.declare(tab, 0, toolbar) {
            self.note_window_place(tab);
        }
        self.sync_window_toolbars();
    }

    /// Every group shows its tabs' declared slots, and what the person did
    /// with them is queued for the apps. Runs every frame; a group whose
    /// slots did not change does nothing.
    pub(super) fn sync_window_toolbars(&mut self) {
        let groups = self.window_groups_mut();
        self.window_toolbars.sync_groups(groups);
    }

    /// Every window group on every output.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the window bars are written through `group_row_ptrs`, which is sound only under `&mut self`"
    )]
    fn window_groups_mut<'a>(&mut self) -> Vec<&'a mut WindowGroup> {
        self.group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &'a mut GroupRow = unsafe { &mut *ptr };
                row.groups.iter_mut()
            })
            .collect()
    }

    /// Look at the windows the shell lent the apps it mounts in its own
    /// process: show the slots each declared in its tab's bar, hand it what
    /// the person did with them, and close the tab of each that asked (its
    /// shell exited), the window with it when it was the only tab. Runs every
    /// frame, after the groups took this frame's input. Returns whether the
    /// desktop shows something new or an app has something to hear.
    pub(super) fn sync_hosted_windows(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            self.sync_window_toolbars();
            return false;
        };
        let windows = {
            let state = chrome.borrow();
            state
                .hosted_inputs
                .in_process()
                .map(|apps| apps.borrow().hosted_windows())
                .unwrap_or_default()
        };
        let groups = self.window_groups_mut();
        let sync = self.window_toolbars.sync_hosted(&windows, groups);
        for tab in &sync.declared {
            self.note_window_place(*tab);
        }
        if sync.searched {
            // An app with nothing to search in place hands on to the palette.
            self.harvest_pending_search_open();
            self.sync_window_toolbars();
        }
        let mut changed = sync.changed;
        let closing = sync.closing;
        if !closing.is_empty() {
            let mut state = chrome.borrow_mut();
            for tab in closing {
                if let Err(error) = crate::shell::window_actions::close_tab(&mut state, tab) {
                    tracing::debug!(?error, tab, "the tab of an ended session did not close");
                }
            }
            changed = true;
        }
        // The windows of tabs that are gone go with them.
        let revision = chrome.borrow().revision;
        if !windows.is_empty() && self.hosted_windows_revision != Some(revision) {
            self.hosted_windows_revision = Some(revision);
            let present: std::collections::HashSet<TabId> = {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| tab.id)
                    .collect()
            };
            let gone: Vec<TabId> = windows
                .iter()
                .map(|(tab, _)| *tab)
                .filter(|tab| !present.contains(tab))
                .collect();
            if !gone.is_empty() {
                if let Some(apps) = chrome.borrow().hosted_inputs.in_process() {
                    let mut apps = apps.borrow_mut();
                    for tab in &gone {
                        apps.forget_hosted_window(*tab);
                    }
                }
                for tab in gone {
                    self.window_toolbars.forget_hosted(tab);
                }
                self.sync_window_toolbars();
            }
        }
        changed
    }

    /// What goes back to the apps: their slots were used.
    pub fn take_toolbar_outbox(&mut self) -> Vec<(u64, ToolbarEvent)> {
        self.sync_window_toolbars();
        self.window_toolbars.take_outbox()
    }

    /// While a window's location is edited in its bar, its field has the
    /// keyboard (but for the Super chords, which stay the desktop's), and a
    /// press anywhere but on it puts the location back. Returns whether the
    /// field took `event`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the window bars are written through `group_row_ptrs`, which is sound only under `&mut self`"
    )]
    pub(super) fn route_window_location_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, WindowEvent};
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        );
        let keys = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) && !ctx.modifiers.state().super_key();
        if !press && !keys {
            return false;
        }
        let cursor = self.last_cursor;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                if !group.editing_location() {
                    continue;
                }
                if press {
                    if !cursor.is_some_and(|p| group.location_hit(p)) {
                        group.cancel_location_edit();
                    }
                    // The press goes on to whatever is under it.
                    return false;
                }
                return group.location_event(event, ctx);
            }
        }
        false
    }

    /// The pointer moved to `p`: the window bar's button under it gets its
    /// tooltip after the dwell.
    pub(super) fn bar_tooltip_pointer(&mut self, p: vello::kurbo::Point) {
        let target = self.group_row_ptrs.values().find_map(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups.iter().find_map(|group| group.bar_tooltip_at(p))
        });
        self.bar_tooltip.on_pointer(target, ui::time::now());
    }

    /// The tooltip of the window bar button under the pointer, over the
    /// windows. Each bar's own menus (overflow, suggestions) are drawn with
    /// its row in [`Self::draw_scene_sections`].
    pub(super) fn draw_bar_tooltip(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        self.bar_tooltip.tick(ui::time::now());
        if let Some((label, anchor)) = self.bar_tooltip.visible() {
            ui::widget::tooltip::draw_hover_tooltip(
                scene,
                transform,
                text_renderer,
                theme,
                anchor,
                label,
                self.rect,
            );
        }
    }

    /// A window's location is being edited in its bar: the shell takes the
    /// keyboard until it is committed or put back.
    pub(crate) fn window_location_editing(&self) -> bool {
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .any(ui::widget::WindowGroup::editing_location)
        })
    }
}
