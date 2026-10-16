//! Requests only. A session intent is never a receipt or proof of mutation.
use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId, WindowWidth, WorkspaceId};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalViewIntent {
    Workspace(WorkspaceId),
    Strip(StripId),
    Group { strip: StripId, group: GroupId },
    Tab { group: GroupId, tab: TabId },
    Width { group: GroupId, width: WindowWidth },
    Maximized { group: GroupId, maximized: bool },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionIntent {
    CloseTab {
        group: GroupId,
        tab: TabId,
    },
    ReorderTab {
        group: GroupId,
        from: usize,
        to: usize,
    },
    ReorderGroup {
        strip: StripId,
        group: GroupId,
        to: usize,
    },
    OpenTab {
        strip: StripId,
        group: GroupId,
    },
    /// Move `group` whole into strip `to`, before group `before` or at the
    /// end of that strip.
    MoveGroup {
        group: GroupId,
        to: StripId,
        before: Option<GroupId>,
    },
}
impl SessionIntent {
    /// The group the request was made in.
    pub fn group(&self) -> GroupId {
        match self {
            Self::CloseTab { group, .. }
            | Self::ReorderTab { group, .. }
            | Self::ReorderGroup { group, .. }
            | Self::OpenTab { group, .. }
            | Self::MoveGroup { group, .. } => *group,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopIntent {
    Local(LocalViewIntent),
    Session(SessionIntent),
}

/// Apply a per-device view change to the rendered desktop: selection,
/// width and maximize. It never changes the topology, so every host runs it
/// locally without asking the owner.
pub fn apply_local_view(tree: &mut DesktopTree, intent: LocalViewIntent) {
    match intent {
        LocalViewIntent::Workspace(id) => {
            if tree.workspaces.iter().any(|w| w.id == id) {
                tree.active = id;
            }
        }
        LocalViewIntent::Strip(id) => {
            if let Some(w) = tree
                .workspaces
                .iter()
                .find(|w| w.id == tree.active && w.strips.iter().any(|s| s.id == id))
            {
                tree.active_strip.insert(w.id, id);
            }
        }
        LocalViewIntent::Group { strip, group } => {
            if tree
                .workspaces
                .iter()
                .filter(|w| w.id == tree.active)
                .flat_map(|w| &w.strips)
                .any(|s| s.id == strip && s.window_groups.iter().any(|g| g.id == group))
            {
                tree.active_strip.insert(tree.active, strip);
                tree.active_group.insert(strip, group);
            }
        }
        other => {
            for w in &mut tree.workspaces {
                for s in &mut w.strips {
                    for g in &mut s.window_groups {
                        match other {
                            LocalViewIntent::Tab { group, tab }
                                if group == g.id && g.tabs.iter().any(|t| t.id == tab) =>
                            {
                                g.activate_tab(tab);
                                if w.id == tree.active {
                                    tree.active_strip.insert(w.id, s.id);
                                    tree.active_group.insert(s.id, g.id);
                                }
                            }
                            LocalViewIntent::Width { group, width } if group == g.id => {
                                g.width = width;
                            }
                            LocalViewIntent::Maximized { group, maximized } if group == g.id => {
                                g.maximized = maximized;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// Drain each toolkit request once; native and portable hosts apply the same typed requests.
pub fn take_maximize(group: &mut ui::widget::WindowGroup) -> Option<LocalViewIntent> {
    group
        .take_pending_maximize_change()
        .map(|maximized| LocalViewIntent::Maximized {
            group: group.id,
            maximized,
        })
}
pub fn take_width(group: &mut ui::widget::WindowGroup) -> Option<LocalViewIntent> {
    group
        .take_pending_width_change()
        .map(|width| LocalViewIntent::Width {
            group: group.id,
            width: WindowWidth::Fraction(width),
        })
}
pub fn take_tab_reorder(group: &mut ui::widget::WindowGroup) -> Option<SessionIntent> {
    group
        .take_pending_tab_reorder()
        .map(|(from, to)| SessionIntent::ReorderTab {
            group: group.id,
            from,
            to,
        })
}
pub fn take_group_reorder(strip: StripId, row: &mut ui::widget::GroupRow) -> Option<SessionIntent> {
    row.take_pending_group_reorder()
        .map(|(group, to)| SessionIntent::ReorderGroup { strip, group, to })
}
pub fn take_closes(group: &mut ui::widget::WindowGroup) -> Vec<SessionIntent> {
    group
        .take_pending_close_commits()
        .into_iter()
        .map(|tab| SessionIntent::CloseTab {
            group: group.id,
            tab,
        })
        .collect()
}
pub fn take_open(strip: StripId, group: &mut ui::widget::WindowGroup) -> Option<SessionIntent> {
    group
        .take_pending_new_tab()
        .map(|group| SessionIntent::OpenTab { strip, group })
}
