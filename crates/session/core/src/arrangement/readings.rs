//! The person's arrangement as a shell reads it: the reading's types, the desk `reading_of`
//! turns into one, and the catalogue of what a place shows ([`Catalog`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_actions::ResourceSpec;
use tilo_desktop_model::{DesktopTree, GroupId, Tab, TabId, TileContent};

use super::{Names, Target};
/// A workspace the person's desk shows, as their arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementWorkspaceReading {
    pub key: String,
    pub name: String,
    /// Whether it is the workspace in front: a desk holds several and shows one, so a shell draws
    /// the rows of this one and no other.
    pub current: bool,
}

/// A row of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementRowReading {
    pub key: String,
    pub workspace: String,
    pub name: String,
    pub shell: String,
}

/// A strip of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementStripReading {
    pub key: String,
    pub workspace: String,
    pub row: String,
    /// Its width in per-mille of its row's usable width.
    pub width: i64,
    pub maximized: bool,
    pub active: String,
    pub placed: bool,
    pub canvas_x: i64,
    pub canvas_y: i64,
    pub canvas_width: i64,
    pub canvas_height: i64,
    /// Whether the strip's window floats above the workspace's windows,
    /// rather than standing among them in its row.
    #[serde(default)]
    pub floating: bool,
    /// The corner of the work area a floating window keeps its distance from:
    /// `top-left`, `top-right`, `bottom-right` or `bottom-left`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub float_corner: String,
    /// Its distance from that corner's side edge, in canvas pixels.
    #[serde(default)]
    pub float_dx: i64,
    /// Its distance from that corner's top or bottom edge, in canvas pixels.
    #[serde(default)]
    pub float_dy: i64,
    /// Its floating width and height in canvas pixels.
    #[serde(default)]
    pub float_width: i64,
    #[serde(default)]
    pub float_height: i64,
    pub shell: String,
}

/// A place of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementPlaceReading {
    pub key: String,
    pub strip: String,
    pub shows: String,
    pub shown_as: String,
    pub record: String,
    /// The title the place recorded when it was made, kept for the record's own bookkeeping:
    /// what a surface shows a window by is [`Self::name`].
    pub title: String,
    /// What every surface calls this place: its window's title and the app's own name
    /// (`Groceries — Notes`), or the app's name alone when the window has no title. Never an
    /// app id.
    pub name: String,
    pub pinned: bool,
    pub shell: String,
}

/// The person's arrangement as a shell's views read it, by key: every workspace, its rows, every
/// window group and every window, each as the desk has it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementReading {
    pub workspaces: Vec<ArrangementWorkspaceReading>,
    pub rows: Vec<ArrangementRowReading>,
    pub strips: Vec<ArrangementStripReading>,
    pub places: Vec<ArrangementPlaceReading>,
    /// The queries the canvas stands while their app answers them, by key: what the person typed,
    /// what it found, or why nothing did (`session.arrangement_query`, shell.md §8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<ArrangementQueryReading>,
}

/// A query typed on the canvas, as its place shows it: what the person looked for, in which app,
/// and what came of it. `state` is `searching` while the app answers, `one` when a single match
/// confident enough is the one the place becomes, `several` with the `matches` to pick from,
/// `none` when nothing matched, and `failed` with the `reason` the app gave.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementQueryReading {
    pub key: String,
    /// What the person typed.
    pub text: String,
    /// The app asked, by its canonical id.
    pub app: String,
    /// The kind of resource the query names, as its app publishes it (`tube.channel`).
    pub kind: String,
    /// `searching`, `one`, `several`, `none` or `failed`.
    pub state: String,
    /// Why the search failed, in the app's own words; empty while it does better.
    pub reason: String,
    /// The resources it found, in the order the app answered them.
    pub matches: Vec<ArrangementRefReading>,
}

/// A resource a query found: its address in its app, the name it goes by, and its kind — the same
/// three words a place's record is named by (`session.arrangement_ref`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementRefReading {
    pub key: String,
    pub address: String,
    pub title: String,
    pub kind: String,
}

/// The person's arrangement as a shell's views read it: what [`Arrangement::reading`] answers and
/// [`Arrangement::resources`] publishes a resource of.
pub type Reading = ArrangementReading;
/// A workspace of [`Reading`].
pub type WorkspaceReading = ArrangementWorkspaceReading;
/// A row of [`Reading`].
pub type RowReading = ArrangementRowReading;
/// A strip of [`Reading`].
pub type StripReading = ArrangementStripReading;
/// A place of [`Reading`].
pub type PlaceReading = ArrangementPlaceReading;

/// The resources the session publishes now, by id: what a place made from a resource takes its app
/// and view from. The session hands it the door's tree each time it changes, and never reads it
/// inside an invocation.
#[derive(Default)]
pub struct Catalog {
    by_id: BTreeMap<String, Target>,
}

impl Catalog {
    /// Take in `resources` (a door's whole tree), recursing into children.
    pub fn take(&mut self, resources: &[ResourceSpec]) {
        let mut by_id = BTreeMap::new();
        for resource in resources {
            collect_targets(resource, &mut by_id);
        }
        self.by_id = by_id;
    }

    /// The resource `id` as it is published, if it is.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Target> {
        self.by_id.get(id)
    }

    /// Put `target` under `id` , as the resource is published: a place made from a resource the
    /// session has just published but the door's tree does not hold yet.
    pub fn remember(&mut self, id: &str, target: Target) {
        self.by_id.insert(id.to_owned(), target);
    }

    /// Remember `resource` as its app publishes it: the app, the views it publishes, and what it
    /// is called — the same target the door's tree yields for it, so a place made from a resource
    /// the session has just published shows it through its app's own view, never as the app's
    /// window.
    pub fn remember_resource(&mut self, resource: &ResourceSpec) {
        collect_targets(resource, &mut self.by_id);
    }
}

/// Every resource of `resource` and its children, by id, with the views it publishes primary first.
fn collect_targets(resource: &ResourceSpec, out: &mut BTreeMap<String, Target>) {
    let mut views: Vec<&tilo_actions::ResourceViewSpec> = resource.views.iter().collect();
    views.sort_by_key(|view| std::cmp::Reverse(view.primary));
    if let Some(app) = resource.app() {
        out.insert(
            resource.id.as_str().to_owned(),
            Target {
                app: app.to_owned(),
                views: views.into_iter().map(|view| view.id.clone()).collect(),
                title: resource.label.clone(),
                kind: resource.kind.clone(),
            },
        );
    }
    for child in &resource.children {
        collect_targets(child, out);
    }
}

/// The shell this build's arrangement is: what every row, strip and place of a reading says it was
/// made by, so a person running a shell of their own reads what this one made and it reads theirs.
const SHELL: &str = "so.tilo.shell.strips";

/// The desk `tree` as the person's arrangement, by key: every workspace, its rows, every window
/// group and every window, each as the desk has it, with what each window shows (`records`) and
/// what the resource it shows is called (`catalog`).
pub(super) fn reading_of(
    tree: &DesktopTree,
    records: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> Reading {
    let mut reading = Reading::default();
    for workspace in &tree.workspaces {
        reading.workspaces.push(WorkspaceReading {
            key: workspace.slot.0.to_string(),
            name: workspace.name.clone(),
            // A desk holds every workspace the person is in and shows one: the shell draws the
            // rows of the one in front, so the reading says which that is.
            current: workspace.slot == tree.active,
        });
        for row in &workspace.strips {
            reading.rows.push(RowReading {
                key: row.id.0.to_string(),
                workspace: workspace.slot.0.to_string(),
                name: row.name.clone(),
                shell: SHELL.to_owned(),
            });
            for group in &row.window_groups {
                let canvas = workspace.arrangement.place_of(group.id);
                let float = workspace.arrangement.float_of(group.id);
                reading.strips.push(StripReading {
                    key: group.id.to_string(),
                    workspace: workspace.slot.0.to_string(),
                    row: row.id.0.to_string(),
                    width: i64::from(group.width.permille()),
                    maximized: group.maximized,
                    active: Some(group.active_tab)
                        .filter(|active| group.tabs.iter().any(|tab| tab.id == *active))
                        .map_or_else(String::new, |active| active.to_string()),
                    placed: canvas.is_some(),
                    canvas_x: canvas.map_or(0, |held| i64::from(held.x)),
                    canvas_y: canvas.map_or(0, |held| i64::from(held.y)),
                    canvas_width: canvas.map_or(0, |held| i64::from(held.width)),
                    canvas_height: canvas.map_or(0, |held| i64::from(held.height)),
                    floating: float.is_some(),
                    float_corner: float
                        .map_or_else(String::new, |float| float.corner.as_str().to_owned()),
                    float_dx: float.map_or(0, |float| i64::from(float.dx)),
                    float_dy: float.map_or(0, |float| i64::from(float.dy)),
                    float_width: float.map_or(0, |float| i64::from(float.width)),
                    float_height: float.map_or(0, |float| i64::from(float.height)),
                    shell: SHELL.to_owned(),
                });
                for tab in &group.tabs {
                    reading
                        .places
                        .push(place_reading(tab, group.id, records, catalog, names));
                }
            }
        }
    }
    reading
}

/// The place the window `tab` of the strip `strip` is, as a shell reads it.
pub(super) fn place_reading(
    tab: &Tab,
    strip: GroupId,
    records: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> PlaceReading {
    let record = records.get(&tab.id).cloned().unwrap_or_default();
    // A window showing a record is named by that record: its window shows it, so its title is the
    // record's, unless the person named the window itself.
    let record_title = catalog
        .get(&record)
        .map(|target| target.title.trim().to_owned())
        .unwrap_or_default();
    let title = if tab.app_title.is_none() && !record_title.is_empty() {
        record_title
    } else {
        tab.title.clone()
    };
    PlaceReading {
        key: tab.id.to_string(),
        strip: strip.to_string(),
        shows: tilo_desktop_model::apps::canonical_app_id(&tab.tile).unwrap_or_default(),
        shown_as: tab.view.clone(),
        record,
        title: tab.title.clone(),
        name: names(&shown_app(&tab.tile), &title),
        pinned: tab.pinned,
        shell: SHELL.to_owned(),
    }
}

/// The app a window belongs to, as the desk names it: the app's id, or a placeholder's own label
/// for an app this device does not have.
pub(super) fn shown_app(tile: &TileContent) -> String {
    match tile {
        TileContent::AppPlaceholder { label, .. } | TileContent::Stub { label, .. } => {
            label.clone()
        }
        held => tilo_desktop_model::apps::canonical_app_id(held).unwrap_or_default(),
    }
}
