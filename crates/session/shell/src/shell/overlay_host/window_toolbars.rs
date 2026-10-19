//! The slots Tilo apps put in their windows' bars ([`ui::window_toolbar`]):
//! an app declares them on its shell-ui connection, the shell hands them to
//! the window group whose tab shows the app, and what the person does with
//! them goes back to that app on the same connection.

use std::collections::HashMap;

use ui::widget::GroupRow;
use ui::window_toolbar::{ToolbarEvent, WindowToolbar};

use super::ShellOverlayHost;
use crate::shell::state::TabId;

/// Every tab's declared slots and the connection of the app that declared
/// them, and what goes back to the apps on the next pump.
#[derive(Default)]
pub(crate) struct WindowToolbars {
    by_tab: HashMap<TabId, (u64, WindowToolbar)>,
    /// Tabs whose slots went away since the groups last saw them.
    cleared: Vec<TabId>,
    outbox: Vec<(u64, ToolbarEvent)>,
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
        match toolbar
            .map(WindowToolbar::bounded)
            .filter(|toolbar| !toolbar.is_empty())
        {
            Some(toolbar) => {
                let next = (connection, toolbar);
                if self.by_tab.get(&tab) == Some(&next) {
                    return false;
                }
                self.by_tab.insert(tab, next);
                true
            }
            None => {
                let had = self.by_tab.remove(&tab).is_some();
                if had {
                    self.cleared.push(tab);
                }
                had
            }
        }
    }

    /// `connection` closed: its app's slots go with it.
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
        let any = !gone.is_empty();
        self.cleared.extend(gone);
        any
    }

    pub(crate) fn toolbar(&self, tab: TabId) -> Option<&WindowToolbar> {
        self.by_tab.get(&tab).map(|(_, toolbar)| toolbar)
    }

    /// Queue `event` for the app that declared `tab`'s slots.
    pub(crate) fn route(&mut self, tab: TabId, event: ToolbarEvent) -> bool {
        let Some((connection, _)) = self.by_tab.get(&tab) else {
            return false;
        };
        self.outbox.push((*connection, event));
        true
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
                }
                events.extend(group.take_toolbar_events());
                events.extend(group.take_location_drafts());
            }
        }
        for (tab, event) in events {
            self.window_toolbars.route(tab, event);
        }
    }

    /// What goes back to the apps: their slots were used.
    pub(crate) fn take_toolbar_outbox(&mut self) -> Vec<(u64, ToolbarEvent)> {
        self.sync_window_toolbars();
        self.window_toolbars.take_outbox()
    }

    /// While a window's location is edited in its bar, its field has the
    /// keyboard (but for the Super chords, which stay the desktop's), and a
    /// press anywhere but on it puts the location back. Returns whether the
    /// field took `event`.
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

    /// The windows' bars' menus (overflow, suggestions) and the tooltip of
    /// the button under the pointer, over the windows.
    pub(super) fn draw_window_bar_popups(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let rows: Vec<_> = self.group_row_ptrs.values().copied().collect();
        for ptr in rows {
            // SAFETY: see field doc on `group_row_ptrs`; drawing is
            // sequential and `&mut self` prevents another traversal.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                group.draw_bar_popups(scene, transform, text_renderer, theme, presence);
            }
        }
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
}
