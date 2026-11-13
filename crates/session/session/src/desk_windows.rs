//! The desk's windows as a reading takes them, in the order Alt+Tab walks them: every workspace's,
//! the one in front first, with where each stands and what may be pictured of it. The desk's
//! resource publishes these readings, and the switcher's panel shows the walk over them.

use tilo_desktop_model::{DesktopTree, Strip, Tab, WindowGroup, Workspace};
use tilo_desktop_surface::DesktopHandle;
use tilo_session_core::shell;

/// What a window with no title is called.
fn untitled() -> String {
    tilo_desktop_surface::i18n::t("window-switcher-untitled")
}

/// Where a tab stands on the desk while its window is read.
#[derive(Clone, Copy)]
struct Standing<'a> {
    workspace: &'a Workspace,
    strip: &'a Strip,
    /// What the workspace and the strip are called as a person reads them: the names they were
    /// given, or the desk's words for what nobody named.
    workspace_name: &'a str,
    strip_name: &'a str,
    group: &'a WindowGroup,
    /// The share of its strip's width the window keeps.
    share: f64,
    /// Where it stands in the picture of its workspace.
    place: i64,
    /// Its place among its group's tabs: `0` for the tab in front.
    tab_index: i64,
    focused: bool,
}

/// One window's reading: what it shows, where it stands and what may be pictured of it.
/// `handle` says what may be pictured: a window whose app holds secrets shows its app's glyph
/// instead of its picture (`withheld`), and one that is not this device's window shows neither
/// (`none`).
fn window_reading(tab: &Tab, at: Standing<'_>, handle: &DesktopHandle) -> shell::WindowReading {
    use tilo_desktop_model::TileContent;
    let app = match &tab.tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => app_id.clone(),
        TileContent::AppPlaceholder { label, .. } | TileContent::Stub { label, .. } => {
            label.clone()
        }
    };
    let title = tab.title.trim();
    // What the window's well shows instead of a picture: its app's own glyph, so two apps' windows
    // never look alike.
    let icon = ui::icons::app_glyph(&app, None).name().to_owned();
    let preview = match &tab.tile {
        TileContent::Foreign { .. } if handle.withholds(&app) => "withheld",
        TileContent::Foreign { .. } => "live",
        _ => "none",
    };
    // A window that runs only on another device may not be pictured here yet: it says which
    // device it is on.
    let (local, device, device_name) = match &tab.tile {
        TileContent::HostedWindow {
            host, host_label, ..
        } => (false, host.clone(), host_label.clone()),
        _ => (true, String::new(), String::new()),
    };
    shell::WindowReading {
        key: tab.id.to_string(),
        title: if title.is_empty() {
            untitled()
        } else {
            title.to_owned()
        },
        // The app as people know it is the device's to name, where the desk publishes the reading.
        origin_app: String::new(),
        app_key: app,
        icon,
        strip: at.strip_name.to_owned(),
        strip_key: at.strip.id.0.to_string(),
        workspace: at.workspace_name.to_owned(),
        workspace_key: at.workspace.slot.0.to_string(),
        group_key: at.group.id.to_string(),
        tab_index: at.tab_index,
        share: at.share,
        local,
        device,
        device_name,
        place: at.place,
        recency: 0,
        focused: at.focused,
        preview: preview.to_owned(),
        // Its app is the compositor's to judge: the window it holds there is the one the pings went
        // to.
        not_responding: match &tab.tile {
            TileContent::Foreign { surface_id, .. } => handle.is_unresponsive(*surface_id),
            _ => false,
        },
    }
}

/// The windows the desk shows: every workspace's, the one in front first, each strip top to bottom
/// and each group leading to trailing, the tab in front of a group first and the tabs behind it in
/// their order. Each window says where it stands (`place`), the workspace, strip and group it is
/// in, its tab's place in the group and the share of the strip it keeps, so a picture can be drawn
/// where the window stands.
pub(crate) fn windows(tree: &DesktopTree, handle: &DesktopHandle) -> Vec<shell::WindowReading> {
    let Some(front) = tree.active_workspace() else {
        return Vec::new();
    };
    let active_strip = tree
        .active_strip
        .get(&front.slot)
        .copied()
        .or_else(|| front.strips.first().map(|strip| strip.id));
    // The tabs the focus history remembers, most recent first.
    let history: Vec<u64> = front
        .strips
        .iter()
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.focus_history.iter().rev().copied())
        .collect();
    let mut workspaces: Vec<&_> = tree.workspaces.iter().collect();
    workspaces.sort_by_key(|workspace| workspace.slot != front.slot);
    let mut readings = Vec::new();
    let mut focused_tab = None;
    for workspace in workspaces {
        let in_front = workspace.slot == front.slot;
        let workspace_name = tilo_desktop_surface::names::workspace(&workspace.name);
        let mut place = 0_i64;
        for (at_strip, strip) in workspace.strips.iter().enumerate() {
            let strip_name = tilo_desktop_surface::names::strip_named(&strip.name, at_strip);
            let active_group = if in_front {
                tree.active_group_for(strip.id)
                    .filter(|id| strip.window_groups.iter().any(|group| group.id == *id))
                    .or_else(|| strip.window_groups.first().map(|group| group.id))
            } else {
                None
            };
            for group in &strip.window_groups {
                // The width the window keeps on the desk: a strip's groups split its width between
                // them, so the pictures of a strip keep the desk's proportions.
                let share = group.width.fraction();
                let leading = group
                    .tabs
                    .iter()
                    .position(|tab| tab.id == group.active_tab)
                    .unwrap_or(0);
                let mut behind = 0_i64;
                for (index, tab) in group.tabs.iter().enumerate() {
                    let tab_index = if index == leading {
                        0
                    } else {
                        behind = behind.saturating_add(1);
                        behind
                    };
                    let focused = in_front
                        && Some(strip.id) == active_strip
                        && Some(group.id) == active_group
                        && tab.id == group.active_tab;
                    if focused {
                        focused_tab = Some(tab.id);
                    }
                    place = place.saturating_add(1);
                    readings.push(window_reading(
                        tab,
                        Standing {
                            workspace,
                            strip,
                            workspace_name: &workspace_name,
                            strip_name: &strip_name,
                            group,
                            share,
                            place,
                            tab_index,
                            focused,
                        },
                        handle,
                    ));
                }
            }
        }
    }
    let rank = |key: &str| -> i64 {
        let tab: u64 = tilo_failure::ignored!(
            "a key that is no tab number sorts after every tab",
            key.parse().unwrap_or(u64::MAX)
        );
        if Some(tab) == focused_tab {
            return 0;
        }
        history
            .iter()
            .position(|at| *at == tab)
            .map_or(i64::MAX / 2, |at| {
                i64::try_from(at).unwrap_or(0).saturating_add(1)
            })
    };
    for (at, reading) in readings.iter_mut().enumerate() {
        reading.recency = rank(&reading.key)
            .saturating_mul(1024)
            .saturating_add(i64::try_from(at).unwrap_or(0));
    }
    readings.sort_by_key(|reading| reading.recency);
    // What is left of the rank is the order the walk takes: where each window stands in it, so a
    // view shows the page the walk has reached without knowing how the order was made.
    for (at, reading) in readings.iter_mut().enumerate() {
        reading.recency = i64::try_from(at).unwrap_or(i64::MAX).saturating_add(1);
    }
    readings
}

/// The windows the switcher's walk crosses, as their places in `windows`, with the place in them
/// of the window in front: every window of the workspace in front, or the ones the app the front
/// window shows has. The walk, the mark the reading carries and the commit all count from this one
/// list.
pub(crate) fn walked(
    windows: &[shell::WindowReading],
    scope: crate::overlays::SwitcherScope,
) -> (Vec<usize>, Option<usize>) {
    use crate::overlays::SwitcherScope;
    let front = windows.iter().position(|window| window.focused);
    let here = front.and_then(|front| windows.get(front));
    let workspace = here.map(|window| window.workspace_key.clone());
    let application = here.map(|window| window.app_key.clone());
    // The walk crosses the workspace in front alone: a window of another workspace is read and
    // searched, but Alt+Tab never takes the person out of the workspace they are in.
    let here = |window: &shell::WindowReading| {
        workspace
            .as_ref()
            .is_none_or(|workspace| &window.workspace_key == workspace)
    };
    let walked: Vec<usize> = match (scope, application) {
        (SwitcherScope::App, Some(application)) => windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.app_key == application && here(window))
            .map(|(at, _)| at)
            .collect(),
        _ => windows
            .iter()
            .enumerate()
            .filter(|(_, window)| here(window))
            .map(|(at, _)| at)
            .collect(),
    };
    let front = front.and_then(|front| walked.iter().position(|at| *at == front));
    (walked, front)
}

/// The window the walk has reached, of `windows`: its place among them, where it stands to be
/// lifted and where Alt let go lands. `None` while no walk is up.
pub(crate) fn reaching(
    overlays: &crate::overlays::Overlays,
    windows: &[shell::WindowReading],
) -> Option<usize> {
    let (walked, front) = walked(windows, overlays.scope());
    overlays
        .reached(walked.len(), front)
        .and_then(|at| walked.get(at).copied())
}

/// Each window's client, by the tab it is: what the compositor pictures where a card shows it.
pub(crate) fn window_surfaces(tree: &DesktopTree) -> std::collections::HashMap<u64, u64> {
    use tilo_desktop_model::TileContent;
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign { surface_id, .. } => Some((tab.id, *surface_id)),
            _ => None,
        })
        .collect()
}

/// What the switcher's panel shows while the walk is up: the windows it crosses, in its order,
/// and the one it has reached. `None` when no walk is up.
pub(crate) fn switcher_view(
    handle: &DesktopHandle,
    overlays: &crate::overlays::Overlays,
    sizes: &std::collections::HashMap<u64, (u32, u32)>,
) -> Option<tilo_desktop::window_switcher::SwitcherView> {
    let (windows, reached, surfaces) = handle.read(|tree| {
        let windows = windows(tree, handle);
        let reached = reaching(overlays, &windows);
        (windows, reached, window_surfaces(tree))
    });
    let reached = reached?;
    let (walked, _) = walked(&windows, overlays.scope());
    let selected = walked.iter().position(|at| *at == reached)?;
    let cards = walked
        .iter()
        .filter_map(|at| windows.get(*at))
        .map(|window| {
            // A window whose app holds secrets from the desk, or one this device does not run,
            // has no picture: its card shows the app's own face. A key that is no tab is such
            // a window, and so is one whose host pictures nothing.
            let client = tilo_failure::ignored!(
                "a window that is no tab has no client to picture",
                window
                    .key
                    .parse::<u64>()
                    .ok()
                    .and_then(|tab| surfaces.get(&tab).copied())
            );
            let window_id = (window.preview == "live").then_some(client).flatten();
            tilo_desktop::window_switcher::SwitcherCard {
                window: window_id,
                // The shape of the window's own pixels, as the compositor lays it out: the card
                // takes it, so the picture fills the card.
                size: client.and_then(|client| sizes.get(&client).copied()),
                title: window.title.clone(),
                app_key: window.app_key.clone(),
            }
        })
        .collect();
    Some(tilo_desktop::window_switcher::SwitcherView {
        cards,
        selected,
        app_scope: overlays.scope() == crate::overlays::SwitcherScope::App,
    })
}

/// Bring the window the switcher's walk reached to the front, and close the switcher. The walk
/// counts from the window in front, over the windows its scope crosses, wrapping.
pub(crate) fn commit_switcher(handle: &DesktopHandle, walk: &crate::overlays::Overlays) {
    let windows = handle.read(|tree| windows(tree, handle));
    let reached = reaching(walk, &windows).and_then(|at| windows.get(at));
    if let Some(tab) = tilo_failure::ignored!(
        "a reached window whose key is no tab focuses nothing",
        reached.and_then(|window| window.key.parse::<u64>().ok())
    ) {
        handle.focus_tab(tab);
    }
    walk.forget_walk();
    walk.show(crate::overlays::Overlay::Rest);
}

/// Open the switcher on the window before the one in front, or walk it `steps` windows on. A
/// walk over fewer than two windows has nowhere to go, and opens nothing: Alt+Tab with one
/// window open does what it always has, which is nothing.
pub(crate) fn walk_switcher(
    handle: &DesktopHandle,
    walk: &crate::overlays::Overlays,
    scope: Option<&str>,
    steps: i64,
) {
    let scope = scope
        .and_then(crate::overlays::SwitcherScope::named)
        .unwrap_or_default();
    // Opening the walk is one window on, as Alt+Tab goes to the window before the one in front
    // whichever way its press names.
    let steps = if steps == 0 { 1 } else { steps };
    if !walk.is_up() {
        let windows = handle.read(|tree| windows(tree, handle));
        let (walked, _) = walked(&windows, scope);
        if walked.len() < 2 {
            return;
        }
    }
    walk.walk(Some(scope.name()), steps);
}

/// Choose the window `at` in the walk's order: `0` the first of them, `-1` the last. Home,
/// End and a card under the pointer choose by place, without crossing the ones between.
pub(crate) fn select_switcher(handle: &DesktopHandle, walk: &crate::overlays::Overlays, at: i64) {
    if !walk.is_up() {
        return;
    }
    let windows = handle.read(|tree| windows(tree, handle));
    let (walked, front) = walked(&windows, walk.scope());
    let count = i64::try_from(walked.len()).unwrap_or(1).max(1);
    let at = if at < 0 {
        count.saturating_add(at)
    } else {
        at.min(count.saturating_sub(1))
    };
    let front = i64::try_from(front.unwrap_or(0)).unwrap_or(0);
    walk.select(at.saturating_sub(front));
}
