//! What a shortcut chord does: the desk's part of the shortcut table, each action carried out
//! through the verb its menu row, its palette row and an agent's invocation name, and the rest
//! handed to the session.

use tilo_desktop_model::layout::TileContent;
use tilo_desktop_surface::navigation::{
    ActivityView, ChromeEffect, DeskEffect, Destination, Route,
};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use super::DesktopChrome;
use crate::chrome::ChromeIntent;
use crate::host_contract::DesktopAsk;

impl DesktopChrome {
    /// Perform a desktop shortcut: false for one the desk has no part in (locking the device, the
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
            ShortcutAction::MaximizeWindow => self.maximize_focused(Some(true)),
            ShortcutAction::RestoreWindow => self.maximize_focused(Some(false)),
            // The window the person closed last comes back where it was; the desk answers the chord
            // either way, as the shell it replaces did.
            ShortcutAction::ReopenClosedTab => self.dispatch(ChromeEffect::ReopenClosed),
            ShortcutAction::ToggleMaximize => self.maximize_focused(None),
            ShortcutAction::ToggleFullscreen => self.toggle_fullscreen(),
            ShortcutAction::ToggleFloat => self.toggle_float(),
            // The tab leaves its window for one of its own, as the window menu's Split right does.
            ShortcutAction::DetachTab => self.split_focused_tab(),
            ShortcutAction::AttachTabLeft => self.attach_focused_tab(-1),
            ShortcutAction::AttachTabRight => self.attach_focused_tab(1),
            ShortcutAction::MoveGroupLeft => self.reorder_focused_group(-1),
            ShortcutAction::MoveGroupRight => self.reorder_focused_group(1),
            ShortcutAction::MoveGroupToPreviousStrip => self.move_focused_group_to_strip(-1),
            ShortcutAction::MoveGroupToNextStrip => self.move_focused_group_to_strip(1),
            ShortcutAction::MoveGroupToPreviousDisplay => self.move_focused_group_to_display(-1),
            ShortcutAction::MoveGroupToNextDisplay => self.move_focused_group_to_display(1),
            ShortcutAction::ResizeNarrower => self.resize_focused_group(-50),
            ShortcutAction::ResizeWider => self.resize_focused_group(50),
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
            // The rest belong to the session under the desk: the device, the surfaces over it and
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
    /// False for one the desk has no part in (a capture, the shortcut guide,
    /// cutting a remote control), which the session then answers.
    pub fn fixed_shortcut(&mut self, action: FixedAction) -> bool {
        match action {
            // The window in front goes, as its menu's Close row does.
            FixedAction::CloseTab => {
                if let Some(route) = self.focused_route() {
                    self.dispatch(DeskEffect::CloseTab { tab: route.tab });
                }
            }
            // Another window of the app in front, right after it, as the menu's New tab row opens
            // one; with no window, the app picker.
            FixedAction::DuplicateTab => {
                match self.focused_route().and_then(|route| {
                    let app = self.app_of(&route)?;
                    Some((app, route))
                }) {
                    Some((app, route)) => {
                        self.dispatch(ChromeEffect::OpenApp {
                            app,
                            beside: Some(route.tab),
                            background: None,
                            float: None,
                        });
                    }
                    None => self.open_the_app_picker(),
                }
            }
            // A new window is the app picker: the rule for where it stands is the desk's, and the
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

    /// Open the app picker, as an empty desk's own button does: search over the apps, for a new
    /// window of the one the person chooses, placed by the desk's rule. The desk carries this out
    /// itself; the surface's verb door refuses it.
    fn open_the_app_picker(&self) {
        self.handle
            .open_search(ui::search::Scope::WindowGroup(None), None);
    }

    /// Where the window in front stands, and the app it runs, when it runs one.
    fn app_of(&self, route: &Route) -> Option<String> {
        self.handle.read(|tree| {
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.slot.0.to_string() == route.workspace)?;
            let strip = workspace
                .strips
                .iter()
                .find(|strip| strip.id.0.to_string() == route.strip)?;
            let group = strip
                .window_groups
                .iter()
                .find(|group| group.id.to_string() == route.group)?;
            let tab = group
                .tabs
                .iter()
                .find(|tab| tab.id.to_string() == route.tab)?;
            match &tab.tile {
                TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
                    Some(app_id.clone())
                }
                _ => None,
            }
        })
    }

    /// The workspace shown, as the desk addresses it.
    fn active_workspace(&self) -> Option<String> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.slot.0.to_string())
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
            let strip_id = tree.active_strip.get(&workspace.slot).copied()?;
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
        self.dispatch(DeskEffect::Focus {
            tab: tab.to_string(),
        });
    }

    /// Detach the tab in front into a window of its own, right after the one it is in. A window of
    /// one tab has nothing to split: the desk says why rather than the chord doing nothing.
    fn split_focused_tab(&self) {
        let Some(route) = self.focused_route() else {
            return;
        };
        self.report(
            self.handle
                .dispatch(ChromeEffect::SplitRight { tab: route.tab }),
        );
    }

    /// Put the tab in front into the window `direction` places away in its strip, its tab standing
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

    /// Move the window in front one place along its strip: the keyboard twin of dragging its tab
    /// bar's ground.
    fn reorder_focused_group(&self, direction: isize) {
        let Some(route) = self.focused_route() else {
            return;
        };
        // The window stands before the one it lands in front of: the window that was before it when
        // it goes left, the one after the window it passes when it goes right, and the strip's end
        // when that was the last window.
        let before = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip = workspace
                .strips
                .iter()
                .find(|strip| strip.id.0.to_string() == route.strip)?;
            let at = strip
                .window_groups
                .iter()
                .position(|group| group.id.to_string() == route.group)?;
            let passed = isize::try_from(at).ok()?.checked_add(direction)?;
            strip.window_groups.get(usize::try_from(passed).ok()?)?;
            let land = if direction < 0 {
                at.checked_sub(1)?
            } else {
                at.checked_add(2)?
            };
            let before = strip
                .window_groups
                .get(land)
                .map(|group| group.id.to_string());
            Some(before)
        });
        let Some(before) = before else {
            return;
        };
        self.dispatch(ChromeEffect::MoveTab {
            tab: route.tab,
            to: Destination {
                strip: Some(route.strip),
                before,
                ..Destination::default()
            },
        });
    }

    /// Carry the window in front into the strip above (`steps < 0`) or below,
    /// landing last there; focus follows it.
    fn move_focused_group_to_strip(&self, steps: isize) {
        let Some(route) = self.focused_route() else {
            return;
        };
        let target = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let at = workspace
                .strips
                .iter()
                .position(|strip| strip.id.0.to_string() == route.strip)?;
            let index = isize::try_from(at).ok()?.checked_add(steps)?;
            let strip = workspace.strips.get(usize::try_from(index).ok()?)?;
            Some(strip.id.0.to_string())
        });
        let Some(strip) = target else {
            self.handle.acknowledge(crate::i18n::t("desk-window-edge"));
            return;
        };
        self.dispatch(ChromeEffect::MoveTab {
            tab: route.tab,
            to: Destination {
                strip: Some(strip),
                ..Destination::default()
            },
        });
    }

    /// Show the window in front on the display `steps` on.
    fn move_focused_group_to_display(&self, steps: isize) {
        let Some(route) = self.focused_route() else {
            return;
        };
        let target = self.handle.read(|tree| {
            let group = route.group.parse::<u64>().ok()?;
            let displays = &tree.outputs.displays;
            let at = displays
                .iter()
                .position(|display| Some(display.id) == tree.outputs.display_of(group))?;
            let index = isize::try_from(at).ok()?.checked_add(steps)?;
            let display = displays.get(usize::try_from(index).ok()?)?;
            Some(display.id.to_string())
        });
        let Some(display) = target else {
            self.handle.acknowledge(crate::i18n::t("desk-window-edge"));
            return;
        };
        self.dispatch(ChromeEffect::MoveTab {
            tab: route.tab,
            to: Destination {
                display: Some(display),
                ..Destination::default()
            },
        });
    }

    /// Give the window in front `delta` permille more or less of its strip.
    fn resize_focused_group(&self, delta: i64) {
        let snapshot = self.handle.snapshot(&self.generation, Vec::new());
        let (Some(route), Some(permille)) = (snapshot.active, snapshot.width_permille) else {
            return;
        };
        let next = i64::from(permille).saturating_add(delta);
        self.dispatch(ChromeEffect::SetWidth {
            group: route.group,
            permille: Some(u32::try_from(next).unwrap_or(permille)),
            preset: None,
        });
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

    /// The window in front, as the desk addresses it now.
    fn focused_route(&self) -> Option<tilo_desktop_surface::navigation::Route> {
        self.handle.snapshot(&self.generation, Vec::new()).active
    }

    /// Lift the focused window above the desk, or put it back into its strip.
    fn toggle_float(&self) {
        let floating = self.handle.read(|tree| -> Option<bool> {
            let workspace = tree.active_workspace()?;
            let strip = *tree.active_strip.get(&workspace.slot)?;
            let group = tree.active_group_for(strip)?;
            Some(workspace.arrangement.float_of(group).is_some())
        });
        let Some(floating) = floating else {
            return;
        };
        let Some(route) = self.handle.snapshot(&self.generation, Vec::new()).active else {
            return;
        };
        self.dispatch(ChromeEffect::Float {
            group: route.group,
            on: !floating,
        });
    }
}
