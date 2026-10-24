//! Naming a host window's tab in place: the window menu's Rename tab and
//! the `tab.rename` action open a field in the tab's own pill
//! ([`ui::widget::TabBar::begin_rename`]); while it is open it has the
//! keyboard (but for the Super chords, which stay the desktop's), and what
//! it commits is recorded through [`crate::shell::window_actions::rename_tab`].

use ui::widget::GroupRow;

use super::ShellOverlayHost;
use super::tab_menu::TabMenuTarget;
use crate::shell::i18n::t;
use crate::shell::state::TabId;

impl ShellOverlayHost {
    /// Open the name field in `target`'s pill. Returns whether it opened.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the window bars are written through `group_row_ptrs`, which is sound only under `&mut self`"
    )]
    pub(super) fn begin_tab_rename(&mut self, target: TabMenuTarget) -> bool {
        let Some(&ptr) = self.group_row_ptrs.get(&target.strip) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.groups
            .iter_mut()
            .find(|group| group.id == target.group)
            .is_some_and(|group| group.begin_tab_rename(target.tab, &t("shell-tab-rename-field")))
    }

    /// Open the name field of `tab`, wherever its window stands.
    fn begin_tab_rename_of(&mut self, tab: TabId) -> bool {
        let target = self.group_row_ptrs.iter().find_map(|(&strip, &ptr)| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .find(|group| group.tabs().iter().any(|t| t.id == tab))
                .map(|group| TabMenuTarget {
                    strip,
                    group: group.id,
                    tab,
                })
        });
        target.is_some_and(|target| self.begin_tab_rename(target))
    }

    /// Open the field a `tab.rename` without a name asked for, and record
    /// each name committed in a pill since the last frame. Returns whether
    /// anything changed on screen.
    pub(super) fn harvest_tab_renames(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let requested = chrome.borrow_mut().tab_rename_request.take();
        let mut dirty = requested.is_some_and(|tab| self.begin_tab_rename_of(tab));
        let mut committed = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            committed.extend(
                row.groups
                    .iter_mut()
                    .filter_map(ui::widget::WindowGroup::take_tab_rename),
            );
        }
        for (tab, name) in committed {
            dirty = true;
            let mut st = chrome.borrow_mut();
            if let Err(err) = crate::shell::window_actions::rename_tab(&mut st, tab, &name) {
                st.push_warning_notification(
                    t("search-action-tab-rename-failed"),
                    err.user_message(),
                );
            }
        }
        dirty
    }

    /// While a tab is being named, its field has the keyboard (but for the
    /// Super chords), and a press anywhere but on it commits the name and
    /// goes on to whatever is under it. Returns whether the field took
    /// `event`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the window bars are written through `group_row_ptrs`, which is sound only under `&mut self`"
    )]
    pub(super) fn route_tab_rename_event(
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
                if group.renaming_tab().is_none() {
                    continue;
                }
                if press {
                    if !cursor.is_some_and(|p| group.tab_rename_hit(p)) {
                        group.commit_tab_rename();
                    }
                    // The press goes on: to the field through the bar, or to
                    // whatever else is under it.
                    return false;
                }
                return group.tab_rename_event(event, ctx);
            }
        }
        false
    }

    /// A tab is being named in its pill: the shell takes the keyboard until
    /// the name is committed or put back.
    pub fn tab_renaming(&self) -> bool {
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .any(|group| group.renaming_tab().is_some())
        })
    }
}
