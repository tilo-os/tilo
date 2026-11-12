//! Where the windows of this desk are, as presence names them to the others: the app and record
//! each window shows, and the screen the person's windows are laid out on.

use tilo_desktop_surface::host::records::{app_of, wire_record};
use tilo_relay_protocol::live::{LiveLayout, LiveRecord};
use ui::record::RecordId;

/// Who each window of this desk is, by tab, as presence names records: the short digest of the app
/// and the record its component bound. What places a person in the window they work in, and a
/// follower's view bringing it forward.
#[must_use]
pub(crate) fn bindings(
    tree: &tilo_desktop_model::DesktopTree,
    notes: &crate::continuations::Continuations,
) -> Vec<(tilo_desktop_model::TabId, String, String)> {
    bindings_of(tree, &notes.windows())
}

/// [`bindings`] for what windows show as their apps said it: the app and record each holds, and
/// the digest presence names the record by. A window whose app named none is read from the desk's
/// own tree.
pub(crate) fn bindings_of(
    tree: &tilo_desktop_model::DesktopTree,
    windows: &[(tilo_desktop_model::TabId, Option<String>, String)],
) -> Vec<(tilo_desktop_model::TabId, String, String)> {
    windows
        .iter()
        .filter_map(|(tab, app, record)| {
            let record = live_record_as(tree, *tab, record, app.as_deref())?;
            let digest = hex::encode(record.short_digest());
            Some((*tab, record.app, digest))
        })
        .collect()
}

/// Where this person's windows are on their screen, as presence says it to the others: the windows
/// of `shown` that name a record, laid out in `area` by the rule every host places by, and which of
/// them the person works in. `None` while the surface shows no screen to place anything on.
#[must_use]
pub fn placed(
    area: Option<ui::kurbo::Rect>,
    shown: &[(tilo_desktop_model::TabId, ui::kurbo::Rect)],
    record_of: impl Fn(tilo_desktop_model::TabId) -> Option<(String, String)>,
    focused: Option<tilo_desktop_model::TabId>,
) -> Option<LiveLayout> {
    let windows: Vec<(LiveRecord, [f64; 4], bool)> = shown
        .iter()
        .filter_map(|&(tab, rect)| {
            let (app, record) = record_of(tab)?;
            Some((
                LiveRecord { app, record },
                [rect.x0, rect.y0, rect.x1, rect.y1],
                focused == Some(tab),
            ))
        })
        .collect();
    let area = area?;
    LiveLayout::place([area.x0, area.y0, area.x1, area.y1], &windows)
}

/// What the window `tab` shows, as presence names it to the others: the app it runs and its
/// record's wire spelling ([`wire_record`]), the spelling a receiver matches against its own
/// records. `None` for a window that runs no app, or shows a record this session does not know yet.
/// The app is said by whoever knows it: a window drawn by its app's own surface names no app in
/// the desk's tree, and its app is what told the session what it shows.
pub(crate) fn live_record_as(
    tree: &tilo_desktop_model::DesktopTree,
    tab: tilo_desktop_model::TabId,
    record: &str,
    app: Option<&str>,
) -> Option<LiveRecord> {
    let app = app
        .map(str::to_owned)
        .or_else(|| app_of(tree, tab).map(str::to_owned))?;
    let record =
        RecordId::parse(record).map_or_else(|| record.to_owned(), |id| wire_record(&id).to_owned());
    Some(LiveRecord { app, record })
}
