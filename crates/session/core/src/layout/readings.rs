//! The person's layout as a shell reads it: the reading's types, the layout `reading_of`
//! turns into one, and the catalogue of what a place shows ([`Catalog`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_actions::ResourceSpec;
use tilo_desktop_model::{GroupId, Layout as Tree, Tab, TabId, TileContent};

use super::{Names, Target};
/// A workspace the person's layout shows, as their layout names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutWorkspaceReading {
    pub key: String,
    pub name: String,
    /// Whether it is the workspace in front: a layout holds several and shows one, so a shell draws
    /// the rows of this one and no other.
    pub current: bool,
}

/// A row of the layout, as the person's layout names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutRowReading {
    pub key: String,
    pub workspace: String,
    pub name: String,
    pub shell: String,
    /// Whether it is the row in front in its workspace.
    pub current: bool,
}

/// A strip of the layout, as the person's layout names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a reading of the record's own fields, each a fact of the strip a view reads by name"
)]
pub struct LayoutStripReading {
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
    /// Whether `canvas_height` is the most it takes: what it shows hugs what it holds up to it.
    #[serde(default)]
    pub canvas_hugs: bool,
    /// Whether the strip's window floats above the workspace's windows,
    /// rather than among them in its row.
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

/// A place of the layout, as the person's layout names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPlaceReading {
    pub key: String,
    pub strip: String,
    pub shows: String,
    pub shown_as: String,
    pub resource: String,
    /// The title the place recorded when it was made: what the canvas draws it by, where it is
    /// drawn as its own content.
    pub title: String,
    /// What the shell's bars and menus call this place: its window's title and the app's own name
    /// (`Groceries — Notes`), or the app's name alone when the window has no title. Never an
    /// app id.
    pub name: String,
    pub pinned: bool,
    pub shell: String,
}

/// The person's layout as a shell's views read it, by key: every workspace, its rows, every
/// window group and every window, each as the layout has it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutReading {
    /// The person at this device, by their name.
    pub person: String,
    /// The strip the keyboard is in, by its key: the window this device has in front. Empty for
    /// none. It is this device's own, read off its shell, never a change of the layout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub front: String,
    /// The others here in the workspace in front.
    pub people: Vec<LayoutPersonReading>,
    pub workspaces: Vec<LayoutWorkspaceReading>,
    pub rows: Vec<LayoutRowReading>,
    pub strips: Vec<LayoutStripReading>,
    pub places: Vec<LayoutPlaceReading>,
    /// The queries the canvas stands while their app answers them, by key: what the person typed,
    /// what it found, or why nothing did (`session.layout_query`, shell.md §8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<LayoutQueryReading>,
}

/// Someone else here in the workspace in front: a person or an agent, by their presence's key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPersonReading {
    pub key: String,
    pub name: String,
}

/// A query typed on the canvas, as its place shows it: what the person looked for, in which app,
/// and what came of it. `state` is `searching` while the app answers, `one` when a single match
/// confident enough is the one the place becomes, `several` with the `matches` to pick from,
/// `none` when nothing matched, and `failed` with the `reason` the app gave.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutQueryReading {
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
    pub matches: Vec<LayoutRefReading>,
}

/// A resource a query found: its address in its app, the name it goes by, and its kind — the same
/// three words a place's record is named by (`session.layout_ref`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutRefReading {
    pub key: String,
    pub address: String,
    pub title: String,
    pub kind: String,
}

/// The person's layout as a shell's views read it: what [`Layout::reading`] answers and
/// [`Layout::resources`] publishes a resource of.
pub type Reading = LayoutReading;
/// A workspace of [`Reading`].
pub type WorkspaceReading = LayoutWorkspaceReading;
/// A row of [`Reading`].
pub type RowReading = LayoutRowReading;
/// A strip of [`Reading`].
pub type StripReading = LayoutStripReading;
/// A place of [`Reading`].
pub type PlaceReading = LayoutPlaceReading;

/// The resources the session publishes now, by id: what a place made from a resource takes its app
/// and view from. The session hands it the API's tree each time it changes, and never reads it
/// inside an invocation.
#[derive(Default)]
pub struct Catalog {
    by_id: BTreeMap<String, Target>,
}

impl Catalog {
    /// Take in `resources` (an action API's whole tree), recursing into children.
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
        self.by_id.get(id).or_else(|| {
            // A place recorded before its app published the item under its stable address
            // (`chat.workspace/channels/c1` for `chat.workspace.channels:c1`) shows the same
            // resource: the record keeps what it holds, and the link keeps opening.
            // And the other way: a stable link to an item its app publishes in the item form.
            let other =
                tilo_actions::stable_item_address(id).or_else(|| tilo_actions::item_form_of(id))?;
            self.by_id.get(&other)
        })
    }

    /// Put `target` under `id` , as the resource is published: a place made from a resource the
    /// session has just published but the API's tree does not hold yet.
    pub fn remember(&mut self, id: &str, target: Target) {
        self.by_id.insert(id.to_owned(), target);
    }

    /// Remember `resource` as its app publishes it: the app, the views it publishes, and what it
    /// is called — the same target the API's tree yields for it, so a place made from a resource
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
                place_view: views
                    .iter()
                    .find(|view| view.place)
                    .map(|view| view.id.clone()),
                place_title: views
                    .iter()
                    .find(|view| view.place)
                    .map_or_else(String::new, |view| view.label.clone()),
                views: views.into_iter().map(|view| view.id.clone()).collect(),
                title: resource.label.clone(),
                kind: resource.kind.clone(),
                app_title: resource
                    .resource_type
                    .as_ref()
                    .map(|named| named.app.one.clone())
                    .unwrap_or_default(),
            },
        );
    }
    for child in &resource.children {
        collect_targets(child, out);
    }
}

/// The shell this build's layout is: what every row, strip and place of a reading says it was
/// made by, so a person running a shell of their own reads what this one made and it reads theirs.
const SHELL: &str = "so.tilo.shell.strips";

/// The layout `tree` as the person's layout, by key: every workspace, its rows, every window
/// group and every window, each as the layout has it, with what each window shows (`resources`) and
/// what the resource it shows is called (`catalog`).
pub(super) fn reading_of(
    tree: &Tree,
    resources: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> Reading {
    let mut reading = Reading {
        front: crate::desktop::navigation::active_route(tree)
            .map(|route| route.group.to_string())
            .unwrap_or_default(),
        ..Reading::default()
    };
    for workspace in &tree.workspaces {
        reading.workspaces.push(WorkspaceReading {
            key: workspace.id().to_string(),
            name: crate::desktop::names::workspace(&workspace.name),
            // A layout holds every workspace the person is in and shows one: the shell draws the
            // rows of the one in front, so the reading says which that is.
            current: workspace.id() == tree.active,
        });
        let front = tree.active_strip_for(workspace);
        for (at, row) in workspace.strips.iter().enumerate() {
            reading.rows.push(RowReading {
                key: row.id.0.to_string(),
                workspace: workspace.id().to_string(),
                name: crate::desktop::names::strip_named(&row.name, at),
                shell: SHELL.to_owned(),
                current: Some(row.id) == front,
            });
            for group in &row.window_groups {
                let canvas = workspace.layout.place_of(group.id);
                let float = workspace.layout.float_of(group.id);
                reading.strips.push(StripReading {
                    key: group.id.to_string(),
                    workspace: workspace.id().to_string(),
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
                    canvas_hugs: canvas.is_some_and(|held| held.hugs),
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
                        .push(place_reading(tab, group.id, resources, catalog, names));
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
    resources: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> PlaceReading {
    let resource = resources.get(&tab.id).cloned().unwrap_or_default();
    // A window showing a resource is named by that resource: its window shows it, so its title is the
    // resource's, unless the person named the window itself.
    let resource_title = catalog
        .get(&resource)
        .map(|target| target.title.trim().to_owned())
        .unwrap_or_default();
    let title = if tab.app_title.is_none() && !resource_title.is_empty() {
        resource_title
    } else {
        tab.title.clone()
    };
    PlaceReading {
        key: tab.id.to_string(),
        strip: strip.to_string(),
        shows: tilo_desktop_model::apps::canonical_app_id(&tab.tile)
            .map(String::from)
            .unwrap_or_default(),
        shown_as: tab.view.clone(),
        resource,
        title: tab.title.clone(),
        name: names(&shown_app(&tab.tile), &title),
        pinned: tab.pinned,
        shell: SHELL.to_owned(),
    }
}

/// The app a window belongs to, as the layout names it: the app's id, or a placeholder's own label
/// for an app this device does not have.
pub(super) fn shown_app(tile: &TileContent) -> String {
    match tile {
        TileContent::AppPlaceholder { label, .. } | TileContent::Stub { label, .. } => {
            label.clone()
        }
        held => tilo_desktop_model::apps::canonical_app_id(held)
            .map(String::from)
            .unwrap_or_default(),
    }
}
