//! What a shortcut chord does: the shell's part of the shortcut table, each action carried out
//! through the verb its menu row, its palette row and an agent's invocation name, and the rest
//! handed to the session.

use tilo_desktop_surface::surface::window_menu;
use tilo_session_core::desktop::navigation::{
    ActivityView, ChromeEffect, Destination, LayoutEffect, Route,
};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use super::DesktopChrome;
use crate::chrome::ChromeIntent;
use crate::host_contract::DesktopAsk;

impl DesktopChrome {
    /// Perform a shell shortcut: false for one the shell has no part in (locking the device, the
    /// magnifier, dictation, …), which the session then answers ([`crate::asks`]).
    /// Every shortcut of the table is named here or handed on: a shortcut the table gains without a
    /// handler does not build. Each names the verb its menu row, its palette row and an agent's
    /// invocation name, so a chord cannot drift into a behaviour of its own.
    pub fn shortcut(&mut self, action: ShortcutAction) -> bool {
        match action {
            ShortcutAction::WorkspaceOverview => self.toggle_overview(),
            ShortcutAction::EmojiPicker => self.toggle_emoji(),
            ShortcutAction::FocusPreviousWindow => self.step_focus(-1),
            ShortcutAction::FocusNextWindow => self.step_focus(1),
            ShortcutAction::PreviousTab => self.step_tab(-1),
            ShortcutAction::NextTab => self.step_tab(1),
            ShortcutAction::PreviousStrip => self.step_strip(-1),
            ShortcutAction::NextStrip => self.step_strip(1),
            ShortcutAction::PreviousWorkspace => {
                self.apply_intent(ChromeIntent::SwitchWorkspace(-1));
            }
            ShortcutAction::NextWorkspace => self.apply_intent(ChromeIntent::SwitchWorkspace(1)),
            ShortcutAction::MaximizeWindow => {
                self.choose_row(&[window_menu::MAXIMIZE]);
            }
            ShortcutAction::RestoreWindow => {
                self.choose_row(&[window_menu::RESTORE]);
            }
            // The window the person closed last comes back where it was; the shell answers the chord
            // either way, as the shell it replaces did.
            ShortcutAction::ReopenClosedTab => {
                self.dispatch(ChromeEffect::ReopenClosed { app: None });
            }
            ShortcutAction::ToggleMaximize => {
                self.choose_row(&[window_menu::MAXIMIZE, window_menu::RESTORE]);
            }
            ShortcutAction::ToggleFullscreen => {
                self.choose_row(&[window_menu::FULLSCREEN, window_menu::EXIT_FULLSCREEN]);
            }
            ShortcutAction::ToggleFloat => {
                self.choose_row(&[window_menu::FLOAT, window_menu::SNAP_BACK]);
            }
            // The tab leaves its window for one of its own, as the window menu's Split right does.
            ShortcutAction::DetachTab => self.split_focused_tab(),
            ShortcutAction::AttachTabLeft => self.attach_focused_tab(-1),
            ShortcutAction::AttachTabRight => self.attach_focused_tab(1),
            // One place along its strip: the keyboard twin of dragging its bar's ground.
            ShortcutAction::MoveGroupLeft => {
                self.move_focused_group_to(Some(window_menu::MOVE_LEFT.to_owned()));
            }
            ShortcutAction::MoveGroupRight => {
                self.move_focused_group_to(Some(window_menu::MOVE_RIGHT.to_owned()));
            }
            ShortcutAction::MoveGroupToPreviousStrip => self.move_focused_group_to_strip(-1),
            ShortcutAction::MoveGroupToNextStrip => self.move_focused_group_to_strip(1),
            ShortcutAction::MoveGroupToPreviousDisplay => self.move_focused_group_to_display(-1),
            ShortcutAction::MoveGroupToNextDisplay => self.move_focused_group_to_display(1),
            ShortcutAction::ResizeNarrower => {
                self.choose_row(&[window_menu::NARROWER]);
            }
            ShortcutAction::ResizeWider => {
                self.choose_row(&[window_menu::WIDER]);
            }
            ShortcutAction::OpenFiles => self.open_app("so.tilo.file-explorer"),
            ShortcutAction::OpenTerminal => self.open_app("so.tilo.terminal"),
            ShortcutAction::ReportProblem => self.ask(DesktopAsk::ReportProblem),
            ShortcutAction::FocusGroup1 => self.focus_group(0),
            ShortcutAction::FocusGroup2 => self.focus_group(1),
            ShortcutAction::FocusGroup3 => self.focus_group(2),
            ShortcutAction::FocusGroup4 => self.focus_group(3),
            ShortcutAction::FocusGroup5 => self.focus_group(4),
            ShortcutAction::FocusGroup6 => self.focus_group(5),
            ShortcutAction::FocusGroup7 => self.focus_group(6),
            ShortcutAction::FocusGroup8 => self.focus_group(7),
            ShortcutAction::FocusGroup9 => self.focus_group(8),
            ShortcutAction::FocusStrip1 => self.focus_strip(0),
            ShortcutAction::FocusStrip2 => self.focus_strip(1),
            ShortcutAction::FocusStrip3 => self.focus_strip(2),
            ShortcutAction::FocusStrip4 => self.focus_strip(3),
            ShortcutAction::FocusStrip5 => self.focus_strip(4),
            ShortcutAction::FocusStrip6 => self.focus_strip(5),
            ShortcutAction::FocusStrip7 => self.focus_strip(6),
            ShortcutAction::FocusStrip8 => self.focus_strip(7),
            ShortcutAction::FocusStrip9 => self.focus_strip(8),
            // The rest belong to the session under the shell: the device, the surfaces over it and
            // the people and agents here.
            ShortcutAction::Search
            | ShortcutAction::Lock
            | ShortcutAction::QuickSettings
            | ShortcutAction::ShortcutHelp
            | ShortcutAction::TabMenu
            | ShortcutAction::ClipboardHistory
            | ShortcutAction::Notifications
            | ShortcutAction::PowerMenu
            | ShortcutAction::NextInputSource
            | ShortcutAction::ToggleMagnifier
            | ShortcutAction::MagnifierZoomIn
            | ShortcutAction::MagnifierZoomOut
            | ShortcutAction::AskAgent
            | ShortcutAction::SaySomething
            | ShortcutAction::DictationToggle
            | ShortcutAction::DictationFinish
            | ShortcutAction::DictationCancel => {
                self.ask(DesktopAsk::Shortcut(action));
                return false;
            }
        }
        true
    }

    /// Perform a fixed desktop shortcut: a chord Settings cannot rebind.
    /// False for one the shell has no part in (a capture, the shortcut guide,
    /// cutting a remote control), which the session then answers.
    pub fn fixed_shortcut(&mut self, action: FixedAction) -> bool {
        match action {
            // The window in front goes, as its menu's Close row does.
            FixedAction::CloseTab => {
                self.choose_row(&[window_menu::CLOSE, window_menu::CLOSE_WINDOW]);
            }
            // Another window of the app in front, right after it, as the menu's New tab row opens
            // one; with no window, the app picker.
            FixedAction::DuplicateTab => {
                if !self.choose_row(&[window_menu::NEW_TAB]) {
                    self.open_the_app_picker();
                }
            }
            // A new window is the app picker: the rule for where it stands is the shell's, and the
            // person chooses the app.
            FixedAction::NewWindow => self.open_the_app_picker(),
            FixedAction::OpenTerminal => self.open_app("so.tilo.terminal"),
            FixedAction::StripSwitcher => {
                self.apply_intent(crate::chrome::ChromeIntent::ToggleStrips);
            }
            // The workspace's history, where the person is: what Super+H showed of what happened
            // here.
            FixedAction::WorkspaceHistory | FixedAction::ExploreWorkspaceHistory => {
                let view = if action == FixedAction::WorkspaceHistory {
                    ActivityView::CatchUp
                } else {
                    ActivityView::Explore
                };
                if let Some(workspace) = self.active_workspace() {
                    self.dispatch(ChromeEffect::Activity {
                        workspace,
                        view,
                        author: None,
                    });
                }
            }
            FixedAction::CaptureScreenshot
            | FixedAction::PictureInPicture
            | FixedAction::SaveRecentReplay
            | FixedAction::ShortcutGuide
            | FixedAction::StopRemoteInput => {
                self.ask(DesktopAsk::FixedShortcut(action));
                return false;
            }
        }
        true
    }

    /// Open the app picker, as an empty shell's own button does: search over the apps, for a new
    /// window of the one the person chooses, placed by the shell's rule. The shell carries this out
    /// itself; the surface's verb API refuses it.
    fn open_the_app_picker(&self) {
        self.handle
            .open_search(ui::search::Scope::WindowGroup(None), None);
    }

    /// The workspace shown, as the shell addresses it.
    fn active_workspace(&self) -> Option<String> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    }

    /// Put the keyboard on the window group `index` of the shown strip, as a click on its window
    /// does. A strip with fewer windows leaves the focus where it was.
    fn focus_group(&self, index: usize) {
        if let Some(tab) = self.window_tab(None, Some(index)) {
            self.handle.focus_tab(tab);
        }
    }

    /// Bring strip `index` of the shown workspace to the front of its display;
    /// a workspace with fewer strips leaves the focus where it was.
    fn focus_strip(&self, index: usize) {
        self.go_to_strip(|_| Some(index));
    }

    /// Step the windows of the shown strip by `steps` : the focus follows along it, as the window
    /// menu's row and a click on a window do.
    fn step_focus(&self, steps: isize) {
        if let Some(tab) = self.window_tab(Some(steps), None) {
            self.handle.focus_tab(tab);
        }
    }

    /// Cycle the tabs of the window in front by `steps` , wrapping as the tab bar does: its ring is
    /// small, so the last tab leads back to the first.
    fn step_tab(&self, steps: isize) {
        let next = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip.get(&workspace.id()).copied()?;
            let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
            let focused = tree.active_group_for(strip.id)?;
            let group = strip
                .window_groups
                .iter()
                .find(|group| group.id == focused)?;
            let len = isize::try_from(group.tabs.len())
                .ok()
                .filter(|len| *len > 0)?;
            let current = group
                .tabs
                .iter()
                .position(|tab| tab.id == group.active_tab)
                .unwrap_or(0);
            let next = isize::try_from(current)
                .ok()?
                .checked_add(steps)?
                .rem_euclid(len);
            group
                .tabs
                .get(usize::try_from(next).ok()?)
                .map(|tab| tab.id)
        });
        let Some(tab) = next else {
            return;
        };
        self.dispatch(LayoutEffect::Focus {
            tab: tab.to_string(),
        });
    }

    /// Detach the tab in front into a window of its own, right after the one it is in. A window of
    /// one tab has nothing to split: the shell says why rather than the chord doing nothing.
    fn split_focused_tab(&self) {
        let Some(route) = self.focused_route() else {
            return;
        };
        self.report(
            self.handle
                .dispatch(ChromeEffect::SplitRight { tab: route.tab }),
        );
    }

    /// Put the tab in front into the window `direction` places away in its strip, its tab
    /// last there.
    fn attach_focused_tab(&self, direction: isize) {
        let Some(route) = self.focused_route() else {
            return;
        };
        let into = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip = workspace
                .strips
                .iter()
                .find(|strip| strip.id.0.to_string() == route.strip)?;
            let at = strip
                .window_groups
                .iter()
                .position(|group| group.id.to_string() == route.group)?;
            let index = isize::try_from(at).ok()?.checked_add(direction)?;
            let group = strip.window_groups.get(usize::try_from(index).ok()?)?;
            Some(group.id.to_string())
        });
        let Some(into) = into else {
            return;
        };
        self.dispatch(ChromeEffect::MoveTab {
            tab: route.tab,
            to: Destination {
                group: Some(into),
                ..Destination::default()
            },
        });
    }

    /// Do to the window in front what the first of `rows` its menu offers does: a chord is its menu
    /// row's twin and runs the row's own command. Whether the window offered one of them.
    fn choose_row(&self, rows: &[&str]) -> bool {
        let Some(tab) = self
            .focused_route()
            .and_then(|route| route.tab.parse::<u64>().ok())
        else {
            return false;
        };
        rows.iter()
            .any(|row| self.handle.choose_window_menu(tab, row, None).is_ok())
    }

    /// Carry the window in front into the strip above (`steps < 0`) or below, landing last there,
    /// as its menu's row for that strip does; focus follows it. Below the last strip is a new one,
    /// as its menu's New strip row makes it.
    fn move_focused_group_to_strip(&self, steps: isize) {
        let next = self.focused_route().and_then(|route| {
            self.handle.read(|tree| {
                let strips = tree.active_workspace().map_or(&[][..], |w| &w.strips[..]);
                let at = strips
                    .iter()
                    .position(|strip| strip.id.0.to_string() == route.strip)?;
                Some(
                    strips
                        .get(at.checked_add_signed(steps)?)
                        .map(|strip| window_menu::move_to_strip_id(strip.id)),
                )
            })
        });
        self.move_focused_group_to(match next {
            Some(None) if steps > 0 => Some(window_menu::MOVE_TO_NEW_STRIP.to_owned()),
            next => next.flatten(),
        });
    }

    /// Carry the window `tab` into a new strip below the last one, made as the strip switcher's
    /// New strip makes it: the one move its menu's New strip row and the chord past the last strip
    /// ask for.
    pub(super) fn move_to_new_strip(&self, tab: tilo_desktop_model::TabId) {
        let moved = self.add_strip(String::new()).is_some_and(|strip| {
            self.handle
                .choose_window_menu(tab, &window_menu::move_to_strip_id(strip), None)
                .is_ok()
        });
        if !moved {
            self.handle.acknowledge(crate::i18n::t("shell-window-edge"));
        }
    }

    /// Show the window in front on the display that stands `steps` displays to its right (left
    /// when negative); with none that way, say the window can go no further.
    fn move_focused_group_to_display(&self, steps: i32) {
        if !self.handle.move_focused_to_display(steps) {
            self.handle.acknowledge(crate::i18n::t("shell-window-edge"));
        }
    }

    /// Choose the menu row `row` carrying the window in front somewhere; with no such place, say
    /// the window can go no further that way.
    fn move_focused_group_to(&self, row: Option<String>) {
        if !row.is_some_and(|row| self.choose_row(&[&row])) {
            self.handle.acknowledge(crate::i18n::t("shell-window-edge"));
        }
    }

    /// Open the app `app` in the shown strip, as its launcher card does.
    fn open_app(&self, app: &str) {
        self.dispatch(ChromeEffect::OpenApp {
            app: app.to_owned(),
            beside: None,
            background: None,
            float: None,
        });
    }

    /// The window in front, as the shell addresses it now.
    fn focused_route(&self) -> Option<Route> {
        self.handle.snapshot(&self.generation, Vec::new()).active
    }
}
