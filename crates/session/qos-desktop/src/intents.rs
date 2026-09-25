//! Requests only. A session intent is never a receipt or proof of mutation.
use crate::{GroupId, StripId, TabId, WindowWidth, WorkspaceId};
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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopIntent {
    Local(LocalViewIntent),
    Session(SessionIntent),
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
