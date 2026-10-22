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

use std::collections::HashMap;

use ui::widget::GroupRow;
use ui::window_toolbar::{ToolbarEvent, WindowOffer, WindowToolbar};

use super::ShellOverlayHost;
use crate::shell::state::TabId;

/// The connection id the slots of an app the shell mounts in its own
/// process are kept under: what the person does with them goes back on its
/// window, never on a connection.
const IN_PROCESS: u64 = u64::MAX;

/// Every tab's declared slots and the connection of the app that declared
/// them, and what goes back to the apps on the next pump.
#[derive(Default)]
pub(crate) struct WindowToolbars {
    by_tab: HashMap<TabId, (u64, WindowToolbar)>,
    /// Tabs whose slots went away since the groups last saw them.
    cleared: Vec<TabId>,
    /// What each tab's app offers the bar's search and History control, and
    /// the connection it said so on.
    offers: HashMap<TabId, (u64, WindowOffer)>,
    /// Tabs whose offer went away since the groups last saw them.
    offers_cleared: Vec<TabId>,
    outbox: Vec<(u64, ToolbarEvent)>,
    /// For an app the shell mounts in its own process: the revision of its
    /// window's slots last taken, by tab.
    hosted_seen: HashMap<TabId, u64>,
    /// What goes back to apps the shell mounts in its own process, by tab.
    hosted_outbox: Vec<(TabId, ToolbarEvent)>,
}

impl WindowToolbars {
    /// `connection`'s app declared `toolbar` for `tab`. Returns whether it
    /// changed anything.
    pub(crate) fn declare(
        &mut self,
        tab: TabId,
        connection: u64,
        toolbar: Option<WindowToolbar>,
    ) -> bool {
        let Some(toolbar) = toolbar
            .map(WindowToolbar::bounded)
            .filter(|toolbar| !toolbar.is_empty())
        else {
            let had = self.by_tab.remove(&tab).is_some();
            if had {
                self.cleared.push(tab);
            }
            return had;
        };
        let next = (connection, toolbar);
        if self.by_tab.get(&tab) == Some(&next) {
            return false;
        }
        self.by_tab.insert(tab, next);
        true
    }

    /// `connection`'s app said what `tab`'s window offers the bar's search
    /// and History control. Returns whether it changed anything.
    pub(crate) fn offer(&mut self, tab: TabId, connection: u64, offer: WindowOffer) -> bool {
        let next = (connection, offer);
        if self.offers.get(&tab) == Some(&next) {
            return false;
        }
        self.offers.insert(tab, next);
        self.offers_cleared.retain(|cleared| *cleared != tab);
        true
    }

    /// What `tab`'s app offers the bar, if it runs in a process of its own.
    pub(crate) fn window_offer(&self, tab: TabId) -> Option<&WindowOffer> {
        self.offers.get(&tab).map(|(_, offer)| offer)
    }

    /// `connection` closed: its app's slots and offers go with it.
    pub(crate) fn disconnected(&mut self, connection: u64) -> bool {
        let gone: Vec<TabId> = self
            .by_tab
            .iter()
            .filter(|(_, (owner, _))| *owner == connection)
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &gone {
            self.by_tab.remove(tab);
        }
        let offered: Vec<TabId> = self
            .offers
            .iter()
            .filter(|(_, (owner, _))| *owner == connection)
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &offered {
            self.offers.remove(tab);
        }
        let any = !gone.is_empty() || !offered.is_empty();
        self.cleared.extend(gone);
        self.offers_cleared.extend(offered);
        any
    }

    pub(crate) fn toolbar(&self, tab: TabId) -> Option<&WindowToolbar> {
        self.by_tab.get(&tab).map(|(_, toolbar)| toolbar)
    }

    /// Queue `event` for the app that declared `tab`'s slots or offered
    /// its window to the bar.
    pub(crate) fn route(&mut self, tab: TabId, event: ToolbarEvent) -> bool {
        let connection = match &event {
            ToolbarEvent::Search { .. }
            | ToolbarEvent::SearchStep { .. }
            | ToolbarEvent::SearchClose
            | ToolbarEvent::History => self.offers.get(&tab).map(|(connection, _)| *connection),
            ToolbarEvent::ToggleSidebar
            | ToolbarEvent::Back
            | ToolbarEvent::Forward
            | ToolbarEvent::Commit { .. }
            | ToolbarEvent::Edit { .. }
            | ToolbarEvent::Action { .. } => {
                self.by_tab.get(&tab).map(|(connection, _)| *connection)
            }
        };
        let Some(connection) = connection else {
            return false;
        };
        if connection == IN_PROCESS {
            self.hosted_outbox.push((tab, event));
        } else {
            self.outbox.push((connection, event));
        }
        true
    }

    /// Tab `tab`'s app, mounted in the shell's process, declared `toolbar`
    /// on its window as revision `revision`. Returns whether it changed
    /// anything.
    fn declare_hosted(
        &mut self,
        tab: TabId,
        revision: u64,
        toolbar: Option<WindowToolbar>,
    ) -> bool {
        self.hosted_seen.insert(tab, revision);
        self.declare(tab, IN_PROCESS, toolbar)
    }

    /// Tab `tab`, mounted in the shell's process, is gone: its slots go.
    fn forget_hosted(&mut self, tab: TabId) {
        self.hosted_seen.remove(&tab);
        if self.by_tab.remove(&tab).is_some() {
            self.cleared.push(tab);
        }
    }

    pub(crate) fn take_outbox(&mut self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.outbox)
    }
}

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
            self.sync_window_toolbars();
        }
        changed
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
    /// search: its Ctrl+F. Only the tab the window shows searches.
    pub(crate) fn open_app_window_search(&mut self, app_id: &str, instance: Option<u64>) -> bool {
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
                opened = group.open_search_for_app(anchor.tab);
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
                self.offer_app_window(connection, app_id, *instance, *offer)
            }
            Message::OpenSearch { instance } => self.open_app_window_search(app_id, *instance),
            _ => return None,
        })
    }

    /// Show `toolbar` in tab `tab`'s bar as though its app declared it, for
    /// screenshots and tests that run no app.
    pub fn declare_tab_toolbar_for_tests(&mut self, tab: TabId, toolbar: Option<WindowToolbar>) {
        self.window_toolbars.declare(tab, 0, toolbar);
        self.sync_window_toolbars();
    }

    /// Every group shows its tabs' declared slots, and what the person did
    /// with them is queued for the apps. Runs every frame; a group whose
    /// slots did not change does nothing.
    pub(super) fn sync_window_toolbars(&mut self) {
        let cleared = std::mem::take(&mut self.window_toolbars.cleared);
        let offers_cleared = std::mem::take(&mut self.window_toolbars.offers_cleared);
        let mut events = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    if let Some(toolbar) = self.window_toolbars.toolbar(id) {
                        // Declarations are stored bounded, as the bar keeps
                        // them: an unchanged one is compared, not copied.
                        if group.tab_toolbar(id) != Some(toolbar) {
                            group.set_tab_toolbar(id, Some(toolbar.clone()));
                        }
                    } else if cleared.contains(&id) {
                        group.set_tab_toolbar(id, None);
                    }
                    if let Some(offer) = self.window_toolbars.window_offer(id) {
                        // Unchanged offers change nothing in the group.
                        group.set_tab_window_offer(id, Some(*offer));
                    } else if offers_cleared.contains(&id) {
                        group.set_tab_window_offer(id, None);
                    }
                }
                events.extend(group.take_toolbar_events());
                events.extend(group.take_location_drafts());
            }
        }
        for (tab, event) in events {
            self.window_toolbars.route(tab, event);
        }
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
        let mut changed = false;
        let mut closing = Vec::new();
        for (tab, window) in &windows {
            let seen = self.window_toolbars.hosted_seen.get(tab).copied();
            if let Some((revision, toolbar)) = window.toolbar_since(seen) {
                changed |= self.window_toolbars.declare_hosted(*tab, revision, toolbar);
            }
            if window.take_close() {
                closing.push(*tab);
            }
        }
        self.sync_window_toolbars();
        for (tab, event) in std::mem::take(&mut self.window_toolbars.hosted_outbox) {
            if let Some((_, window)) = windows.iter().find(|(shown, _)| *shown == tab) {
                window.deliver(event);
                // The app hears it on its next frame.
                changed = true;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ui::window_toolbar::ToolbarLocation;

    fn location(label: &str) -> WindowToolbar {
        WindowToolbar {
            sidebar: None,
            location: Some(ToolbarLocation {
                label: label.into(),
                editable: true,
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }
    }

    #[test]
    fn events_go_back_to_the_app_that_declared_the_tab_and_die_with_it() {
        let mut toolbars = WindowToolbars::default();
        assert!(toolbars.declare(7, 41, Some(location("example.org"))));
        assert!(!toolbars.declare(7, 41, Some(location("example.org"))));
        assert!(toolbars.route(
            7,
            ToolbarEvent::Commit {
                text: "rust".into()
            }
        ));
        assert!(
            !toolbars.route(8, ToolbarEvent::Back),
            "no app declared tab 8"
        );
        assert_eq!(
            toolbars.take_outbox(),
            vec![(
                41,
                ToolbarEvent::Commit {
                    text: "rust".into()
                }
            )]
        );
        assert!(toolbars.disconnected(41));
        assert!(toolbars.toolbar(7).is_none());
        assert_eq!(toolbars.cleared, vec![7]);
    }

    #[test]
    fn the_bars_search_and_history_go_to_the_app_that_offered_its_window() {
        use ui::window_toolbar::{HistoryOffer, SearchOffer};
        let mut toolbars = WindowToolbars::default();
        let offer = WindowOffer {
            search: SearchOffer::Document,
            count: None,
            history: Some(HistoryOffer::Closed),
        };
        // Notes declares no slots, only what its window offers the bar.
        assert!(toolbars.offer(3, 52, offer));
        assert!(
            !toolbars.offer(3, 52, offer),
            "an equal offer is nothing new"
        );
        assert!(toolbars.toolbar(3).is_none());
        for event in [
            ToolbarEvent::Search {
                query: "cat".into(),
            },
            ToolbarEvent::SearchStep { forward: true },
            ToolbarEvent::SearchClose,
            ToolbarEvent::History,
        ] {
            assert!(toolbars.route(3, event));
        }
        assert!(
            !toolbars.route(3, ToolbarEvent::Back),
            "slots nobody declared go nowhere"
        );
        let outbox = toolbars.take_outbox();
        assert_eq!(outbox.len(), 4);
        assert!(outbox.iter().all(|(connection, _)| *connection == 52));
        assert!(toolbars.disconnected(52));
        assert!(toolbars.window_offer(3).is_none());
        assert_eq!(toolbars.offers_cleared, vec![3]);
        assert!(!toolbars.route(3, ToolbarEvent::History));
    }
}
