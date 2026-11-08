//! The arrangement a build before this one kept beside the desk.
//! A session used to keep the person's arrangement in a document of its own
//! (`tilo/arrangement.automerge`): a workspace, a row, a strip and a place each as an entry under
//! its own id, and nothing in the desk's document was what the person saw. The desk's one document
//! holds all of it now, so the first start on this build reads what the document holds that the
//! desk does not ([`adopt`]) and the file goes. A window it kept as a live surface that did not
//! open again comes back as the app it showed, as the desk keeps every window
//! ([`Tab::relaunchable`]).

use std::collections::btree_map::Entry;
use std::path::Path;

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use tilo_desktop_model::{
    ArrangementKind, CanvasPlace, DesktopTree, Strip, StripId, Tab, TabId, TileContent,
    WindowGroup, WindowWidth, Workspace, WorkspaceSlot,
};

/// The arrangement file a build before this one kept beside the desk.
const FILE: &str = "arrangement.automerge";

/// What the kept arrangement gives the desk it is adopted into: the desk with the rows, strips and
/// places it kept, and the record each of its windows shows by tab.
pub(crate) struct Adopted {
    pub(crate) desk: DesktopTree,
    pub(crate) shows: Vec<(TabId, String)>,
}

/// The desk `desk` , with what the arrangement the old file under `state` holds that the desk does
/// not: the rows, strips and places it describes, each by its id, and the record each of those
/// windows shows, by tab. `None` when this device kept no arrangement, or kept one this build
/// cannot read: the desk this device holds is the one its person sees.
pub(crate) fn adopt(state: &Path, desk: &DesktopTree) -> Option<Adopted> {
    let path = state.join("tilo").join(FILE);
    let bytes = std::fs::read(&path).ok()?;
    let doc = match AutoCommit::load(&bytes) {
        Ok(doc) => doc,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the kept arrangement is not this build's");
            return None;
        }
    };
    let desk = merge(desk, &as_desk(&doc));
    let shows = windows_of(&doc);
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(path = %path.display(), %error, "the kept arrangement is not cleared");
    }
    Some(Adopted { desk, shows })
}

/// The record each window the old arrangement kept showed, by tab: what the desk keeps beside its
/// windows, so a place comes back showing its record.
fn windows_of(doc: &impl ReadDoc) -> Vec<(TabId, String)> {
    let mut shows: Vec<(TabId, String)> = entries(doc, "places")
        .into_iter()
        .filter_map(|(tab, place)| {
            let record = text(doc, &place, "record")?;
            (!record.trim().is_empty()).then_some((tab, record))
        })
        .collect();
    shows.sort_by_key(|(tab, _)| *tab);
    shows
}

/// `desk` with the rows, strips and places of `kept` that it does not hold: a window the
/// arrangement kept is open again, and nothing the desk holds is moved or dropped. A workspace the
/// desk no longer has stays gone: which workspaces a desk has is the desk's, and one of them may be
/// another device's to add.
fn merge(desk: &DesktopTree, kept: &DesktopTree) -> DesktopTree {
    let mut merged = desk.clone();
    for workspace in &kept.workspaces {
        let Some(held) = merged
            .workspaces
            .iter_mut()
            .find(|held| held.slot == workspace.slot)
        else {
            continue;
        };
        for row in &workspace.strips {
            if !held.strips.iter().any(|held| held.id == row.id) {
                held.strips.push(Strip::new(row.id, row.name.clone()));
            }
            let Some(strip) = held.strips.iter_mut().find(|held| held.id == row.id) else {
                continue;
            };
            for group in &row.window_groups {
                if let Some(now) = strip
                    .window_groups
                    .iter_mut()
                    .find(|held| held.id == group.id)
                {
                    for tab in &group.tabs {
                        if !now.tabs.iter().any(|held| held.id == tab.id) {
                            now.tabs.push(tab.clone());
                        }
                    }
                } else {
                    strip.window_groups.push(group.clone());
                }
                if let Some(canvas) = workspace.arrangement.place_of(group.id)
                    && let Entry::Vacant(place) = held.arrangement.places.entry(group.id)
                {
                    place.insert(canvas);
                }
            }
        }
    }
    merged
}

/// The desk the old arrangement's document describes: its workspaces, their rows, strips and
/// places, each as the entry says it.
fn as_desk(doc: &impl ReadDoc) -> DesktopTree {
    let mut tree = DesktopTree::default();
    let mut workspaces: Vec<(f64, u64, String)> = entries(doc, "workspaces")
        .into_iter()
        .map(|(id, entry)| {
            (
                number(doc, &entry, "order").unwrap_or(0.0),
                id,
                text(doc, &entry, "name").unwrap_or_default(),
            )
        })
        .collect();
    workspaces.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, slot, name) in workspaces {
        let slot = WorkspaceSlot(slot);
        let mut workspace = Workspace::new(slot, name);
        let mut rows: Vec<(f64, u64, String)> = entries(doc, "rows")
            .into_iter()
            .filter(|(_, entry)| workspace_of(doc, entry) == slot.0)
            .map(|(id, entry)| {
                // ignored-failure: an order the old build wrote in a form this build cannot read sorts first
                (
                    number(doc, &entry, "order").unwrap_or(0.0),
                    id,
                    text(doc, &entry, "name").unwrap_or_default(),
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (_, row, name) in rows {
            workspace.strips.push(Strip::new(StripId(row), name));
        }
        let mut strips: Vec<(f64, u64, ObjId)> = entries(doc, "strips")
            .into_iter()
            .filter(|(_, entry)| workspace_of(doc, entry) == slot.0)
            // ignored-failure: an order the old build wrote in a form this build cannot read sorts first
            .map(|(id, entry)| (number(doc, &entry, "order").unwrap_or(0.0), id, entry))
            .collect();
        strips.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (_, group, entry) in strips {
            // ignored-failure: a strip naming no row this build can read is left out, and the rest come in
            let row = StripId(parse(doc, &entry, "row").unwrap_or_default());
            let Some(strip) = workspace.strips.iter_mut().find(|strip| strip.id == row) else {
                continue;
            };
            let mut places: Vec<(f64, u64, ObjId)> = entries(doc, "places")
                .into_iter()
                // ignored-failure: a place naming no strip this build can read is left out, and the rest come in
                .filter(|(_, place)| parse(doc, place, "strip").unwrap_or_default() == group)
                .map(|(id, place)| (number(doc, &place, "order").unwrap_or(0.0), id, place))
                .collect();
            places.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let mut tabs = Vec::new();
            for (_, id, place) in places {
                if let Some(tab) = tab_of(doc, id, &place) {
                    tabs.push(tab);
                }
            }
            let Some(first) = tabs.first().map(|tab: &Tab| tab.id) else {
                continue;
            };
            // ignored-failure: a strip naming an active window this build cannot read shows its first
            let active = parse(doc, &entry, "active")
                .filter(|tab: &u64| tabs.iter().any(|held| held.id == *tab))
                .unwrap_or(first);
            let width = json(doc, &entry, "width")
                // ignored-failure: a width the old build wrote in a form this build cannot read is normal
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or(WindowWidth::Normal);
            strip.window_groups.push(WindowGroup {
                id: group,
                tabs,
                active_tab: active,
                width,
                maximized: truth(doc, &entry, "maximized").unwrap_or(false),
                focus_history: Vec::new(),
            });
            if let Some(canvas) = json(doc, &entry, "canvas")
                // ignored-failure: a canvas this build cannot read leaves the strip where the desk lays it
                .and_then(|json| serde_json::from_str::<CanvasPlace>(&json).ok())
            {
                workspace.arrangement.places.insert(group, canvas);
            }
        }
        if !workspace.arrangement.places.is_empty() {
            workspace.arrangement.kind = ArrangementKind::Canvas;
        }
        tree.workspaces.push(workspace);
    }
    if let Some(first) = tree.workspaces.first() {
        tree.active = first.slot;
        if let Some(row) = first.strips.first() {
            tree.active_strip.insert(first.slot, row.id);
        }
    }
    tree
}

/// The tab the place entry `place` describes, in the form the desk opens it again; `None` for a
/// place naming no app to open.
fn tab_of(doc: &impl ReadDoc, id: u64, place: &ObjId) -> Option<Tab> {
    let title = text(doc, place, "title").unwrap_or_default();
    let shows = text(doc, place, "shows").unwrap_or_default();
    let saved = json(doc, place, "state")
        // ignored-failure: a live surface this build cannot read comes back as the app it showed
        .and_then(|state| serde_json::from_str::<TileContent>(&state).ok());
    let saved = saved.unwrap_or_else(|| TileContent::LocalApp {
        app_id: shows.clone(),
        label: title.clone(),
        note: String::new(),
    });
    let tab = Tab {
        id,
        title,
        tile: saved,
        app_title: None,
        view: text(doc, place, "view").unwrap_or_default(),
        pinned: truth(doc, place, "pinned").unwrap_or(false),
    }
    .relaunchable();
    if matches!(&tab.tile, TileContent::LocalApp { app_id, .. } if app_id.trim().is_empty()) {
        return None;
    }
    Some(tab)
}

/// The workspace the entry describes.
fn workspace_of(doc: &impl ReadDoc, entry: &ObjId) -> u64 {
    // ignored-failure: an entry naming no workspace this build can read belongs to none of them
    parse(doc, entry, "workspace").unwrap_or_default()
}

/// Every entry of the map at `ROOT[key]` , by id.
fn entries(doc: &impl ReadDoc, key: &str) -> Vec<(u64, ObjId)> {
    // ignored-failure: a document holding no map under this key held no entry this build reads
    let Some((Value::Object(ObjType::Map), container)) = doc.get(&ROOT, key).ok().flatten() else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for key in doc.keys(&container) {
        let Ok(id) = key.parse::<u64>() else {
            continue;
        };
        if let Some((Value::Object(ObjType::Map), entry)) = doc.get(&container, &key).ok().flatten()
        {
            held.push((id, entry));
        }
    }
    held
}

fn text(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn json(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<String> {
    text(doc, obj, key)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "an order comes from a document's number and is exact in f64 as read"
)]
fn number(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<f64> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::F64(value) => Some(*value),
            ScalarValue::Int(value) => Some(*value as f64),
            ScalarValue::Uint(value) => Some(*value as f64),
            _ => None,
        },
        _ => None,
    }
}

/// A numeric id held as text: the whole `u64` , not a float that loses its low bits past 2^53.
fn parse(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<u64> {
    // ignored-failure: a field the old build wrote as something other than a number names no id
    text(doc, obj, key)?.parse().ok()
}

fn truth(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Boolean(value) => Some(*value),
            _ => None,
        },
        _ => None,
    }
}
