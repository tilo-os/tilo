//! Applying queued navigation to the tree, then to the live rows.
//!
//! A command is admitted against the tree when it is dispatched (see
//! `crate::navigation`), and lands here on the next update pass: the tree takes it first, so
//! peers and the next session read the desk it leaves, and each row is told what changed in it,
//! in order, so its live widgets follow without being rebuilt.

use ui::automerge::{AutoCommit, ObjId};
use ui::search::Scope;
use ui::widget::Widget as _;

use tilo_desktop_model::{DesktopTree, WindowWidth};

use crate::navigation::{self, Admitted, RouteIds};

use super::paging::pan_row;
use super::tile_content::descriptor;
use super::{
    ACTIVITY_APP, DesktopSurface, find_group, find_tab, floats, launch, reveal_source, route_of,
    row_op::RowOp, search, tab_beside, window_menu,
};

impl DesktopSurface {
    /// Apply queued navigation to the tree, then to the live rows.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass: every command against the tree, then its widget follow-ups in order"
    )]
    pub(super) fn apply_commands(&mut self, state: &AutoCommit, path: &ObjId) {
        let queue = std::mem::take(&mut self.shared.borrow_mut().queue);
        if queue.is_empty() {
            return;
        }
        let mut ops = Vec::new();
        {
            let mut shared = self.shared.borrow_mut();
            for command in queue {
                let command = match command {
                    Admitted::Open(app) => navigation::resolve_open(&shared.tree, &app),
                    // A record already open in a window comes forward.
                    Admitted::Launch(app, float) => match self
                        .tiles
                        .showing(&app)
                        .and_then(|tab| route_of(&shared.tree, tab))
                    {
                        Some(ids) => Admitted::Select(ids),
                        None => Admitted::Launch(app, float),
                    },
                    // The workspace's history is the Activity app's to show: it opens, or comes
                    // forward, and is offered the view.
                    Admitted::Activity(_, view, author) => {
                        let link = crate::activity::ActivityDeepLink {
                            author,
                            ..crate::activity::ActivityDeepLink::default()
                        };
                        shared.activity_focus = Some((view, link));
                        navigation::resolve_open(&shared.tree, ACTIVITY_APP)
                    }
                    other => other,
                };
                // Admission ran at dispatch; an earlier command in this batch may since have
                // removed the target.
                let live =
                    |tree: &DesktopTree, ids: RouteIds| navigation::locate(tree, ids).is_some();
                match command {
                    Admitted::Select(ids) if live(&shared.tree, ids) => {
                        let tree = &mut shared.tree;
                        tree.active = ids.workspace;
                        tree.active_strip.insert(ids.workspace, ids.strip);
                        tree.set_active_group(ids.strip, ids.group);
                        if let Some(group) = find_group(tree, ids.strip, ids.group) {
                            group.activate_tab(ids.tab);
                        }
                        ops.push(RowOp::Show(ids.strip, ids.group, ids.tab));
                    }
                    Admitted::Width(ids, permille) if live(&shared.tree, ids) => {
                        if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group) {
                            group.width = WindowWidth::Fraction(permille);
                            ops.push(RowOp::Width(ids.strip, ids.group, group.width));
                        }
                    }
                    Admitted::Maximize(ids, maximized) if live(&shared.tree, ids) => {
                        if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group)
                            && group.maximized != maximized
                        {
                            group.maximized = maximized;
                            ops.push(RowOp::ToggleMaximized(ids.strip, ids.group));
                        }
                    }
                    Admitted::Fullscreen(ids, fullscreen) if live(&shared.tree, ids) => {
                        // The row reads this in `present` ; the window taken across the display
                        // takes the whole strip with it.
                        shared.tree.set_fullscreen(ids.strip, ids.group, fullscreen);
                    }
                    Admitted::Float(ids, on) if live(&shared.tree, ids) => {
                        // A chord, a menu row or an agent floats the window — a card back to the
                        // column, any other over the place it stands — or puts it back in its strip;
                        // the row reads it in `present` .
                        let lifted = match self.rows.get_mut(&ids.strip) {
                            Some(row) if on => {
                                floats::float(&mut shared, ids.strip, row, ids.group)
                            }
                            Some(_) => floats::set(&mut shared, ids.strip, ids.group, None),
                            None => false,
                        };
                        if lifted {
                            shared.tree.set_active_group(ids.strip, ids.group);
                        }
                        // A card back in the column shows the window beside it, clear of it.
                        if lifted && shared.dock.cards.contains_key(&ids.group) {
                            self.reveal = self
                                .rows
                                .get(&ids.strip)
                                .and_then(|row| floats::opened_beside(row, ids.group))
                                .map(|beside| (ids.strip, beside));
                        }
                    }
                    Admitted::Rename(ids, name) if live(&shared.tree, ids) => {
                        if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group)
                            && let Some(tab) = group.tabs.iter_mut().find(|tab| tab.id == ids.tab)
                            && tab.rename(&name)
                        {
                            // A name is what the window shows, not a window the desk gained or
                            // lost: the row renames its pill where it stands.
                            ops.push(RowOp::Rename {
                                strip: ids.strip,
                                group: ids.group,
                                tab: descriptor(tab),
                            });
                        }
                    }
                    Admitted::Close(ids) if live(&shared.tree, ids) => {
                        if let Some(removed) = shared.tree.close_tab(ids.tab) {
                            ops.push(RowOp::Close {
                                strip: removed.strip,
                                group: removed.group,
                                tab: ids.tab,
                                group_removed: removed.group_removed,
                            });
                        }
                    }
                    // The window comes back where it was, its app launched again if that is what it
                    // takes: the row takes it in as it takes a window a peer added, and the strip
                    // comes to it as it does to a launched one.
                    Admitted::Reopen(workspace) => {
                        let Some(reopened) = shared.tree.reopen_closed(workspace) else {
                            continue;
                        };
                        self.reveal = Some((reopened.strip, reopened.group));
                        shared.replaced = true;
                        // The window that closed is gone for good, so the tab comes back as the app
                        // that opens it again: the app is asked for, and the window that comes
                        // takes this tab.
                        if let Some((workspace, tab)) =
                            find_tab(&shared.tree, reopened.strip, reopened.tab)
                        {
                            self.tiles.reopened(workspace, tab);
                        }
                    }
                    Admitted::Strip(workspace, strip) => {
                        shared.tree.active = workspace;
                        shared.tree.active_strip.insert(workspace, strip);
                    }
                    Admitted::Workspace(workspace) => shared.tree.active = workspace,
                    Admitted::Launch(app, float) => {
                        match launch::open(self.tiles.as_mut(), &mut shared, &app, float) {
                            Some(op) => ops.push(op),
                            None => continue,
                        }
                    }
                    // The tab lands right after the one it was asked to open beside, inside its
                    // window group.
                    Admitted::TabOpenBeside(ids, app, background) if live(&shared.tree, ids) => {
                        match tab_beside::open_tab_beside(
                            self.tiles.as_mut(),
                            &mut shared,
                            ids,
                            &app,
                            background,
                        ) {
                            Some(op) => ops.push(op),
                            None => continue,
                        }
                    }
                    // Opening search changes no model state.
                    Admitted::Search(query) => {
                        shared.search_request = Some(search::SearchRequest::Open {
                            scope: Scope::Workspace,
                            query,
                        });
                        continue;
                    }
                    // Presentation only: the tree does not change.
                    Admitted::Pan(strip, offset) => {
                        ops.push(RowOp::Pan(strip, offset));
                        continue;
                    }
                    Admitted::Scroll(ids, action) if live(&shared.tree, ids) => {
                        ops.push(RowOp::Scroll(ids.strip, ids.group, action));
                        continue;
                    }
                    Admitted::History(ids, direction) if live(&shared.tree, ids) => {
                        ops.push(RowOp::History(ids.strip, ids.group, ids.tab, direction));
                        continue;
                    }
                    // The Activity app's windows close as any window does.
                    Admitted::CloseActivity(_) => {
                        while let Some(ids) = navigation::window_of_app(&shared.tree, ACTIVITY_APP)
                        {
                            let Some(removed) = shared.tree.close_tab(ids.tab) else {
                                break;
                            };
                            ops.push(RowOp::Close {
                                strip: removed.strip,
                                group: removed.group,
                                tab: ids.tab,
                                group_removed: removed.group_removed,
                            });
                        }
                    }
                    Admitted::NotificationCenter(open) => {
                        if let Some(notifications) = &shared.notifications {
                            notifications.ask_centre(open);
                        }
                        continue;
                    }
                    // A window of an app this device started runs in the workspace that started it,
                    // with its grants and its records: the tab does not cross by itself. The
                    // session carries it instead — asking first when the move crosses to or from a
                    // workspace other people are in — and this tab takes the window the app opens
                    // where the person put it.
                    Admitted::MoveToWorkspace(ids, to)
                        if live(&shared.tree, ids)
                            && shared.window_of_tab(ids.tab).is_some()
                            && self.tiles.carry(ids.tab, to) =>
                    {
                        continue;
                    }
                    // Everything else is a window menu's row: the desktop's own moves, widths and
                    // closes of one window.
                    other => {
                        if !window_menu::apply(&mut shared, &other, &mut ops) {
                            continue;
                        }
                    }
                }
                shared.changed();
            }
        }
        self.show_active_strip();
        for op in ops {
            match op {
                RowOp::Show(strip, group, tab) => {
                    if let Some(row) = self.rows.get_mut(&strip) {
                        row.set_group_active_tab(group, tab);
                        if self.settle.take() != Some(group) {
                            // A window focused off screen slides into frame: a key's move says
                            // where it went and is over at once within `fast` , a click glides, so
                            // focus never cuts the view.
                            let source = reveal_source();
                            row.scroll_group_into_view_animated(
                                group,
                                ui::motion::frame_now(),
                                source,
                            );
                        }
                    }
                }
                RowOp::Rename { strip, group, tab } => {
                    if let Some(widget) = self.group_widget(strip, group) {
                        widget.update_tab_descriptor(tab);
                    }
                }
                RowOp::Width(strip, group, width) => {
                    if let Some(group) = self.group_widget(strip, group) {
                        group.set_width(width);
                    }
                }
                RowOp::Open {
                    strip,
                    group,
                    index,
                    tab,
                    float,
                } => self.open_group(strip, group, index, &tab, float),
                RowOp::TabInGroup {
                    strip,
                    group,
                    index,
                    tab,
                    front,
                    background,
                } => self.insert_tab(strip, group, index, &tab, front, background),
                RowOp::ToggleMaximized(strip, group_id) => {
                    let restored = self.group_widget(strip, group_id).is_some_and(|group| {
                        group.toggle_maximized();
                        // The tree already holds the new state.
                        let _ = group.take_pending_maximize_change();
                        !group.maximized
                    });
                    if restored {
                        // Restored, the window comes back into view.
                        self.reveal = Some((strip, group_id));
                    }
                }
                RowOp::Close {
                    strip,
                    group,
                    tab,
                    group_removed,
                } => {
                    if let Some(group) = self.group_widget(strip, group) {
                        if group_removed {
                            group.start_closing();
                        } else {
                            group.request_close_tab(tab);
                        }
                    }
                }
                RowOp::Pan(strip, offset) => {
                    if self.shown == Some(strip)
                        && let Some(row) = self.rows.get_mut(&strip)
                        && pan_row(row, offset)
                    {
                        self.relayout = true;
                    }
                }
                RowOp::Scroll(strip, group, action) => {
                    if let Some(group) = self.group_widget(strip, group) {
                        let _ = group.apply_primary_vertical_scroll(action.into());
                    }
                }
                RowOp::History(strip, group, tab, direction) => {
                    if let Some(host) = self
                        .group_widget(strip, group)
                        .and_then(|group| group.host_for_tab_mut(tab))
                        && host.on_nav_gesture(direction, state, path)
                    {
                        self.structure_dirty = true;
                    }
                }
                RowOp::WindowShare(tab, share) => {
                    self.ask_window_share(tab, share);
                }
            }
        }
        // A row takes changes of its own while these land — the share a window
        // that opened beside another asks of the strip — and the record takes
        // them now: the pass that opened the window is the last until
        // something else moves, and the tree would keep the widths the strip
        // had before, which peers and the next session read as the desk's.
        self.harvest();
    }
}
