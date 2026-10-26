//! The window menu: what a right-click or a long press anywhere on a
//! window's bar, or the window-menu shortcut, offers for that window.
//!
//! One model for every door. [`window_menu_rows`] decides the rows from what
//! is true of the window ([`WindowMenuFacts`]); [`build_window_menu`] turns
//! them into the shared `ContextMenu`, each row carrying the shortcut that
//! does the same thing, in the standard menu style. The overlay host performs
//! a chosen row with the same operations the shortcuts and the `group.*` /
//! `tab.*` actions run, so a menu row is never a capability of its own.

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use ui::icons::Icon;
use ui::platform::Shortcut;
use ui::widget::context_menu::{ContextMenu, MenuItem};

use crate::shell::i18n::{t, t_args};
use crate::shell::state::{StripId, WindowWidth, WorkspaceId};

/// One thing the window menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMenuAction {
    /// Take the person to the window that is asking for them.
    Reveal,
    /// Cover the display with the window, or leave full screen.
    FullScreen,
    /// Fill the strip with the window, or restore it.
    Maximize,
    /// Carry the whole window into another strip of its workspace.
    MoveToStrip(StripId),
    /// Take the window's tab into another workspace (through the consent
    /// sheet when a client window crosses the sandbox boundary).
    MoveToWorkspace(WorkspaceId),
    /// Show the window on another display, as the Super+Shift chord does.
    MoveToDisplay(u32),
    /// Give the window a preset share of its strip.
    Width(WindowWidth),
    /// Give every window in the strip the same share.
    EqualizeWidths,
    /// A new tab of the same app, beside this one.
    NewTab,
    /// Name the tab in place, in its pill: only a host (non-Tilo) window's
    /// tab, whose title is the window's own rather than an app's resource.
    RenameTab,
    SplitRight,
    /// Detach the tab into a new window at the end of the strip.
    MoveToNewGroup,
    /// Close this tab, through the durable lifecycle the `✕` uses.
    CloseTab,
    /// Close the window and every tab in it, as one close a reopen undoes.
    CloseWindow,
    /// Tell an agent to do something with this window: the agent composer
    /// opens at the pointer about the window's app.
    TellAgent,
    /// Stop the tab controlling the desktop beyond its workspace: the one
    /// row of the bar's "Controls your desktop" chip menu
    /// ([`desktop_control_menu_rows`]).
    StopDesktopControl,
}

const MOVE_TO_STRIP_KEY: &str = "tabmenu:move-to-strip";
const MOVE_TO_WORKSPACE_KEY: &str = "tabmenu:move-to-workspace";
/// Moving the window to a display: the menu's list of where it can stand.
const MOVE_TO_DISPLAY_KEY: &str = "tabmenu:move-to-display";
const WIDTH_KEY: &str = "tabmenu:width";

impl WindowMenuAction {
    /// Stable semantic address of the row (`tabmenu:<slug>`): the
    /// `semantic:` target a script or the bench names, and the
    /// `perform_semantic_action` target an assistive technology activates.
    pub fn semantic_key(self) -> String {
        match self {
            Self::Reveal => "tabmenu:reveal".into(),
            Self::FullScreen => "tabmenu:full-screen".into(),
            Self::Maximize => "tabmenu:maximize".into(),
            Self::MoveToStrip(strip) => format!("{MOVE_TO_STRIP_KEY}:{}", strip.raw()),
            Self::MoveToWorkspace(workspace) => {
                format!("{MOVE_TO_WORKSPACE_KEY}:{}", workspace.raw())
            }
            Self::MoveToDisplay(display) => format!("{MOVE_TO_DISPLAY_KEY}:{display}"),
            Self::Width(WindowWidth::Narrow) => "tabmenu:width:narrow".into(),
            Self::Width(WindowWidth::Normal) => "tabmenu:width:normal".into(),
            Self::Width(WindowWidth::Wide) => "tabmenu:width:wide".into(),
            Self::Width(WindowWidth::Fraction(permille)) => {
                format!("tabmenu:width:{permille}")
            }
            Self::EqualizeWidths => "tabmenu:width:equal".into(),
            Self::NewTab => "tabmenu:new-tab".into(),
            Self::RenameTab => "tabmenu:rename".into(),
            Self::SplitRight => "tabmenu:split-right".into(),
            Self::MoveToNewGroup => "tabmenu:new-group".into(),
            Self::CloseTab => "tabmenu:close".into(),
            Self::CloseWindow => "tabmenu:close-window".into(),
            Self::TellAgent => "tabmenu:tell-agent".into(),
            Self::StopDesktopControl => "tabmenu:stop-desktop-control".into(),
        }
    }

    /// The action a semantic target names, if it is one of ours.
    pub fn from_semantic_key(key: &str) -> Option<Self> {
        if let Some(raw) = key
            .strip_prefix(MOVE_TO_STRIP_KEY)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return raw.parse().ok().map(|raw| Self::MoveToStrip(StripId(raw)));
        }
        if let Some(raw) = key
            .strip_prefix(MOVE_TO_WORKSPACE_KEY)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return raw
                .parse()
                .ok()
                .map(|raw| Self::MoveToWorkspace(WorkspaceId(raw)));
        }
        if let Some(raw) = key
            .strip_prefix(MOVE_TO_DISPLAY_KEY)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return raw.parse().ok().map(Self::MoveToDisplay);
        }
        if let Some(raw) = key
            .strip_prefix(WIDTH_KEY)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return match raw {
                "narrow" => Some(Self::Width(WindowWidth::Narrow)),
                "normal" => Some(Self::Width(WindowWidth::Normal)),
                "wide" => Some(Self::Width(WindowWidth::Wide)),
                "equal" => Some(Self::EqualizeWidths),
                permille => permille
                    .parse()
                    .ok()
                    .map(|permille| Self::Width(WindowWidth::Fraction(permille))),
            };
        }
        [
            Self::Reveal,
            Self::FullScreen,
            Self::Maximize,
            Self::NewTab,
            Self::RenameTab,
            Self::SplitRight,
            Self::MoveToNewGroup,
            Self::CloseTab,
            Self::CloseWindow,
            Self::TellAgent,
            Self::StopDesktopControl,
        ]
        .into_iter()
        .find(|action| action.semantic_key() == key)
    }
}

/// What is true of the window the menu is for: it decides which rows the
/// menu has and how they read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowMenuFacts {
    pub tab_count: usize,
    pub maximized: bool,
    pub full_screen: bool,
    pub strips: Vec<(StripId, String)>,
    /// The other workspaces the window could move to, by name.
    pub workspaces: Vec<(WorkspaceId, String)>,
    /// The other displays the window could be shown on, by name.
    pub displays: Vec<(u32, String)>,
    /// The tab shows a host window this device may name
    /// ([`crate::shell::tab_binding::may_rename_tab`]).
    pub renamable: bool,
    /// The window asked for the person and has not had them yet.
    pub attention: bool,
}

/// One row the menu offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowMenuEntry {
    pub action: WindowMenuAction,
    pub label: String,
    pub icon: Option<Icon>,
    /// The shortcut that does the same thing, shown at the row's end.
    pub shortcut: Option<Shortcut>,
}

/// The menu's rows in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowMenuRow {
    Entry(WindowMenuEntry),
    /// A row opening a nested menu of destinations. With none, it stays,
    /// disabled, so the menu reads the same for every window.
    Submenu {
        /// `tabmenu:move-to-strip` / `tabmenu:move-to-workspace`.
        key: &'static str,
        label: String,
        icon: Icon,
        entries: Vec<WindowMenuEntry>,
    },
    Separator,
    /// A line saying something about the window, never a command: what the
    /// chip's grant allows and how long it lasts.
    Caption(String),
}

impl WindowMenuRow {
    /// The row's own action, when choosing it does something directly.
    pub fn action(&self) -> Option<WindowMenuAction> {
        match self {
            Self::Entry(entry) => Some(entry.action),
            Self::Submenu { .. } | Self::Separator | Self::Caption(_) => None,
        }
    }

    /// Every action this row or its nested menu offers.
    pub fn actions(&self) -> Vec<WindowMenuAction> {
        match self {
            Self::Entry(entry) => vec![entry.action],
            Self::Submenu { entries, .. } => entries.iter().map(|entry| entry.action).collect(),
            Self::Separator | Self::Caption(_) => Vec::new(),
        }
    }

    /// The row's name: what a reader announces and a test reads.
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Entry(entry) => Some(&entry.label),
            Self::Submenu { label, .. } => Some(label),
            Self::Caption(text) => Some(text),
            Self::Separator => None,
        }
    }
}

/// The chord the desktop runs `action` with, when it has one.
fn bound(config: &ShortcutConfig, action: ShortcutAction) -> Option<Shortcut> {
    config.bindings.get(&action).map(menu_shortcut)
}

/// A fixed desktop chord by id, preferring its Super form (Super+W over
/// Alt+F4): the system's own spelling of the command.
fn fixed(id: &str) -> Option<Shortcut> {
    let chords = settings_store::keyboard_shortcuts::fixed_shortcut(id)?.chords();
    chords
        .iter()
        .find(|chord| {
            chord
                .modifiers
                .iter()
                .any(|m| m.eq_ignore_ascii_case("super"))
        })
        .or_else(|| chords.first())
        .map(menu_shortcut)
}

fn menu_shortcut(chord: &KeyChord) -> Shortcut {
    Shortcut::chord(chord.modifiers.iter().map(String::as_str), &chord.key)
}

/// A row opening a nested menu of the places `destinations` names.
fn places<'a>(
    key: &'static str,
    label: String,
    icon: Icon,
    destinations: impl Iterator<Item = (WindowMenuAction, &'a String)>,
) -> WindowMenuRow {
    WindowMenuRow::Submenu {
        key,
        label,
        icon,
        entries: destinations
            .map(|(action, name)| WindowMenuEntry {
                action,
                label: name.clone(),
                icon: None,
                shortcut: None,
            })
            .collect(),
    }
}

/// Move to strip and Move to workspace, each a nested menu of places.
fn move_rows(facts: &WindowMenuFacts) -> Vec<WindowMenuRow> {
    vec![
        places(
            MOVE_TO_STRIP_KEY,
            t("shell-window-menu-move-to-strip"),
            Icon::ArrowUpDown,
            facts
                .strips
                .iter()
                .map(|(id, name)| (WindowMenuAction::MoveToStrip(*id), name)),
        ),
        places(
            MOVE_TO_WORKSPACE_KEY,
            t("shell-window-menu-move-to-workspace"),
            Icon::Layers,
            facts
                .workspaces
                .iter()
                .map(|(id, name)| (WindowMenuAction::MoveToWorkspace(*id), name)),
        ),
        places(
            MOVE_TO_DISPLAY_KEY,
            t("shell-window-menu-move-to-display"),
            Icon::Monitor,
            facts
                .displays
                .iter()
                .map(|(id, name)| (WindowMenuAction::MoveToDisplay(*id), name)),
        ),
    ]
}

/// The shares of its strip a window can take, and Equalise all.
fn width_row() -> WindowMenuRow {
    WindowMenuRow::Submenu {
        key: WIDTH_KEY,
        label: t("shell-window-menu-width"),
        icon: Icon::Columns2,
        entries: vec![
            WindowMenuEntry {
                action: WindowMenuAction::Width(WindowWidth::Fraction(500)),
                label: t("shell-window-menu-width-half"),
                icon: None,
                shortcut: None,
            },
            WindowMenuEntry {
                action: WindowMenuAction::Width(WindowWidth::Narrow),
                label: t("shell-window-menu-width-narrow"),
                icon: None,
                shortcut: None,
            },
            WindowMenuEntry {
                action: WindowMenuAction::Width(WindowWidth::Normal),
                label: t("shell-window-menu-width-normal"),
                icon: None,
                shortcut: None,
            },
            WindowMenuEntry {
                action: WindowMenuAction::Width(WindowWidth::Wide),
                label: t("shell-window-menu-width-wide"),
                icon: None,
                shortcut: None,
            },
            WindowMenuEntry {
                action: WindowMenuAction::EqualizeWidths,
                label: t("shell-window-menu-width-equal"),
                icon: None,
                shortcut: None,
            },
        ],
    }
}

/// The window menu's rows for a window `facts` describes, with the
/// shortcuts `config` binds:
///
/// 1. Full screen and Maximize (or Exit full screen and Restore);
/// 2. Move to strip and Move to workspace, each a nested menu of places;
/// 3. New tab, Rename tab for a host window's tab, and for a tabbed window
///    Split right and Move to new group;
/// 4. Close tab, for a tabbed window, then Close window;
/// 5. Tell an agent…, as every context menu ends.
///
/// A window of one tab is offered Close window only: its tab is the window.
pub fn window_menu_rows(facts: &WindowMenuFacts, config: &ShortcutConfig) -> Vec<WindowMenuRow> {
    let entry = |action, label: String, icon, shortcut| {
        WindowMenuRow::Entry(WindowMenuEntry {
            action,
            label,
            icon: Some(icon),
            shortcut,
        })
    };
    let tabbed = facts.tab_count >= 2;
    let mut rows = Vec::new();
    if facts.attention {
        rows.push(entry(
            WindowMenuAction::Reveal,
            t("shell-window-menu-reveal"),
            Icon::CornerUpRight,
            None,
        ));
        rows.push(WindowMenuRow::Separator);
    }
    rows.extend([
        entry(
            WindowMenuAction::FullScreen,
            t(if facts.full_screen {
                "shell-window-menu-exit-full-screen"
            } else {
                "shell-window-menu-full-screen"
            }),
            Icon::Fullscreen,
            bound(config, ShortcutAction::ToggleFullscreen),
        ),
        entry(
            WindowMenuAction::Maximize,
            t(if facts.maximized {
                "shell-window-menu-restore"
            } else {
                "shell-window-menu-maximize"
            }),
            if facts.maximized {
                Icon::Minimize2
            } else {
                Icon::Maximize2
            },
            bound(config, ShortcutAction::ToggleMaximize),
        ),
        WindowMenuRow::Separator,
    ]);
    rows.extend(move_rows(facts));
    rows.push(width_row());
    rows.extend([
        WindowMenuRow::Separator,
        entry(
            WindowMenuAction::NewTab,
            t("shell-window-menu-new-tab"),
            Icon::Plus,
            fixed("fixed-duplicate-tab"),
        ),
    ]);
    if facts.renamable {
        rows.push(entry(
            WindowMenuAction::RenameTab,
            t("shell-window-menu-rename-tab"),
            Icon::Pencil,
            None,
        ));
    }
    if tabbed {
        rows.push(entry(
            WindowMenuAction::SplitRight,
            t("shell-tab-menu-split-right"),
            Icon::Columns2,
            None,
        ));
        rows.push(entry(
            WindowMenuAction::MoveToNewGroup,
            t("shell-tab-menu-new-group"),
            Icon::AppWindow,
            None,
        ));
    }
    rows.push(WindowMenuRow::Separator);
    let close = fixed("fixed-close-tab");
    if tabbed {
        rows.push(entry(
            WindowMenuAction::CloseTab,
            t("shell-tab-menu-close"),
            Icon::X,
            close,
        ));
        rows.push(entry(
            WindowMenuAction::CloseWindow,
            t("shell-window-menu-close-window"),
            Icon::X,
            None,
        ));
    } else {
        // The one tab is the window: the chord that closes the tab closes it.
        rows.push(entry(
            WindowMenuAction::CloseWindow,
            t("shell-window-menu-close-window"),
            Icon::X,
            close,
        ));
    }
    rows.push(WindowMenuRow::Separator);
    rows.push(tell_agent_row());
    rows
}

/// "Tell an agent…": the row every menu the desktop opens ends with.
pub fn tell_agent_row() -> WindowMenuRow {
    WindowMenuRow::Entry(WindowMenuEntry {
        action: WindowMenuAction::TellAgent,
        label: t("shell-window-menu-tell-agent"),
        icon: Some(Icon::Sparkles),
        shortcut: None,
    })
}

fn menu_item(entry: &WindowMenuEntry) -> MenuItem {
    let mut item = MenuItem::selection(entry.label.clone());
    if let Some(icon) = entry.icon {
        item = item.with_icon(icon);
    }
    if let Some(shortcut) = &entry.shortcut {
        item = item.with_shortcut(shortcut.clone());
    }
    item
}

/// A closed [`ContextMenu`] holding `rows`, index-aligned with them: the
/// host reads a pick back through `ContextMenu::take_selection` (a row) and
/// `take_submenu_selection` (a row and its nested row). The callbacks are
/// inert because the host has to mutate itself and the persisted tree.
pub fn build_window_menu(rows: &[WindowMenuRow]) -> ContextMenu {
    let mut menu = ContextMenu::new();
    for row in rows {
        match row {
            WindowMenuRow::Entry(entry) => menu.push(menu_item(entry)),
            WindowMenuRow::Submenu {
                label,
                icon,
                entries,
                ..
            } => menu.push(
                MenuItem::submenu(label.clone(), entries.iter().map(menu_item).collect())
                    .with_icon(*icon)
                    .with_enabled(!entries.is_empty()),
            ),
            WindowMenuRow::Separator => menu.push(MenuItem::separator()),
            WindowMenuRow::Caption(text) => menu.push(MenuItem::caption(text.clone())),
        }
    }
    menu
}

/// The menu the bar's "Controls your desktop" chip opens: `captions` say
/// what the grant allows and how long it lasts, then the one way to stop it.
pub fn desktop_control_menu_rows(captions: Vec<String>) -> Vec<WindowMenuRow> {
    let mut rows: Vec<_> = captions.into_iter().map(WindowMenuRow::Caption).collect();
    rows.push(WindowMenuRow::Separator);
    rows.push(WindowMenuRow::Entry(WindowMenuEntry {
        action: WindowMenuAction::StopDesktopControl,
        label: t("desktop-control-stop"),
        icon: Some(Icon::Ban),
        shortcut: None,
    }));
    rows
}

/// "Strip N" for an unnamed strip at index `index`, as the strip switcher
/// names it.
pub fn strip_shown_name(name: &str, index: usize) -> String {
    if name.trim().is_empty() {
        t_args(
            "shell-window-menu-unnamed-strip",
            &[("number", index.saturating_add(1).into())],
        )
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(tab_count: usize) -> WindowMenuFacts {
        WindowMenuFacts {
            tab_count,
            maximized: false,
            full_screen: false,
            strips: vec![(StripId(2), "Research".into())],
            workspaces: vec![(WorkspaceId(9), "Studio".into())],
            renamable: false,
            attention: false,
            displays: Vec::new(),
        }
    }

    fn labels(rows: &[WindowMenuRow]) -> Vec<&str> {
        rows.iter().filter_map(WindowMenuRow::label).collect()
    }

    /// The menu lists the other displays the window could stand on, by the
    /// number the display settings use for one they cannot name, and each
    /// row is addressable the way every menu row is.
    #[test]
    fn the_window_menu_lists_the_other_displays() {
        let config = ShortcutConfig::default();
        let two = WindowMenuFacts {
            displays: vec![(2, "Display 2".into()), (3, "Display 3".into())],
            ..facts(1)
        };
        let rows = window_menu_rows(&two, &config);
        let display = rows
            .iter()
            .find(|row| row.label() == Some("Move to display"))
            .expect("a Move to display submenu");
        let actions = display.actions();
        assert_eq!(
            actions,
            [
                WindowMenuAction::MoveToDisplay(2),
                WindowMenuAction::MoveToDisplay(3)
            ]
        );
        let labels: Vec<&str> = match display {
            WindowMenuRow::Submenu { entries, .. } => {
                entries.iter().map(|entry| entry.label.as_str()).collect()
            }
            _ => panic!("Move to display is a submenu"),
        };
        assert_eq!(labels, ["Display 2", "Display 3"]);
        assert_eq!(
            WindowMenuAction::from_semantic_key(&actions[1].semantic_key()),
            Some(WindowMenuAction::MoveToDisplay(3)),
            "a row's stable address resolves back to it"
        );
        // One display: the row stays, disabled, so the menu reads the same
        // for every window.
        let alone = window_menu_rows(&facts(1), &config);
        let display = alone
            .iter()
            .find(|row| row.label() == Some("Move to display"))
            .expect("the row stays");
        assert!(display.actions().is_empty());
    }

    #[test]
    fn the_window_menu_offers_width_presets_and_equalise() {
        let rows = window_menu_rows(&facts(2), &ShortcutConfig::default());
        let width = rows
            .iter()
            .find(|row| row.label() == Some("Width"))
            .expect("a Width submenu");
        let labels = width
            .actions()
            .into_iter()
            .map(|action| action.semantic_key())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "tabmenu:width:500",
                "tabmenu:width:narrow",
                "tabmenu:width:normal",
                "tabmenu:width:wide",
                "tabmenu:width:equal",
            ]
        );
        assert_eq!(
            WindowMenuAction::from_semantic_key("tabmenu:width:500"),
            Some(WindowMenuAction::Width(WindowWidth::Fraction(500)))
        );
        assert_eq!(
            WindowMenuAction::from_semantic_key("tabmenu:width:narrow"),
            Some(WindowMenuAction::Width(WindowWidth::Narrow))
        );
        assert_eq!(
            WindowMenuAction::from_semantic_key("tabmenu:width:equal"),
            Some(WindowMenuAction::EqualizeWidths)
        );
    }

    #[test]
    fn a_window_that_is_asking_offers_to_take_you_to_it_first() {
        let config = ShortcutConfig::default();
        let asking = WindowMenuFacts {
            attention: true,
            ..facts(1)
        };
        let rows = window_menu_rows(&asking, &config);
        let shown = labels(&rows);
        assert_eq!(shown.first(), Some(&"Go to this window"));
        assert_eq!(
            rows.first().and_then(WindowMenuRow::action),
            Some(WindowMenuAction::Reveal)
        );
        assert_eq!(
            WindowMenuAction::from_semantic_key("tabmenu:reveal"),
            Some(WindowMenuAction::Reveal)
        );
        assert_eq!(
            labels(&window_menu_rows(&facts(1), &config)).first(),
            Some(&"Full screen"),
            "a quiet window has no reveal row"
        );
    }

    #[test]
    fn the_window_menu_offers_every_window_action_with_its_shortcut() {
        let _us = ui::keycaps::scoped(ui::Keycaps::US);
        let config = ShortcutConfig::default();
        let rows = window_menu_rows(&facts(2), &config);
        assert_eq!(
            labels(&rows),
            [
                "Full screen",
                "Maximize",
                "Move to strip",
                "Move to workspace",
                "Move to display",
                "Width",
                "New tab",
                "Split right",
                "Move to new group",
                "Close tab",
                "Close window",
                "Tell an agent…",
            ]
        );
        let shortcut = |label: &str| {
            rows.iter().find_map(|row| match row {
                WindowMenuRow::Entry(entry) if entry.label == label => {
                    entry.shortcut.as_ref().map(|s| {
                        s.label(ui::platform::PlatformConventions::new(
                            ui::platform::Platform::Linux,
                        ))
                    })
                }
                _ => None,
            })
        };
        let expect = |action: ShortcutAction| {
            config.bindings.get(&action).map(|chord| {
                menu_shortcut(chord).label(ui::platform::PlatformConventions::new(
                    ui::platform::Platform::Linux,
                ))
            })
        };
        assert!(expect(ShortcutAction::ToggleFullscreen).is_some());
        assert_eq!(
            shortcut("Full screen"),
            expect(ShortcutAction::ToggleFullscreen)
        );
        assert_eq!(shortcut("Maximize"), expect(ShortcutAction::ToggleMaximize));
        assert_eq!(shortcut("New tab").as_deref(), Some("Super+T"));
        assert_eq!(shortcut("Close tab").as_deref(), Some("Super+W"));

        let menu = build_window_menu(&rows);
        let shown: Vec<_> = menu
            .items()
            .iter()
            .filter_map(|item| Some((item.label()?, item.shortcut().is_some())))
            .collect();
        assert!(shown.contains(&("Full screen", true)));
        assert!(shown.contains(&("Close tab", true)));
    }

    #[test]
    fn move_to_strip_and_workspace_list_the_real_places_and_round_trip() {
        let rows = window_menu_rows(&facts(1), &ShortcutConfig::default());
        let nested = |key: &str| {
            rows.iter()
                .find_map(|row| match row {
                    WindowMenuRow::Submenu {
                        key: k, entries, ..
                    } if *k == key => Some(entries.clone()),
                    _ => None,
                })
                .expect("the nested menu is offered")
        };
        let strips = nested(MOVE_TO_STRIP_KEY);
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].label, "Research");
        assert_eq!(strips[0].action, WindowMenuAction::MoveToStrip(StripId(2)));
        let workspaces = nested(MOVE_TO_WORKSPACE_KEY);
        assert_eq!(workspaces[0].label, "Studio");
        for row in &rows {
            for action in row.actions() {
                assert_eq!(
                    WindowMenuAction::from_semantic_key(&action.semantic_key()),
                    Some(action)
                );
            }
        }
        assert_eq!(WindowMenuAction::from_semantic_key("tabmenu:nope"), None);

        let alone = WindowMenuFacts {
            tab_count: 1,
            ..WindowMenuFacts::default()
        };
        let menu = build_window_menu(&window_menu_rows(&alone, &ShortcutConfig::default()));
        let strip_row = menu
            .items()
            .iter()
            .find(|item| item.label() == Some("Move to strip"))
            .expect("still offered");
        assert!(!strip_row.is_enabled());
    }

    /// A window of one tab says Close window, never "Close tab", and is
    /// offered no split; Maximize and Full screen read as their way back
    /// while the window is in that state.
    #[test]
    fn rows_read_as_what_they_will_do_for_this_window() {
        let config = ShortcutConfig::default();
        let single = labels(&window_menu_rows(&facts(1), &config))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(!single.iter().any(|label| label == "Close tab"));
        assert!(!single.iter().any(|label| label == "Split right"));
        assert_eq!(
            single[single.len() - 2..],
            ["Close window", "Tell an agent…"]
        );

        let state = WindowMenuFacts {
            maximized: true,
            full_screen: true,
            ..facts(1)
        };
        let rows = window_menu_rows(&state, &config);
        assert_eq!(labels(&rows)[..2], ["Exit full screen", "Restore"]);
    }

    /// A host window's tab offers Rename tab, beside New tab, and the row
    /// round-trips through its semantic key; a Tilo app's tab titles itself
    /// and is offered none.
    #[test]
    fn only_a_host_windows_tab_offers_rename() {
        let config = ShortcutConfig::default();
        let app = labels(&window_menu_rows(&facts(2), &config))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(!app.iter().any(|label| label == "Rename tab"));

        let host = WindowMenuFacts {
            renamable: true,
            ..facts(1)
        };
        let rows = window_menu_rows(&host, &config);
        let shown = labels(&rows);
        let new_tab = shown.iter().position(|l| *l == "New tab").expect("New tab");
        assert_eq!(shown.get(new_tab + 1), Some(&"Rename tab"));
        assert!(
            rows.iter()
                .any(|row| row.action() == Some(WindowMenuAction::RenameTab))
        );
        assert_eq!(WindowMenuAction::RenameTab.semantic_key(), "tabmenu:rename");
        assert_eq!(
            WindowMenuAction::from_semantic_key("tabmenu:rename"),
            Some(WindowMenuAction::RenameTab)
        );
    }
}
