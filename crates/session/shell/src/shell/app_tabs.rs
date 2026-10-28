//! Tabs of one app, opened and closed by the app itself.
//!
//! Applications have no tab strips: a second page, file or conversation is a
//! second instance of the app in a tab of the desktop's window group. An app
//! asks for that over its authenticated `shell-ui` connection
//! (`ui::shell_tabs`): another instance right after its own tab, its own tab
//! closed, the last closed tab back. The connection vouches for the app id;
//! the record instance the app reports only picks *which* of that app's tabs
//! it is, so an app can never place, close or read another app's tab.
//!
//! The `tab.open-beside` action is the same open for everyone else — the
//! palette, an agent, a script — anchored on the focused tab.

use crate::shell::chrome_state::ChromeState;
use crate::shell::launcher;
use crate::shell::state::{GroupId, RoomId, TabId, TileContent, WorkspaceTree};
use crate::shell::tab_binding::Placement;

/// The tab a request is relative to, and where it sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabAnchor {
    pub room: RoomId,
    pub group: GroupId,
    /// The anchor's position in its group.
    pub index: usize,
    pub tab: TabId,
}

/// Why an app's tab request was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppTabError {
    /// No installed app has this id.
    UnknownApp(String),
    /// No tab of the asking app could be found.
    NoTab,
    /// No workspace to open into.
    NoWorkspace,
    /// The target is not a URI this app declares.
    InvalidTarget,
    /// The launch or close could not happen.
    Refused(String),
}

impl std::fmt::Display for AppTabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApp(app) => write!(f, "{app} is not installed"),
            Self::NoTab => f.write_str("the app has no tab here"),
            Self::NoWorkspace => f.write_str("there is no workspace to open into"),
            Self::InvalidTarget => f.write_str("the app does not open that address"),
            Self::Refused(message) => f.write_str(message),
        }
    }
}

/// The app id a tile shows, for the tiles an app instance lives in.
fn tile_app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
            Some(app_id.as_str())
        }
        _ => None,
    }
}

/// Every tab of the tree with where it sits.
fn tabs(tree: &WorkspaceTree) -> impl Iterator<Item = (TabAnchor, &TileContent)> {
    tree.desktop.workspaces.iter().flat_map(|workspace| {
        workspace.strips.iter().flat_map(move |strip| {
            strip.window_groups.iter().flat_map(move |group| {
                group.tabs.iter().enumerate().map(move |(index, tab)| {
                    (
                        TabAnchor {
                            room: workspace.room(),
                            group: group.id,
                            index,
                            tab: tab.id,
                        },
                        &tab.tile,
                    )
                })
            })
        })
    })
}

/// Where `tab` sits, whatever it shows.
pub fn anchor_for_tab(tree: &WorkspaceTree, tab: TabId) -> Option<TabAnchor> {
    tabs(tree)
        .find(|(anchor, _)| anchor.tab == tab)
        .map(|(anchor, _)| anchor)
}

/// Where the active tab of `group` sits.
pub fn anchor_in_group(
    tree: &WorkspaceTree,
    group: crate::shell::state::GroupId,
) -> Option<TabAnchor> {
    tabs(tree)
        .map(|(anchor, _)| anchor)
        .filter(|anchor| anchor.group == group)
        .find(|anchor| {
            tree.desktop.workspaces.iter().any(|workspace| {
                workspace.strips.iter().any(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|g| g.id == group && g.active_tab == anchor.tab)
                })
            })
        })
}

fn focused_tab(tree: &WorkspaceTree) -> Option<TabId> {
    let workspace = tree.desktop.active_workspace()?;
    let strip = tree
        .desktop
        .active_strip
        .get(&workspace.id)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first())?;
    let group = tree
        .desktop
        .active_group_for(strip.id)
        .and_then(|id| strip.window_groups.iter().find(|group| group.id == id))
        .or_else(|| strip.window_groups.first())?;
    Some(group.active_tab)
}

/// The tab of `app_id` a request from that app answers to: the tab its
/// launch record `instance` is bound to, else the focused tab when it shows
/// `app_id`, else the app's first tab in the active workspace.
pub fn anchor_for_app(
    tree: &WorkspaceTree,
    app_id: &str,
    instance: Option<u64>,
) -> Option<TabAnchor> {
    let shows_app = |tile: &TileContent| {
        tile_app_id(tile).is_some_and(|tile_app| launcher::app_id_matches_package(tile_app, app_id))
    };
    if let Some(instance) = instance.filter(|instance| *instance != 0) {
        let bound = tree
            .local_app_records
            .iter()
            .filter(|(_, record)| record.record_instance == instance)
            .find_map(|(tab, _)| {
                tabs(tree).find(|(anchor, tile)| anchor.tab == *tab && shows_app(tile))
            });
        if let Some((anchor, _)) = bound {
            return Some(anchor);
        }
    }
    if let Some(focused) = focused_tab(tree)
        && let Some((anchor, _)) =
            tabs(tree).find(|(anchor, tile)| anchor.tab == focused && shows_app(tile))
    {
        return Some(anchor);
    }
    let active_room = tree
        .desktop
        .active_workspace()
        .map(crate::shell::state::Workspace::room);
    tabs(tree)
        .find(|(anchor, tile)| Some(anchor.room) == active_room && shows_app(tile))
        .map(|(anchor, _)| anchor)
}

/// Whether the tab a request from `app_id` (launched as record `instance`)
/// answers to is the focused tab. A tab chord the app left unhandled is a
/// keypress in the focused window; by the time it arrives focus may have
/// moved on, and stepping through some other group's tabs would then be a
/// surprise.
pub fn is_focused_app_tab(tree: &WorkspaceTree, app_id: &str, instance: Option<u64>) -> bool {
    anchor_for_app(tree, app_id, instance)
        .is_some_and(|anchor| focused_tab(tree) == Some(anchor.tab))
}

/// The window a dialog `app_id` asks for in `workspace` belongs to: the
/// window the app named (`named`, the surface its `parent_window` handle
/// was exported for), else the focused tab when it shows the app, else the
/// app's tab in front of its window group, else any tab of the app there.
/// Only that app's own tabs are candidates, so a request can never hang a
/// dialog on another app, whatever window it names.
pub fn asking_tab(
    tree: &WorkspaceTree,
    workspace: crate::shell::state::WorkspaceId,
    app_id: &str,
    named: Option<u64>,
) -> Option<TabId> {
    let shows_app = |tile: &TileContent| {
        tile_app_id(tile).is_some_and(|tile_app| launcher::app_id_matches_package(tile_app, app_id))
    };
    let space = tree
        .desktop
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)?;
    let focused = focused_tab(tree).filter(|_| tree.desktop.active == workspace);
    let mut fallback = None;
    let mut in_front = None;
    for group in space.strips.iter().flat_map(|strip| &strip.window_groups) {
        for tab in group.tabs.iter().filter(|tab| shows_app(&tab.tile)) {
            if let (TileContent::Foreign { surface_id, .. }, Some(named)) = (&tab.tile, named)
                && *surface_id == named
            {
                return Some(tab.id);
            }
        }
    }
    for group in space.strips.iter().flat_map(|strip| &strip.window_groups) {
        for tab in group.tabs.iter().filter(|tab| shows_app(&tab.tile)) {
            if Some(tab.id) == focused {
                return Some(tab.id);
            }
            if group.active_tab == tab.id {
                in_front = in_front.or(Some(tab.id));
            }
            fallback = fallback.or(Some(tab.id));
        }
    }
    in_front.or(fallback)
}

/// Where the new instance's window lands: right after `anchor`, in front
/// unless `background`; without an anchor, wherever a launch lands.
pub fn placement_after(st: &ChromeState, anchor: Option<TabAnchor>, background: bool) -> Placement {
    match anchor {
        Some(anchor) if background => {
            Placement::BackgroundTabAt(anchor.group, anchor.index.saturating_add(1))
        }
        Some(anchor) => Placement::TabAt(anchor.group, anchor.index.saturating_add(1)),
        None => {
            let tree = st.tree();
            crate::shell::placement::placement_for(
                &tree,
                tilo_desktop_model::commands::OpenIntent::Launch,
                st.strip_viewport(),
            )
        }
    }
}

/// Open another instance of `package_id` as a tab right after `anchor`,
/// opening `target` (a URI the app declares) when given.
///
/// # Errors
///
/// When the app is not installed, the target is not one of its URIs, there
/// is no workspace, or the launch cannot be queued.
pub fn open_beside(
    st: &mut ChromeState,
    package_id: &str,
    target: Option<&str>,
    anchor: Option<TabAnchor>,
    background: bool,
) -> Result<(), AppTabError> {
    // The compositor's loop opens tabs: the apps as last enumerated, never a
    // registry read, which walks the disk.
    let app = launcher::installed_apps_without_waiting()
        .into_iter()
        .find(|app| app.has_gui && app.package_id == package_id)
        .ok_or_else(|| AppTabError::UnknownApp(package_id.to_owned()))?;
    if let Some(target) = target {
        let resource = crate::shell::uri_open::resource_for_uri(target)
            .map_err(|_| AppTabError::InvalidTarget)?;
        if !launcher::revalidate_resource_claim(&app.package_id, &resource) {
            return Err(AppTabError::InvalidTarget);
        }
    }
    let workspace = {
        let mut tree = st.tree_mut();
        let active_room = tree
            .desktop
            .active_workspace()
            .map(crate::shell::state::Workspace::room);
        match anchor {
            Some(anchor) if Some(anchor.room) != active_room => tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == anchor.room)
                .cloned(),
            _ => tree.workspace_after_opening_app(&app.package_id),
        }
    }
    .ok_or(AppTabError::NoWorkspace)?;
    let placement = placement_after(st, anchor, background);
    hold_and_launch(st, &app.package_id, &app.display_name, placement, || {
        launcher::queue_launch_target_in_workspace(
            app.clone(),
            &workspace,
            target.map(str::to_owned),
        )
    })
    .map(drop)
}

/// The new tab is there from this frame, like a browser's: it holds the
/// window's place at `placement` and the app's window binds into it when it
/// maps. Held before `queue` starts the launch, since the window may map
/// before this returns. A launch refused before any process leaves no place.
fn hold_and_launch(
    st: &ChromeState,
    package_id: &str,
    display_name: &str,
    placement: Placement,
    queue: impl FnOnce() -> Result<(), launcher::LaunchError>,
) -> Result<TabId, AppTabError> {
    let held = crate::shell::tab_binding::hold_place_for_launch(
        &mut st.tree_mut(),
        package_id,
        display_name,
        placement,
    );
    match queue() {
        Ok(()) => Ok(held),
        Err(error) => {
            launcher::clear_pending_placement(package_id);
            crate::shell::tab_binding::remove_tab(&mut st.tree_mut(), held);
            Err(AppTabError::Refused(error.user_message()))
        }
    }
}

/// The next window `package_id` maps lands as a tab right after `anchor`:
/// a window the app starts itself, with no launch of the shell's to carry
/// the placement.
pub fn place_next_window_beside(
    st: &ChromeState,
    package_id: &str,
    anchor: Option<TabAnchor>,
    background: bool,
) {
    let placement = placement_after(st, anchor, background);
    launcher::set_pending_placement(package_id, placement);
}

/// One request an app made about its own tab, as the shell answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppTabRequest {
    OpenBeside {
        target: Option<String>,
        background: bool,
    },
    Close,
    ReopenClosed,
    PlaceNextWindowBeside {
        background: bool,
    },
}

/// Answer `request` from the app the `shell-ui` connection vouched for as
/// `app_id`, launched as record `instance`.
///
/// # Errors
///
/// Why the request was refused; nothing changed.
pub fn apply_request(
    st: &mut ChromeState,
    app_id: &str,
    instance: Option<u64>,
    request: AppTabRequest,
) -> Result<(), AppTabError> {
    let anchor = anchor_for_app(&st.tree(), app_id, instance);
    match request {
        AppTabRequest::OpenBeside { target, background } => {
            open_beside(st, app_id, target.as_deref(), anchor, background)
        }
        AppTabRequest::Close => {
            let anchor = anchor.ok_or(AppTabError::NoTab)?;
            crate::shell::window_actions::close_tab(st, anchor.tab)
                .map_err(|error| AppTabError::Refused(error.user_message()))
        }
        AppTabRequest::ReopenClosed => {
            st.reopen_closed_tab();
            Ok(())
        }
        AppTabRequest::PlaceNextWindowBeside { background } => {
            place_next_window_beside(st, app_id, anchor, background);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{Tab, WindowGroup, WindowWidth, WorkspaceTree, next_id};

    struct Fixture {
        group: GroupId,
        page: TabId,
        other: TabId,
    }

    /// One group: a page of `app`, bound to launch record `instance`, then a
    /// window of another app, which is focused.
    fn group_with_page(tree: &mut WorkspaceTree, app: &str, instance: u64) -> Fixture {
        let (group, page, other) = (next_id(), next_id(), next_id());
        let workspace = tree.desktop.active_workspace_mut().unwrap();
        let strip = workspace.strips.first_mut().unwrap();
        strip.window_groups.push(WindowGroup {
            id: group,
            tabs: vec![
                Tab {
                    id: page,
                    title: "Page".into(),
                    tile: TileContent::Foreign {
                        app_id: app.into(),
                        surface_id: 9_001,
                    },
                    app_title: None,
                },
                Tab {
                    id: other,
                    title: "Other".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.example.other".into(),
                        surface_id: 9_002,
                    },
                    app_title: None,
                },
            ],
            active_tab: other,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let strip_id = strip.id;
        tree.desktop.set_active_group(strip_id, group);
        tree.remember_local_app_record(page, instance);
        Fixture { group, page, other }
    }

    fn group_tabs(tree: &WorkspaceTree, group: GroupId) -> (Vec<TabId>, TabId) {
        let group = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|candidate| candidate.id == group)
            .unwrap();
        (
            group.tabs.iter().map(|tab| tab.id).collect(),
            group.active_tab,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each phase and its fresh fixture visible"
    )]
    fn an_app_opens_and_closes_its_own_desktop_tabs() {
        {
            const APP: &str = "so.tilo.app-tabs-anchor";
            let mut tree = WorkspaceTree::bootstrap_demo();
            let fixture = group_with_page(&mut tree, APP, 41);
            let anchor = anchor_for_app(&tree, APP, Some(41)).unwrap();
            assert_eq!(
                (anchor.tab, anchor.group, anchor.index),
                (fixture.page, fixture.group, 0)
            );
            // Without a record the focused tab is another app's: the app's
            // own first tab answers instead.
            let anchor = anchor_for_app(&tree, APP, None).unwrap();
            assert_eq!(anchor.tab, fixture.page);
            // A record bound to another app's tab is not this app's tab.
            assert!(anchor_for_app(&tree, "org.example.missing", Some(41)).is_none());
            assert_ne!(
                anchor_for_app(&tree, "org.example.other", Some(41)).map(|a| a.tab),
                Some(fixture.page),
                "an app can never anchor on another app's tab"
            );
        }

        {
            const APP: &str = "so.tilo.app-tabs-front";
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let st = chrome.borrow_mut();
            let fixture = group_with_page(&mut st.tree_mut(), APP, 42);
            let anchor = anchor_for_app(&st.tree(), APP, Some(42));
            let placement = placement_after(&st, anchor, false);
            assert_eq!(placement, Placement::TabAt(fixture.group, 1));
            place_next_window_beside(&st, APP, anchor, false);
            let mut tree = st.tree_mut();
            let opened =
                crate::shell::tab_binding::adopt_toplevel(&mut tree, 9_101, Some(APP.into()));
            let (tabs, active) = group_tabs(&tree, fixture.group);
            drop(tree);
            assert_eq!(tabs, vec![fixture.page, opened, fixture.other]);
            assert_eq!(active, opened, "a tab opened in front is the one shown");
        }

        {
            const APP: &str = "so.tilo.app-tabs-behind";
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let st = chrome.borrow_mut();
            let fixture = group_with_page(&mut st.tree_mut(), APP, 43);
            let anchor = anchor_for_app(&st.tree(), APP, Some(43));
            assert_eq!(
                placement_after(&st, anchor, true),
                Placement::BackgroundTabAt(fixture.group, 1)
            );
            place_next_window_beside(&st, APP, anchor, true);
            let mut tree = st.tree_mut();
            let opened =
                crate::shell::tab_binding::adopt_toplevel(&mut tree, 9_102, Some(APP.into()));
            let (tabs, active) = group_tabs(&tree, fixture.group);
            drop(tree);
            assert_eq!(tabs, vec![fixture.page, opened, fixture.other]);
            assert_eq!(active, fixture.other, "a background tab never takes focus");
        }

        {
            const APP: &str = "so.tilo.app-tabs-close";
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let mut st = chrome.borrow_mut();
            let fixture = group_with_page(&mut st.tree_mut(), APP, 44);
            crate::shell::foreign::store().note_adopted(9_001);
            apply_request(&mut st, APP, Some(44), AppTabRequest::Close).unwrap();
            assert_eq!(
                group_tabs(&st.tree(), fixture.group).0,
                vec![fixture.page, fixture.other],
                "the app can defer its ordinary close request"
            );
            assert!(
                crate::shell::tab_binding::close_tab_for_destroyed_surface_and_persist(
                    &mut st.tree_mut(),
                    9_001,
                )
            );
            crate::shell::foreign::store().forget(9_001);
            st.sync_external_tree_revision();
            assert_eq!(group_tabs(&st.tree(), fixture.group).0, vec![fixture.other]);
            apply_request(&mut st, APP, None, AppTabRequest::ReopenClosed).unwrap();
            let tree = st.tree();
            let (tabs, _) = group_tabs(&tree, fixture.group);
            let reopened = anchor_for_tab(&tree, fixture.page).unwrap();
            drop(tree);
            assert_eq!(tabs, vec![fixture.page, fixture.other], "back where it was");
            assert_eq!(reopened.index, 0);
        }

        {
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let mut st = chrome.borrow_mut();
            let fixture = group_with_page(&mut st.tree_mut(), "so.tilo.app-tabs-present", 45);
            assert_eq!(
                apply_request(
                    &mut st,
                    "so.tilo.app-tabs-absent",
                    Some(45),
                    AppTabRequest::Close
                ),
                Err(AppTabError::NoTab)
            );
            assert_eq!(
                group_tabs(&st.tree(), fixture.group).0,
                vec![fixture.page, fixture.other]
            );
        }

        // Phase `an_app_hosted_in_the_shell_answers_the_same_chords`: the
        // Ctrl tab chords reach the app first and step, close or reopen only
        // because it left them unhandled.
        {
            use winit::event::Modifiers;
            use winit::keyboard::{Key, ModifiersState};
            let mut frame = crate::shell::test_support::PopulatedShellFixture::new(
                (1280, 800),
                ui::theme::Theme::dark(),
            );
            let mut document = automerge::AutoCommit::new();
            let mut focus = None;
            let group_of = |frame: &crate::shell::test_support::PopulatedShellFixture| {
                let chrome = frame.chrome();
                let tab = crate::shell::window_actions::active_tab_id(&chrome).unwrap();
                let tree = chrome.tree();
                let anchor = anchor_for_tab(&tree, tab).unwrap();
                let tabs = group_tabs(&tree, anchor.group).0;
                drop(tree);
                (anchor.group, tabs)
            };
            let (group, before) = group_of(&frame);
            let mut chord = |frame: &mut crate::shell::test_support::PopulatedShellFixture,
                             key: &str,
                             modifiers: ModifiersState| {
                let key = if key == "Tab" {
                    Key::Named(winit::keyboard::NamedKey::Tab)
                } else {
                    Key::Character(key.into())
                };
                frame.dispatch_event(
                    &ui::synthetic_input::key_input(&key, winit::event::ElementState::Pressed),
                    &Modifiers::from(modifiers),
                    &mut ui::widget::Edits::lend(&mut document),
                    &automerge::ROOT,
                    &mut focus,
                )
            };
            let active = |frame: &crate::shell::test_support::PopulatedShellFixture| {
                crate::shell::window_actions::active_tab_id(&frame.chrome()).unwrap()
            };
            let start = active(&frame);
            assert!(
                chord(&mut frame, "Tab", ModifiersState::CONTROL),
                "Ctrl+Tab"
            );
            let stepped = active(&frame);
            assert_ne!(stepped, start, "Ctrl+Tab the app left shows the next tab");
            assert!(before.contains(&stepped), "within the same group");
            assert!(
                chord(
                    &mut frame,
                    "Tab",
                    ModifiersState::CONTROL | ModifiersState::SHIFT
                ),
                "Ctrl+Shift+Tab"
            );
            assert_eq!(active(&frame), start, "and Ctrl+Shift+Tab goes back");
            assert!(chord(&mut frame, "w", ModifiersState::CONTROL), "Ctrl+W");
            let closed = group_tabs(&frame.chrome().tree(), group).0;
            assert_eq!(
                closed.len(),
                before.len() - 1,
                "Ctrl+W closed the hosted tab"
            );
            assert!(
                chord(
                    &mut frame,
                    "T",
                    ModifiersState::CONTROL | ModifiersState::SHIFT
                ),
                "Ctrl+Shift+T"
            );
            assert_eq!(
                group_tabs(&frame.chrome().tree(), group).0,
                before,
                "reopened in place"
            );
            assert!(chord(&mut frame, "t", ModifiersState::CONTROL), "Ctrl+T");
            let (after_group, after) = group_of(&frame);
            assert_eq!(after_group, group, "the new instance joins the same group");
            assert_eq!(after.len(), before.len() + 1);
            let _ = chord(&mut frame, "t", ModifiersState::empty());
            assert_eq!(
                group_of(&frame).1,
                after,
                "a plain t is typing, not a tab chord"
            );
            drop(frame);
        }

        {
            const APP: &str = "so.tilo.app-tabs-instant";
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let st = chrome.borrow_mut();
            let fixture = group_with_page(&mut st.tree_mut(), APP, 46);
            let anchor = anchor_for_app(&st.tree(), APP, Some(46));
            let placement = placement_after(&st, anchor, false);
            let mut seen_at_launch = None;
            let held = hold_and_launch(&st, APP, "Instant", placement, || {
                seen_at_launch = Some(group_tabs(&st.tree(), fixture.group));
                Ok(())
            })
            .unwrap();
            assert_eq!(
                seen_at_launch,
                Some((vec![fixture.page, held, fixture.other], held)),
                "the opening card sits right after its opener, in front, before any process"
            );
            // The card was laid out: the window that takes its place is
            // configured to its size at once, not after the next layout.
            crate::shell::foreign::store()
                .note_opening_tab(held, vello::kurbo::Rect::new(10.0, 20.0, 650.0, 500.0));
            let opened = crate::shell::tab_binding::adopt_toplevel(
                &mut st.tree_mut(),
                9_146,
                Some(APP.into()),
            );
            assert_eq!(opened, held, "the window takes the card's place");
            assert_eq!(
                crate::shell::foreign::store().take_pending_configure(9_146),
                Some((640, 480)),
                "configured to the card's size in the dispatch that adopted it"
            );
            assert_eq!(
                group_tabs(&st.tree(), fixture.group).0,
                vec![fixture.page, held, fixture.other]
            );

            // A launch refused before any process leaves no card behind.
            let placement = placement_after(&st, anchor, false);
            let refused = hold_and_launch(&st, APP, "Instant", placement, || {
                Err(launcher::LaunchError::NotInstalled {
                    package_id: APP.into(),
                })
            });
            assert!(matches!(refused, Err(AppTabError::Refused(_))));
            assert_eq!(
                group_tabs(&st.tree(), fixture.group).0,
                vec![fixture.page, held, fixture.other]
            );
        }

        {
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let mut st = chrome.borrow_mut();
            assert_eq!(
                open_beside(&mut st, "so.tilo.app-tabs-not-installed", None, None, false),
                Err(AppTabError::UnknownApp(
                    "so.tilo.app-tabs-not-installed".into()
                ))
            );
        }
    }
}
