//! What a shortcut chord does to the desktop. Each shortcut runs the verb that its menu row, its
//! palette row and an agent's invocation name run, so a chord cannot drift into behaviour of its
//! own. The session performs these on the desktop it owns. A shell, whether drawn in Rust or in
//! tilolang, only answers what is left: the surfaces it draws itself.

use ui::shortcut_table::{FixedAction, ShortcutAction};

use super::DesktopHandle;
use super::window_menu;
use crate::desktop::navigation::{ActivityView, ChromeEffect, Destination, LayoutEffect, Route};

/// What became of a shortcut the desktop was asked to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Performed {
    /// The desktop carried it out, or said why it could not.
    Done,
    /// A surface the shell draws itself: the overview, the emoji picker or the strip switcher.
    Shell(ShellPart),
    /// The shell's app picker, for a new window of the app the person chooses.
    AppPicker,
    /// Not the desktop's: the device, the surfaces over it, and the people and agents here.
    Session,
}

/// A surface the shell draws and opens itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellPart {
    Overview,
    EmojiPicker,
    StripSwitcher,
    ReportProblem,
}

impl DesktopHandle {
    /// Perform the shortcut `action` on the desktop. A shortcut the table gains without a handler
    /// does not build.
    pub fn perform_shortcut(&self, action: ShortcutAction) -> Performed {
        match action {
            ShortcutAction::WorkspaceOverview => return Performed::Shell(ShellPart::Overview),
            ShortcutAction::EmojiPicker => return Performed::Shell(ShellPart::EmojiPicker),
            ShortcutAction::ReportProblem => return Performed::Shell(ShellPart::ReportProblem),
            ShortcutAction::FocusPreviousWindow => self.step_window_focus(-1),
            ShortcutAction::FocusNextWindow => self.step_window_focus(1),
            ShortcutAction::PreviousTab => self.step_tab(-1),
            ShortcutAction::NextTab => self.step_tab(1),
            ShortcutAction::PreviousStrip => self.step_strip(-1),
            ShortcutAction::NextStrip => self.step_strip(1),
            ShortcutAction::PreviousWorkspace => self.step_workspace(-1),
            ShortcutAction::NextWorkspace => self.step_workspace(1),
            ShortcutAction::MaximizeWindow => {
                self.choose_focused_row(&[window_menu::MAXIMIZE]);
            }
            ShortcutAction::RestoreWindow => {
                self.choose_focused_row(&[window_menu::RESTORE]);
            }
            // The window the person closed last comes back where it was.
            ShortcutAction::ReopenClosedTab => {
                self.perform(ChromeEffect::ReopenClosed {
                    app: None,
                    window: None,
                });
            }
            ShortcutAction::ToggleMaximize => {
                self.choose_focused_row(&[window_menu::MAXIMIZE, window_menu::RESTORE]);
            }
            ShortcutAction::ToggleFullscreen => {
                self.choose_focused_row(&[window_menu::FULLSCREEN, window_menu::EXIT_FULLSCREEN]);
            }
            ShortcutAction::ToggleFloat => {
                self.choose_focused_row(&[window_menu::FLOAT, window_menu::SNAP_BACK]);
            }
            // The tab leaves its window for one of its own, as the window menu's Split right does.
            ShortcutAction::DetachTab => self.split_focused_tab(),
            ShortcutAction::AttachTabLeft => self.attach_focused_tab(-1),
            ShortcutAction::AttachTabRight => self.attach_focused_tab(1),
            // One place along its strip: the keyboard twin of dragging its bar's ground.
            ShortcutAction::MoveGroupLeft => {
                self.move_focused_to_row(Some(window_menu::MOVE_LEFT.to_owned()));
            }
            ShortcutAction::MoveGroupRight => {
                self.move_focused_to_row(Some(window_menu::MOVE_RIGHT.to_owned()));
            }
            ShortcutAction::MoveGroupToPreviousStrip => self.move_focused_to_strip(-1),
            ShortcutAction::MoveGroupToNextStrip => self.move_focused_to_strip(1),
            ShortcutAction::MoveGroupToPreviousDisplay => self.move_focused_display(-1),
            ShortcutAction::MoveGroupToNextDisplay => self.move_focused_display(1),
            ShortcutAction::ResizeNarrower => {
                self.choose_focused_row(&[window_menu::NARROWER]);
            }
            ShortcutAction::ResizeWider => {
                self.choose_focused_row(&[window_menu::WIDER]);
            }
            ShortcutAction::OpenFiles => self.open_app_here("so.tilo.file-explorer"),
            ShortcutAction::OpenTerminal => self.open_app_here("so.tilo.terminal"),
            ShortcutAction::FocusGroup1 => self.focus_nth_window(0),
            ShortcutAction::FocusGroup2 => self.focus_nth_window(1),
            ShortcutAction::FocusGroup3 => self.focus_nth_window(2),
            ShortcutAction::FocusGroup4 => self.focus_nth_window(3),
            ShortcutAction::FocusGroup5 => self.focus_nth_window(4),
            ShortcutAction::FocusGroup6 => self.focus_nth_window(5),
            ShortcutAction::FocusGroup7 => self.focus_nth_window(6),
            ShortcutAction::FocusGroup8 => self.focus_nth_window(7),
            ShortcutAction::FocusGroup9 => self.focus_nth_window(8),
            ShortcutAction::FocusStrip1 => self.focus_nth_strip(0),
            ShortcutAction::FocusStrip2 => self.focus_nth_strip(1),
            ShortcutAction::FocusStrip3 => self.focus_nth_strip(2),
            ShortcutAction::FocusStrip4 => self.focus_nth_strip(3),
            ShortcutAction::FocusStrip5 => self.focus_nth_strip(4),
            ShortcutAction::FocusStrip6 => self.focus_nth_strip(5),
            ShortcutAction::FocusStrip7 => self.focus_nth_strip(6),
            ShortcutAction::FocusStrip8 => self.focus_nth_strip(7),
            ShortcutAction::FocusStrip9 => self.focus_nth_strip(8),
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
            | ShortcutAction::DictationCancel => return Performed::Session,
        }
        Performed::Done
    }

    /// Perform the fixed shortcut `action`, a chord Settings cannot rebind.
    pub fn perform_fixed_shortcut(&self, action: FixedAction) -> Performed {
        match action {
            // The window in front goes, as its menu's Close row does.
            FixedAction::CloseTab => {
                self.choose_focused_row(&[window_menu::CLOSE, window_menu::CLOSE_WINDOW]);
            }
            // Another window of the app in front, right after it, as the menu's New tab row opens
            // one; with no window, the app picker.
            FixedAction::DuplicateTab => {
                if !self.choose_focused_row(&[window_menu::NEW_TAB]) {
                    return Performed::AppPicker;
                }
            }
            // A new window is the app picker: where it stands is the shell's rule, and the person
            // chooses the app.
            FixedAction::NewWindow => return Performed::AppPicker,
            FixedAction::OpenTerminal => self.open_app_here("so.tilo.terminal"),
            FixedAction::StripSwitcher => return Performed::Shell(ShellPart::StripSwitcher),
            FixedAction::WorkspaceHistory | FixedAction::ExploreWorkspaceHistory => {
                let view = if action == FixedAction::WorkspaceHistory {
                    ActivityView::CatchUp
                } else {
                    ActivityView::Explore
                };
                if let Some(workspace) = self.shown_workspace() {
                    self.perform(ChromeEffect::Activity {
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
            | FixedAction::StopRemoteInput => return Performed::Session,
        }
        Performed::Done
    }

    /// Admit `verb`; a refusal is the desktop's state moving under the chord, not the person's
    /// to read.
    fn perform(&self, verb: impl Into<crate::desktop::navigation::Verb>) {
        if let Err(error) = self.dispatch(verb) {
            tracing::warn!(%error, "desktop shortcut");
        }
    }

    /// The workspace shown, as the shell addresses it.
    fn shown_workspace(&self) -> Option<String> {
        self.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    }

    /// The window in front, as the shell addresses it now.
    fn focused_route(&self) -> Option<Route> {
        self.snapshot("", Vec::new()).active
    }

    /// The window `step` places from the focused one in the shown strip, or the strip's `nth`.
    #[must_use]
    pub fn window_tab(
        &self,
        step: Option<isize>,
        nth: Option<usize>,
    ) -> Option<tilo_desktop_model::TabId> {
        self.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip_for(workspace)?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let index = match (step, nth) {
                (_, Some(nth)) => nth,
                (Some(step), None) => {
                    let focused = tree
                        .active_group_for(strip.id)
                        .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
                        .unwrap_or(0);
                    focused.checked_add_signed(step)?
                }
                (None, None) => return None,
            };
            strip.window_groups.get(index).map(|group| group.active_tab)
        })
    }

    /// Put the keyboard on window `index` of the shown strip, as a click on it does. A strip with
    /// fewer windows leaves the focus where it was.
    fn focus_nth_window(&self, index: usize) {
        if let Some(tab) = self.window_tab(None, Some(index)) {
            self.focus_tab(tab);
        }
    }

    /// Step the focus along the shown strip by `steps`.
    fn step_window_focus(&self, steps: isize) {
        if let Some(tab) = self.window_tab(Some(steps), None) {
            self.focus_tab(tab);
        }
    }

    /// Cycle the tabs of the window in front by `steps`, wrapping as the tab bar does.
    fn step_tab(&self, steps: isize) {
        let next = self.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip_for(workspace)?;
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
        if let Some(tab) = next {
            self.perform(LayoutEffect::Focus {
                tab: tab.to_string(),
            });
        }
    }

    /// Go to the strip `pick` chooses from the shown one's index; whether there was one.
    pub fn go_to_strip(&self, pick: impl FnOnce(usize) -> Option<usize>) -> bool {
        let next = self.read(|tree| {
            let workspace = tree.active_workspace()?;
            let current = tree
                .active_strip_for(workspace)
                .and_then(|id| workspace.strips.iter().position(|s| s.id == id))
                .unwrap_or(0);
            let strip = workspace.strips.get(pick(current)?)?;
            Some((workspace.id().to_string(), strip.id.0.to_string()))
        });
        let Some((workspace, strip)) = next else {
            return false;
        };
        self.perform(ChromeEffect::ShowStrip { workspace, strip });
        true
    }

    /// Bring strip `index` of the shown workspace to the front; with fewer strips nothing moves.
    fn focus_nth_strip(&self, index: usize) {
        self.go_to_strip(|_| Some(index));
    }

    /// Step strips by `steps`, saying so past an end.
    pub fn step_strip(&self, steps: isize) {
        if !self.go_to_strip(|current| current.checked_add_signed(steps)) {
            match steps.signum() {
                1 => self.acknowledge(crate::desktop::i18n::t("shell-last-strip")),
                -1 => self.acknowledge(crate::desktop::i18n::t("shell-first-strip")),
                _ => {}
            }
        }
    }

    /// Step workspaces by `delta`, saying so past an end.
    pub fn step_workspace(&self, delta: i32) {
        let next = self.read(|tree| {
            let index = tree.workspaces.iter().position(|w| w.id() == tree.active)?;
            let next = index.checked_add_signed(isize::try_from(delta).ok()?)?;
            tree.workspaces.get(next).map(|w| w.id().to_string())
        });
        match next {
            Some(workspace) => self.perform(ChromeEffect::SwitchWorkspace {
                workspace: Some(workspace),
                delta: None,
            }),
            None if delta > 0 => self.acknowledge(crate::desktop::i18n::t("shell-last-workspace")),
            None if delta < 0 => {
                self.acknowledge(crate::desktop::i18n::t("shell-first-workspace"));
            }
            None => {}
        }
    }

    /// Detach the tab in front into a window of its own, right after the one it is in.
    fn split_focused_tab(&self) {
        // A window of one tab has nothing to split: the person is told why.
        if let Some(route) = self.focused_route()
            && let Err(refusal) = self.dispatch(ChromeEffect::SplitRight { tab: route.tab })
        {
            self.acknowledge(refusal);
        }
    }

    /// Put the tab in front into the window `direction` places away in its strip, last there.
    fn attach_focused_tab(&self, direction: isize) {
        let Some(route) = self.focused_route() else {
            return;
        };
        let into = self.read(|tree| {
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
        if let Some(into) = into {
            self.perform(ChromeEffect::MoveTab {
                tab: route.tab,
                to: Destination {
                    group: Some(into),
                    ..Destination::default()
                },
            });
        }
    }

    /// Do to the window in front what the first of `rows` its menu offers does: a chord is its
    /// menu row's twin. Whether the window offered one of them.
    fn choose_focused_row(&self, rows: &[&str]) -> bool {
        let Some(tab) = self
            .focused_route()
            .and_then(|route| route.tab.parse::<u64>().ok())
        else {
            return false;
        };
        rows.iter()
            .any(|row| self.choose_window_menu(tab, row, None).is_ok())
    }

    /// Carry the window in front into the strip above (`steps < 0`) or below, landing last there.
    /// Below the last strip is a new one, as its menu's New strip row asks: a window alone in the
    /// last strip has nowhere to go.
    fn move_focused_to_strip(&self, steps: isize) {
        let Some(route) = self.focused_route() else {
            self.acknowledge(crate::desktop::i18n::t("shell-window-edge"));
            return;
        };
        let next = self.read(|tree| {
            let strips = tree.active_workspace().map_or(&[][..], |w| &w.strips[..]);
            let at = strips
                .iter()
                .position(|strip| strip.id.0.to_string() == route.strip)?;
            Some(
                strips
                    .get(at.checked_add_signed(steps)?)
                    .map(|strip| window_menu::move_to_strip_id(strip.id)),
            )
        });
        self.move_focused_to_row(match next {
            Some(None) if steps > 0 => Some(window_menu::MOVE_TO_NEW_STRIP.to_owned()),
            next => next.flatten(),
        });
    }

    /// Show the window in front on the display `steps` to its right (left when negative).
    fn move_focused_display(&self, steps: i32) {
        if !self.move_focused_to_display(steps) {
            self.acknowledge(crate::desktop::i18n::t("shell-window-edge"));
        }
    }

    /// Choose the menu row `row` that carries the window in front somewhere; with no such place,
    /// say the window can go no further that way.
    fn move_focused_to_row(&self, row: Option<String>) {
        if !row.is_some_and(|row| self.choose_focused_row(&[&row])) {
            self.acknowledge(crate::desktop::i18n::t("shell-window-edge"));
        }
    }

    /// Open the app `app` in the shown strip, as its launcher card does.
    fn open_app_here(&self, app: &str) {
        self.perform(ChromeEffect::OpenApp {
            app: app.to_owned(),
            beside: None,
            background: None,
            float: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::state::{DesktopHandle, DesktopPass, TileSource};
    use tilo_desktop_model::{
        Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };
    use ui::shortcut_table::{FixedAction, ShortcutAction, ShortcutHome};

    fn window(id: u64, title: &str, app: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
        let active_tab = tabs[0].id;
        WindowGroup {
            id,
            tabs,
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn layout_tree(strips: Vec<Strip>) -> Layout {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
        workspace.strips = strips;
        let mut tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Default::default()
        };
        let first = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(WorkspaceId::from_u128(1), first);
        if let Some(group) = tree.workspaces[0].strips[0].window_groups.first() {
            tree.active_group.insert(first, group.id);
        }
        tree
    }

    /// One strip with two windows: one of two tabs, and one of one tab.
    fn two_windows() -> Layout {
        layout_tree(vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![
                group(
                    3,
                    vec![
                        window(20, "Notes", "so.tilo.notes"),
                        window(21, "Mail", "so.tilo.mail"),
                    ],
                ),
                group(4, vec![window(30, "Chat", "so.tilo.chat")]),
            ],
        }])
    }

    /// A shell every place a chord reaches exists on: nine strips, the shown one holding nine windows,
    /// so numbering a strip or a window lands somewhere and moving to the strip above or below does
    /// too.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "a fixture's own ids, small whole numbers"
    )]
    fn every_place() -> Layout {
        let groups: Vec<WindowGroup> = (0..9_u64)
            .map(|index| {
                let id = index + 1;
                group(
                    id,
                    vec![
                        window(100 + index * 2, "Left", "so.tilo.notes"),
                        window(101 + index * 2, "Right", "so.tilo.mail"),
                    ],
                )
            })
            .collect();
        // Nine strips: the shown one holds the nine windows, and the eight below it one window each.
        let mut strips: Vec<Strip> = (1..9_u64)
            .map(|index| Strip {
                id: StripId(10 + index),
                name: format!("Strip {index}"),
                window_groups: vec![group(
                    20 + index,
                    vec![window(200 + index, "Window", "so.tilo.notes")],
                )],
            })
            .collect();
        strips.insert(
            0,
            Strip {
                id: StripId(2),
                name: "Main".into(),
                window_groups: groups,
            },
        );
        layout_tree(strips)
    }

    struct Notices;

    impl TileSource for Notices {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The passes a shell's queued commands take to reach its tree.
    fn settle(handle: &DesktopHandle) {
        DesktopPass::new(handle.clone()).step(&mut Notices);
    }

    fn in_front(handle: &DesktopHandle) -> Option<u64> {
        handle
            .snapshot("g", vec![])
            .active
            .and_then(|route| route.tab.parse().ok())
    }

    fn windows_of(handle: &DesktopHandle, group: &str) -> Vec<String> {
        handle
            .snapshot("g", vec![])
            .entries
            .into_iter()
            .filter(|entry| entry.route.group == group)
            .map(|entry| entry.route.tab)
            .collect()
    }

    /// The group in front, and the strip it stands in.
    fn in_front_group(handle: &DesktopHandle) -> Option<(u64, u64)> {
        let route = handle.snapshot("g", vec![]).active?;
        Some((route.group.parse().ok()?, route.strip.parse().ok()?))
    }

    /// The groups of the shown strip, left to right.
    fn groups_of_shown_strip(handle: &DesktopHandle) -> Vec<u64> {
        let Some((_, strip)) = in_front_group(handle) else {
            return Vec::new();
        };
        handle.read(|tree| {
            tree.workspaces[0]
                .strips
                .iter()
                .find(|shown| shown.id.0 == strip)
                .map(|shown| shown.window_groups.iter().map(|group| group.id).collect())
                .unwrap_or_default()
        })
    }

    /// The width of the window in front, in permille of its strip.
    fn width_in_front(handle: &DesktopHandle) -> Option<u32> {
        let (group, _) = in_front_group(handle)?;
        handle.read(|tree| {
            tree.workspaces[0]
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|shown| shown.id == group)
                .map(|shown| shown.width.permille())
        })
    }

    /// A shell holding `tree`.
    fn shell(tree: Layout) -> DesktopHandle {
        DesktopHandle::new(tree, 7)
    }

    /// Every shortcut of the table is carried out by one side or handed to the other: the desktop
    /// answers the shortcuts of its own windows, and the session's are passed on whole — none falls
    /// between them.
    #[test]
    fn the_table_hands_every_shortcut_to_the_shell_or_the_session() {
        for action in ShortcutAction::ALL {
            let handle = shell(every_place());
            let performed = handle.perform_shortcut(action);
            match (action, action.home()) {
                // Reporting a problem is the shell's picker, which asks its host where to send it.
                (ShortcutAction::ReportProblem, _) => {
                    assert_eq!(
                        performed,
                        Performed::Shell(ShellPart::ReportProblem),
                        "{action:?} keeps its own chord"
                    );
                }
                (_, ShortcutHome::Shell) => {
                    assert_ne!(
                        performed,
                        Performed::Session,
                        "{action:?} is the shell's, not the session's"
                    );
                }
                (_, ShortcutHome::Session) => {
                    assert_eq!(performed, Performed::Session, "{action:?} is the session's");
                }
            }
        }
    }

    /// The chords of the table move what they name: the tab in front within its window, a tab into a
    /// window of its own, that window along its strip and into the strip below, and the window's share
    /// of the strip.
    #[test]
    fn the_chords_move_tabs_windows_and_strips() {
        let handle = shell(two_windows());
        assert_eq!(in_front(&handle), Some(20));

        handle.perform_shortcut(ShortcutAction::NextTab);
        settle(&handle);
        assert_eq!(in_front(&handle), Some(21), "the next tab of its window");
        handle.perform_shortcut(ShortcutAction::NextTab);
        settle(&handle);
        assert_eq!(
            in_front(&handle),
            Some(20),
            "and round the ring of its window's tabs"
        );
        handle.perform_shortcut(ShortcutAction::PreviousTab);
        settle(&handle);
        assert_eq!(in_front(&handle), Some(21), "back the other way");

        // A tab into a window of its own, right after the one it left: its own window holds the tab in
        // front, and the window it left keeps the other.
        handle.perform_shortcut(ShortcutAction::DetachTab);
        settle(&handle);
        assert_eq!(windows_of(&handle, "3"), ["20"]);
        let split = groups_of_shown_strip(&handle);
        assert_eq!(split.first(), Some(&3), "the window it left stands first");
        assert_eq!(
            windows_of(&handle, split[1].to_string().as_str()),
            ["21"],
            "and the tab is in the window after it: {split:?}"
        );

        // Attaching it to the window before it puts the tab back.
        handle.perform_shortcut(ShortcutAction::AttachTabLeft);
        settle(&handle);
        assert_eq!(windows_of(&handle, "3"), ["20", "21"]);

        // A window moves one place along its strip.
        let before = groups_of_shown_strip(&handle);
        handle.perform_shortcut(ShortcutAction::MoveGroupRight);
        settle(&handle);
        let after = groups_of_shown_strip(&handle);
        assert_ne!(before, after, "the window moved along its strip");
        assert_eq!(after, [4, 3], "one place right: {after:?}");
        handle.perform_shortcut(ShortcutAction::MoveGroupLeft);
        settle(&handle);
        assert_eq!(
            groups_of_shown_strip(&handle),
            before,
            "and back the other way"
        );

        // A share of the strip, narrower and wider again.
        let normal = width_in_front(&handle).expect("a window in front");
        handle.perform_shortcut(ShortcutAction::ResizeNarrower);
        settle(&handle);
        let narrower = width_in_front(&handle).expect("a window in front");
        assert!(narrower < normal, "{narrower} is narrower than {normal}");
        handle.perform_shortcut(ShortcutAction::ResizeWider);
        settle(&handle);
        assert_eq!(width_in_front(&handle), Some(normal), "and wider again");

        // Below the last strip a window that shares its strip gets a new strip of its own, and the
        // person goes with it.
        let (group, first) = in_front_group(&handle).expect("a window in front");
        handle.perform_shortcut(ShortcutAction::MoveGroupToNextStrip);
        settle(&handle);
        let strips = handle.read(|tree| tree.workspaces[0].strips.len());
        assert_eq!(strips, 2, "a strip was made below the last one");
        let (moved, below) = in_front_group(&handle).expect("the window is still in front");
        assert_eq!(moved, group, "the same window");
        assert_ne!(below, first, "in the new strip");
        assert_eq!(groups_of_shown_strip(&handle), [group], "alone there");

        // Alone in the last strip it has nowhere to go: no other strip is made.
        handle.perform_shortcut(ShortcutAction::MoveGroupToNextStrip);
        settle(&handle);
        assert_eq!(in_front_group(&handle), Some((group, below)));
        assert_eq!(
            handle.read(|tree| tree.workspaces[0].strips.len()),
            2,
            "no empty strip is left behind"
        );
    }

    /// The fixed chords find their commands: the window in front closes, another window of its app
    /// opens beside it, and what the session owns is asked for whole, so the same action answers a
    /// menu row, the palette and an agent.
    #[test]
    fn the_fixed_chords_close_duplicate_and_ask_the_session() {
        let handle = shell(two_windows());
        assert_eq!(in_front(&handle), Some(20));

        assert_eq!(
            handle.perform_fixed_shortcut(FixedAction::CloseTab),
            Performed::Done
        );
        settle(&handle);
        assert_eq!(in_front(&handle), Some(21), "the window in front closed");

        // Another window of the app in front is the shell's to open.
        assert_eq!(
            handle.perform_fixed_shortcut(FixedAction::DuplicateTab),
            Performed::Done
        );

        // A new window is the app picker: the person chooses the app.
        assert_eq!(
            handle.perform_fixed_shortcut(FixedAction::NewWindow),
            Performed::AppPicker
        );

        // The capture studio, the guide and the remote control are the session's.
        for action in [
            FixedAction::CaptureScreenshot,
            FixedAction::PictureInPicture,
            FixedAction::ShortcutGuide,
            FixedAction::StopRemoteInput,
        ] {
            assert_eq!(
                handle.perform_fixed_shortcut(action),
                Performed::Session,
                "{action:?} is the session's"
            );
        }
    }

    /// On a shell with no window, Super+T has no app to open another window of: it opens the app
    /// picker, as Super+N does.
    #[test]
    fn a_new_tab_on_an_empty_shell_opens_the_app_picker() {
        let handle = shell(layout_tree(vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: Vec::new(),
        }]));
        assert_eq!(
            handle.perform_fixed_shortcut(FixedAction::DuplicateTab),
            Performed::AppPicker,
            "the app picker is open"
        );
    }

    /// The window in front takes the strip below it, and the person goes with it.
    #[test]
    fn a_window_moves_to_the_strip_below() {
        let tree = layout_tree(vec![
            Strip {
                id: StripId(2),
                name: "Main".into(),
                window_groups: vec![group(3, vec![window(20, "Notes", "so.tilo.notes")])],
            },
            Strip {
                id: StripId(5),
                name: "Below".into(),
                window_groups: vec![group(6, vec![window(40, "Chat", "so.tilo.chat")])],
            },
        ]);
        let handle = shell(tree);

        handle.perform_shortcut(ShortcutAction::MoveGroupToNextStrip);
        settle(&handle);

        let strip = in_front_group(&handle).map(|(_, strip)| strip);
        assert_eq!(strip, Some(5), "the window is in the strip below");
        let strips = handle.read(|tree| {
            tree.workspaces[0]
                .strips
                .iter()
                .map(|strip| {
                    (
                        strip.id.0,
                        strip
                            .window_groups
                            .iter()
                            .map(|group| group.id)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(
            strips,
            [(2, vec![]), (5, vec![6, 3])],
            "the window left its strip for the one below"
        );
    }

    /// The display chords go by where the displays stand, not by the order the outputs were listed
    /// in: with the first output on the right, the arrow towards the other display moves the window
    /// there, and the arrow away from it moves nothing.
    #[test]
    fn the_display_chords_follow_where_the_displays_stand() {
        let handle = shell(two_windows());
        let display = |id: u32, monitor: &str, x: f64| tilo_desktop_model::Display {
            id,
            monitor: monitor.into(),
            x,
            y: 0.0,
            width: 640.0,
            height: 800.0,
            primary: false,
        };
        // The first output, where the windows start, stands right of the second.
        assert!(handle.set_displays(&[display(0, "right", 640.0), display(1, "left", 0.0)]));
        let (group, _) = in_front_group(&handle).unwrap();
        assert_eq!(
            handle.display_of(group),
            Some(0),
            "it starts on the right display"
        );

        handle.perform_shortcut(ShortcutAction::MoveGroupToNextDisplay);
        settle(&handle);
        assert_eq!(
            handle.display_of(group),
            Some(0),
            "nothing stands right of the right display"
        );
        handle.perform_shortcut(ShortcutAction::MoveGroupToPreviousDisplay);
        settle(&handle);
        assert_eq!(
            handle.display_of(group),
            Some(1),
            "the left arrow takes it to the display on the left"
        );
        handle.perform_shortcut(ShortcutAction::MoveGroupToNextDisplay);
        settle(&handle);
        assert_eq!(
            handle.display_of(group),
            Some(0),
            "and the right arrow back"
        );
    }

    /// A window closed from its ✕ comes back where it was when the person asks for it back: at its old
    /// index in its group, in front, and only the once.
    #[test]
    fn the_reopen_chord_brings_a_closed_window_back_where_it_was() {
        let tree = layout_tree(vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![
                group(
                    3,
                    vec![
                        window(20, "Notes", "so.tilo.notes"),
                        window(30, "Chat", "so.tilo.chat"),
                        window(40, "Files", "so.tilo.files"),
                    ],
                ),
                group(4, vec![window(50, "Mail", "so.tilo.mail")]),
            ],
        }]);
        let handle = shell(tree);
        assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);

        // The window's ✕ asks the desktop to close it, as a page's does.
        let closed = handle.snapshot("g", vec![]).entries[1].route.clone();
        assert_eq!(closed.group, "3");
        handle
            .dispatch(crate::desktop::navigation::LayoutEffect::CloseTab { tab: closed.tab })
            .unwrap();
        settle(&handle);
        assert_eq!(windows_of(&handle, "3"), ["20", "40"]);

        handle.perform_shortcut(ShortcutAction::ReopenClosedTab);
        settle(&handle);
        assert_eq!(
            windows_of(&handle, "3"),
            ["20", "30", "40"],
            "back at its old index in its group"
        );
        assert_eq!(in_front(&handle), Some(30), "and in front");

        // The close has been undone: the chord brings back nothing a second time, and the desktop still
        // answers it.
        handle.perform_shortcut(ShortcutAction::ReopenClosedTab);
        settle(&handle);
        assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);
        assert_eq!(windows_of(&handle, "4"), ["50"]);
    }
}
