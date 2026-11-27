//! The layout's windows as a reading takes them, in the order Alt+Tab walks them: every workspace's,
//! the one in front first, with where each stands and what may be pictured of it. The layout's
//! resource publishes these readings, and the switcher's panel shows the walk over them.

use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
};
use tilo_session_core::desktop::overlays::{Overlay, Overlays, SwitcherScope};
use tilo_session_core::desktop::state::DesktopHandle;

pub use tilo_device_surfaces::windows::{
    Author, Bar, BarAction, BarNotice, BarReaction, BarReactions, BarView, BarViews, Ended, Find,
    Window, WindowDisplay, WindowFace, Windows,
};

/// What a window with no title is called.
fn untitled() -> String {
    tilo_session_core::desktop::i18n::t("window-switcher-untitled")
}

/// The displays the desktop spreads over, in reading order, each by the id a window moves to it
/// with and the number a person counts it by.
pub(crate) fn displays(tree: &Layout) -> Vec<WindowDisplay> {
    tree.outputs
        .reading_order()
        .into_iter()
        .enumerate()
        .map(|(at, id)| WindowDisplay {
            id: id.to_string(),
            number: i64::try_from(at).unwrap_or(i64::MAX).saturating_add(1),
        })
        .collect()
}

/// Where a tab stands on the shell while its window is read.
#[derive(Clone, Copy)]
struct TabPlace<'a> {
    workspace: &'a Workspace,
    strip: &'a Strip,
    /// What the workspace and the strip are called as a person reads them: the names they were
    /// given, or the layout's words for what nobody named.
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
fn window_reading(
    tab: &Tab,
    at: TabPlace<'_>,
    handle: &DesktopHandle,
    fullscreen: bool,
    display: String,
) -> Window {
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
    let icon = tilo_ui_base::icons::app_glyph(&app, None).name().to_owned();
    let preview = match &tab.tile {
        TileContent::Foreign { .. } if handle.withholds(&app) => "withheld",
        TileContent::Foreign { .. } => "live",
        _ => "none",
    };
    // The shape of the window the compositor draws, when the tab holds one here.
    let size = match &tab.tile {
        TileContent::Foreign { surface_id, .. } => handle.window_size(*surface_id),
        _ => None,
    };
    // A window that runs only on another device may not be pictured here yet: it says which
    // device it is on.
    let (elsewhere, device, device_name) = match &tab.tile {
        TileContent::HostedWindow {
            host, host_label, ..
        } => (true, host.clone(), host_label.clone()),
        _ => (false, String::new(), String::new()),
    };
    let controls_desktop = tilo_desktop_model::commands::has_control_at(
        &at.workspace.policy,
        &app,
        tilo_clock::unix_now(),
    );
    Window {
        key: tab.id.to_string(),
        title: if title.is_empty() {
            untitled()
        } else {
            title.to_owned()
        },
        // The app as people know it is the device's to name, where the layout publishes the reading.
        origin_app: String::new(),
        app_key: app,
        icon,
        strip: at.strip_name.to_owned(),
        strip_key: at.strip.id.0.to_string(),
        workspace: at.workspace_name.to_owned(),
        workspace_key: at.workspace.id().to_string(),
        group_key: at.group.id.to_string(),
        tab_index: at.tab_index,
        share: at.share,
        elsewhere,
        device,
        device_name,
        place: at.place,
        recency: 0,
        focused: at.focused,
        preview: preview.to_owned(),
        // The window's own shape, as the compositor draws it: a place picturing it keeps its
        // aspect. A window with no window here to measure says nothing.
        width: size.map_or(0, |(width, _)| width),
        height: size.map_or(0, |(_, height)| height),
        // Its app is the compositor's to judge: the window it holds there is the one the pings went
        // to.
        not_responding: match &tab.tile {
            TileContent::Foreign { surface_id, .. } => handle.is_unresponsive(*surface_id),
            _ => false,
        },
        // Its app waits for the person, as the compositor says when something of it asks: a
        // refusal, a dialog they are not looking at, a background open.
        attention: handle.asks_attention(tab.id),
        // A window of its own that has not shown here yet: its app is starting, and its place
        // shows what is coming.
        busy: !elsewhere
            && size.is_none()
            && matches!(
                &tab.tile,
                TileContent::Foreign { .. } | TileContent::LocalApp { .. }
            ),
        // What it shows has changes its app has not saved, as its bar says.
        modified: handle
            .bar_toolbar(tab.id)
            .is_some_and(|toolbar| toolbar.modified),
        // Who else is at it now, their pointer over it.
        presence: handle
            .pointer_faces(tab.id)
            .into_iter()
            .map(|(name, colour)| WindowFace { name, colour })
            .collect(),
        fullscreen,
        floating: at.workspace.layout.float_of(at.group.id).is_some(),
        renameable: matches!(
            tab.tile,
            TileContent::Foreign { .. } | TileContent::HostedWindow { .. }
        ),
        opens_tabs: matches!(
            tab.tile,
            TileContent::Foreign { .. } | TileContent::LocalApp { .. }
        ),
        controls_desktop,
        display_id: display,
        bar_location: handle.window_name(tab.id).unwrap_or_default(),
        bar_field: bar_field(handle.bar_field(tab.id)).to_owned(),
        bar_typing: handle.bar_takes_typing() && handle.bar_field(tab.id).is_some(),
        find: find_of(handle, tab.id),
        bar: handle.bar_toolbar(tab.id).as_ref().map(bar_of),
        author: author_of(handle, tab.id),
        ended: handle.ended_answer(tab.id).map(|answer| Ended {
            what: answer.what,
            why: answer.why,
            said: answer.said,
            retry: answer.retry,
            details: answer.details,
        }),
    }
}

/// Who opened the window `tab`, as the shared desktop's record says: the agent whose run opened
/// it when one did, and the member who opened it otherwise.
fn author_of(handle: &DesktopHandle, tab: u64) -> Option<Author> {
    use tilo_action_policy::MemberId;
    let person = handle.author_of(tab);
    let run = handle.opened_by(tab);
    match (person, run) {
        (None, None) => None,
        (Some(person), None) => Some(Author {
            identity: person.clone(),
            kind: "person".to_owned(),
            name: String::new(),
            colour: author_colour(MemberId::parsed_person(&person).map_or_else(
                || tilo_ui_base::tokens::PresenceHue::of_agent(&person),
                |member| tilo_ui_base::tokens::PresenceHue::of_member(&member),
            )),
            agent: String::new(),
            run: String::new(),
            title: String::new(),
        }),
        (_, Some(run)) => Some(Author {
            identity: String::from_utf8_lossy(&run.agent.actor_bytes()).into_owned(),
            kind: "agent".to_owned(),
            name: run.label.clone(),
            colour: author_colour(tilo_ui_base::tokens::PresenceHue::of_member(&run.agent)),
            agent: String::from_utf8_lossy(&run.agent.actor_bytes()).into_owned(),
            run: run.run,
            title: run.title,
        }),
    }
}

/// The presence colour `hue` is, as `#rrggbb`: the one every surface draws its member in.
fn author_colour(hue: tilo_ui_base::tokens::PresenceHue) -> String {
    let (red, green, blue) = tilo_ui_base::presence::identity_rgb(hue);
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// The parts `toolbar` declares, as the reading spells them: each action with the id its app
/// knows it by, its menu items under it, and a reaction's press as the action the toolkit names.
fn bar_of(toolbar: &ui::window_toolbar::WindowToolbar) -> Bar {
    use ui::window_toolbar::{ToolbarAction, ToolbarViews, reaction_action};
    fn action(a: &ToolbarAction) -> BarAction {
        BarAction {
            id: a.id.clone(),
            name: a.name.clone(),
            icon: a.icon.clone(),
            enabled: a.enabled,
            on: a.on,
            toggles: a.toggles,
            menu: a.menu,
            items: a.items.iter().map(action).collect(),
        }
    }
    Bar {
        leading: toolbar.leading.iter().map(action).collect(),
        actions: toolbar.actions.iter().map(action).collect(),
        back: toolbar.navigation.is_some_and(|navigation| navigation.back),
        forward: toolbar
            .navigation
            .is_some_and(|navigation| navigation.forward),
        sidebar: toolbar.sidebar.is_some(),
        sidebar_on: toolbar.sidebar.is_some_and(|sidebar| sidebar),
        views: toolbar.views.as_ref().map(|views: &ToolbarViews| BarViews {
            name: views.name.clone(),
            choices: views
                .choices
                .iter()
                .map(|view| BarView {
                    id: view.id.clone(),
                    label: view.label.clone(),
                })
                .collect(),
            chosen: views.chosen.clone(),
        }),
        notice: toolbar.notice.as_ref().map(|notice| BarNotice {
            text: notice.text.clone(),
            tone: match notice.kind {
                ui::widget::toast::ToastKind::Success => "success",
                ui::widget::toast::ToastKind::Warning => "warning",
                ui::widget::toast::ToastKind::Error => "error",
                ui::widget::toast::ToastKind::Info => "info",
            }
            .to_owned(),
            actions: notice.actions.iter().map(action).collect(),
        }),
        reactions: toolbar.reactions.as_ref().map(|reactions| BarReactions {
            pills: reactions
                .pills
                .iter()
                .map(|pill| BarReaction {
                    emoji: pill.emoji.clone(),
                    count: i64::from(pill.count),
                    mine: pill.mine,
                    names: pill.names.clone(),
                    action: reaction_action(&pill.emoji),
                })
                .collect(),
        }),
    }
}

/// What the window `tab` offers the bar's search, as the reading spells it: nothing when its app
/// searches nothing in place, else what the search acts on, what it found, and the abilities and
/// options it runs under.
fn find_of(handle: &DesktopHandle, tab: u64) -> Option<Find> {
    use ui::widget::FindMode;
    use ui::window_toolbar::SearchOffer;
    let offer = handle.bar_offer(tab)?;
    let acts_on = match offer.search {
        SearchOffer::Nothing => return None,
        SearchOffer::Document => "document",
        SearchOffer::List => "list",
        SearchOffer::Query => "query",
    };
    let count = offer.count.unwrap_or_default();
    Some(Find {
        acts_on: acts_on.to_owned(),
        // A document counts the match it stands on of all of them; a list the rows it left of
        // all it had. Both say it the same way.
        current: i64::from(count.shown),
        total: i64::from(count.total),
        replaces: offer.find.replace,
        regex: offer.find.regex,
        hex: offer.find.hex,
        match_case: offer.options.match_case,
        match_whole_word: offer.options.whole_word,
        match_regex: offer.options.mode == FindMode::Regex,
        match_hex: offer.options.mode == FindMode::Hex,
        query: offer
            .adopted
            .map(|adopted| adopted.query)
            .unwrap_or_default(),
    })
}

/// The bar field `field` as the reading spells it: the app's location, the tab's name, its search
/// (`find` or `replace`), or empty for none.
fn bar_field(field: Option<tilo_session_core::desktop::state::BarField>) -> &'static str {
    use tilo_session_core::desktop::state::BarField;
    use ui::window_toolbar::SearchOpening;
    match field {
        Some(BarField::Location) => "location",
        Some(BarField::Rename) => "rename",
        Some(BarField::Search(SearchOpening::Find)) => "find",
        Some(BarField::Search(SearchOpening::Replace)) => "replace",
        None => "",
    }
}

/// Whether `tab` counts as a window for the switcher's walk and the overview's pictures: a place a
/// saved window stood in is the tab's and not a window to walk to. A tab that says what became of
/// its app is a window while it says it: the shell draws the card where the window was.
fn tab_is_window(tab: &Tab, handle: &DesktopHandle) -> bool {
    !matches!(
        tab.tile,
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }
    ) || handle.ended_answer(tab.id).is_some()
}

/// The window currently in front in `tree`'s active workspace — `(strip, group, tab)` — when the
/// active strip's active group (or its first group) holds an active tab that counts as a window.
/// Both [`windows`] and [`switcher_walk_empty_in`] derive focus from this one rule.
fn front_window(tree: &Layout, handle: &DesktopHandle) -> Option<(StripId, u64, u64)> {
    let front = tree.active_workspace()?;
    let active_strip = tree.active_strip_for(front)?;
    let strip = front.strips.iter().find(|strip| strip.id == active_strip)?;
    let active_group = tree
        .active_group_for(strip.id)
        .filter(|id| strip.window_groups.iter().any(|group| group.id == *id))
        .or_else(|| strip.window_groups.first().map(|group| group.id))?;
    let group = strip
        .window_groups
        .iter()
        .find(|group| group.id == active_group)?;
    let tab = group
        .tabs
        .iter()
        .find(|tab| tab.id == group.active_tab && tab_is_window(tab, handle))?;
    Some((strip.id, group.id, tab.id))
}

/// The windows the layout shows: every workspace's, the one in front first, each strip top to bottom
/// and each group leading to trailing, the tab in front of a group first and the tabs behind it in
/// their order. Each window says where it stands (`place`), the workspace, strip and group it is
/// in, its tab's place in the group and the share of the strip it keeps, so a picture can be drawn
/// where the window stands.
pub(crate) fn windows(tree: &Layout, handle: &DesktopHandle) -> Vec<Window> {
    let Some(front) = tree.active_workspace() else {
        return Vec::new();
    };
    let focused_place = front_window(tree, handle);
    let focused_tab = focused_place.map(|(_, _, tab)| tab);
    // The tabs the focus history remembers, most recent first.
    let history: Vec<u64> = front
        .strips
        .iter()
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.focus_history.iter().rev().copied())
        .collect();
    let mut workspaces: Vec<&_> = tree.workspaces.iter().collect();
    workspaces.sort_by_key(|workspace| workspace.id() != front.id());
    let mut readings = Vec::new();
    for workspace in workspaces {
        let in_front = workspace.id() == front.id();
        let workspace_name = tilo_session_core::desktop::names::workspace(&workspace.name);
        let mut place = 0_i64;
        for (at_strip, strip) in workspace.strips.iter().enumerate() {
            let strip_name = tilo_session_core::desktop::names::strip_named(&strip.name, at_strip);
            for group in &strip.window_groups {
                // The width the window keeps on the shell: a strip's groups split its width between
                // them, so the pictures of a strip keep the layout's proportions.
                let share = group.width.fraction();
                let leading = group
                    .tabs
                    .iter()
                    .position(|tab| tab.id == group.active_tab)
                    .unwrap_or(0);
                let mut behind = 0_i64;
                for (index, tab) in group.tabs.iter().enumerate() {
                    if !tab_is_window(tab, handle) {
                        continue;
                    }
                    let tab_index = if index == leading {
                        0
                    } else {
                        behind = behind.saturating_add(1);
                        behind
                    };
                    let focused = in_front && Some((strip.id, group.id, tab.id)) == focused_place;
                    place = place.saturating_add(1);
                    readings.push(window_reading(
                        tab,
                        TabPlace {
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
                        tree.fullscreen_group() == Some(group.id),
                        tree.outputs
                            .display_of(group.id)
                            .map_or_else(String::new, |display| display.to_string()),
                    ));
                }
            }
        }
    }
    order_by_recency(&mut readings, focused_tab, &history);
    readings
}

/// Sort `readings` in the order Alt+Tab walks them: the focused tab first, then the focus
/// history's tabs most recent first, then the rest in layout order, numbering `recency` from 1.
fn order_by_recency(readings: &mut [Window], focused_tab: Option<u64>, history: &[u64]) {
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
}

/// The windows the switcher's walk crosses, as their places in `windows`, with the place in them
/// of the window in front: every window of `workspace`, or the ones the app the front window shows
/// has in `workspace`. The walk, the mark the reading carries and the commit all count from this
/// one list.
pub(crate) fn walked(
    windows: &[Window],
    workspace: WorkspaceId,
    scope: SwitcherScope,
) -> (Vec<usize>, Option<usize>) {
    let workspace = workspace.to_string();
    let front = windows
        .iter()
        .position(|window| window.focused && window.workspace_key == workspace);
    let application = front
        .and_then(|at| windows.get(at))
        .map(|window| window.app_key.as_str());
    // The walk crosses the workspace in front alone: a window of another workspace is read and
    // searched, but Alt+Tab never takes the person out of the workspace they are in.
    let in_workspace = |window: &Window| window.workspace_key == workspace;
    let in_scope = |window: &Window| match scope {
        SwitcherScope::Workspace => true,
        SwitcherScope::App => application.is_some_and(|app| window.app_key == app),
    };
    let walked: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, window)| in_workspace(window) && in_scope(window))
        .map(|(at, _)| at)
        .collect();
    let front = front.and_then(|front| walked.iter().position(|at| *at == front));
    (walked, front)
}

/// Move the keyboard to the window beside the shell's controls, in the order Alt+Tab walks the
/// workspace in front (the window in front first): forwards for `forward`, backwards for
/// `false`, or the next one past the window in front. This is where a direction the shell's
/// views leave over goes, so a controller's d-pad crosses the bar and dock into the windows.
/// Nothing moves when the workspace holds no window this device shows: the answer says whether a
/// window took the focus.
pub(crate) fn focus_beside(handle: &DesktopHandle, forward: bool) -> bool {
    let (windows, workspace) = handle.read(|tree| (windows(tree, handle), tree.active));
    let workspace = workspace.to_string();
    let here: Vec<&Window> = windows
        .iter()
        .filter(|window| window.workspace_key == workspace && !window.elsewhere)
        .collect();
    let focused = here.iter().position(|window| window.focused);
    let next = match (focused, forward) {
        (Some(at), true) => here.get(at.saturating_add(1)),
        (Some(at), false) => at.checked_sub(1).and_then(|at| here.get(at)),
        (None, true) => here.first(),
        (None, false) => here.last(),
    };
    next.is_some_and(|window| {
        window
            .key
            .parse::<u64>()
            .is_ok_and(|tab| handle.focus_tab(tab))
    })
}

/// The window the walk has reached, of `windows`: its place among them, where it stands to be
/// lifted and where Alt let go lands. `None` while no walk is up.
pub(crate) fn reaching(
    overlays: &Overlays,
    windows: &[Window],
    workspace: WorkspaceId,
) -> Option<usize> {
    let (walked, front) = walked(windows, workspace, overlays.scope());
    overlays
        .reached(walked.len(), front)
        .and_then(|at| walked.get(at).copied())
}

/// Bring the window the switcher's walk reached to the front, and close the switcher. The walk
/// counts from the window in front, over the windows its scope crosses, wrapping.
pub(crate) fn commit_switcher(handle: &DesktopHandle, walk: &Overlays) {
    let (windows, workspace) = handle.read(|tree| (windows(tree, handle), tree.active));
    let reached = reaching(walk, &windows, workspace).and_then(|at| windows.get(at));
    if let Some(tab) = tilo_failure::ignored!(
        "a reached window whose key is no tab focuses nothing",
        reached.and_then(|window| window.key.parse::<u64>().ok())
    ) {
        handle.focus_tab(tab);
    }
    walk.forget_walk();
    walk.show(Overlay::Rest);
}

/// Whether a walk over `(walked, front)` has a target to move to: empty never does; with a window
/// already in front there must be another beside it, while with none in front even a lone window is
/// a target to reach.
fn has_walk_target(walked: &[usize], front: Option<usize>) -> bool {
    !walked.is_empty() && (front.is_none() || walked.len() >= 2)
}

/// Whether a walk in `scope` on `handle`'s active workspace has no walkable windows: shares
/// [`tab_is_window`] and [`front_window`] with [`windows`] and reads the tree with no allocations,
/// so both `show-surface`/`open-surface` and the session's pass check the same empty-walk rule.
pub(crate) fn switcher_walk_empty_in(handle: &DesktopHandle, scope: SwitcherScope) -> bool {
    handle.read(|tree| match scope {
        SwitcherScope::Workspace => tree.active_workspace().is_none_or(|front| {
            !front
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| tab_is_window(tab, handle))
        }),
        SwitcherScope::App => front_window(tree, handle).is_none(),
    })
}

/// Whether an open walk on `handle` has no walkable windows left in its scope, so the session's pass
/// dismisses an emptied walk on the same pass even before the reveal delay elapses.
pub(crate) fn switcher_walk_empty(handle: &DesktopHandle, walk: &Overlays) -> bool {
    switcher_walk_empty_in(handle, walk.scope())
}

/// Open the switcher on the window before the one in front, or walk it `steps` windows on. With a
/// window already in front, a walk over fewer than two windows has nowhere to go and opens nothing;
/// with none in front, even a lone window is reached. An empty walk never opens the switcher, and a
/// walk that becomes empty closes it.
pub(crate) fn walk_switcher(
    handle: &DesktopHandle,
    walk: &Overlays,
    scope: SwitcherScope,
    steps: i64,
) {
    // Opening the walk is one window on, as Alt+Tab goes to the window before the one in front
    // whichever way its press names.
    let steps = if steps == 0 { 1 } else { steps };
    let (windows, workspace) = handle.read(|tree| (windows(tree, handle), tree.active));
    let (walked, front) = walked(&windows, workspace, scope);
    if walked.is_empty() {
        if walk.switcher_up() {
            walk.forget_walk();
            walk.show(Overlay::Rest);
        }
        return;
    }
    let opening = !walk.switcher_up() || walk.scope() != scope;
    if !walk.switcher_up() && !has_walk_target(&walked, front) {
        return;
    }
    // With no window in front the walk's origin is before index 0 (`-1`), so a forward opening step
    // (`+1`) reaches index 0 and a backward opening step (`-1`) shifts by +1 to reach the last
    // window (`count - 1`).
    let steps = if opening && front.is_none() && steps < 0 {
        steps.saturating_add(1)
    } else {
        steps
    };
    walk.walk(scope, steps);
}

/// Choose the window `at` in the walk's order: `0` the first of them, `-1` the last. Home,
/// End and a card under the pointer choose by place, without crossing the ones between.
pub(crate) fn select_switcher(handle: &DesktopHandle, walk: &Overlays, at: i64) {
    if !walk.switcher_up() {
        return;
    }
    let (windows, workspace) = handle.read(|tree| (windows(tree, handle), tree.active));
    let (walked, front) = walked(&windows, workspace, walk.scope());
    if walked.is_empty() {
        walk.forget_walk();
        walk.show(Overlay::Rest);
        return;
    }
    let count = i64::try_from(walked.len()).unwrap_or(1).max(1);
    let at = if at < 0 {
        count.saturating_add(at)
    } else {
        at.min(count.saturating_sub(1))
    };
    let origin = front.map_or(-1, |front| i64::try_from(front).unwrap_or(0));
    walk.select(at.saturating_sub(origin));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_window(key: &str, app_key: &str, workspace: WorkspaceId, focused: bool) -> Window {
        Window {
            key: key.to_owned(),
            app_key: app_key.to_owned(),
            workspace_key: workspace.to_string(),
            focused,
            ..Window::default()
        }
    }

    #[test]
    fn walked_returns_empty_for_empty_active_workspace() {
        let empty = WorkspaceId::from_u128(1);
        let elsewhere = WorkspaceId::from_u128(2);
        let windows = vec![
            test_window("23", "so.tilo.mail", elsewhere, false),
            test_window("24", "so.tilo.notes", elsewhere, false),
        ];

        assert_eq!(
            walked(&windows, empty, SwitcherScope::Workspace),
            (vec![], None)
        );
        assert_eq!(walked(&windows, empty, SwitcherScope::App), (vec![], None));
    }

    #[test]
    fn walked_filters_workspace_and_app_independently() {
        let work = WorkspaceId::from_u128(1);
        let elsewhere = WorkspaceId::from_u128(2);

        // No focused window in the active workspace (for example when the active strip is empty
        // while another strip in `work` holds windows): workspace scope still stays in `work`,
        // while app scope has no front app to match.
        let unfocused = vec![
            test_window("20", "so.tilo.notes", work, false),
            test_window("21", "so.tilo.mail", work, false),
            test_window("23", "so.tilo.notes", elsewhere, false),
            test_window("24", "so.tilo.mail", elsewhere, false),
        ];
        assert_eq!(
            walked(&unfocused, work, SwitcherScope::Workspace),
            (vec![0, 1], None)
        );
        assert_eq!(walked(&unfocused, work, SwitcherScope::App), (vec![], None));

        // Focused window in the active workspace: app scope keeps only that app inside `work`.
        let focused = vec![
            test_window("20", "so.tilo.notes", work, true),
            test_window("21", "so.tilo.mail", work, false),
            test_window("22", "so.tilo.notes", work, false),
            test_window("23", "so.tilo.notes", elsewhere, false),
        ];
        assert_eq!(
            walked(&focused, work, SwitcherScope::Workspace),
            (vec![0, 1, 2], Some(0))
        );
        assert_eq!(
            walked(&focused, work, SwitcherScope::App),
            (vec![0, 2], Some(0))
        );
    }
}
