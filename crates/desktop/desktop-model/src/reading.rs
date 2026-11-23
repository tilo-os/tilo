//! The person's layout as a shell reads it: the reading published as `session.layout`, which a
//! shell's views read (`type Layout (origin: session.layout)`), and the walk of the layout's tree
//! every host makes it with ([`of`]). A native session and a page showing a workspace's canvas on
//! the web read the same layout the same way.

use serde::{Deserialize, Serialize};

use crate::{GroupId, Layout as Tree, Tab};

/// A workspace the person's layout shows, as their layout names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutWorkspaceReading {
    pub key: String,
    pub name: String,
    /// Whether it is the workspace in front: a layout holds several and shows one, so a shell draws
    /// the rows of this one and no other.
    pub current: bool,
    /// Where the workspace opens as a page on the web, when the shell is a page
    /// (`/w/<workspace>/desktop`): what its workspace card links to. Empty on a device, where
    /// switching to a workspace shows it in the same layout.
    pub home: String,
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

/// The shell this build's layout is: what every row, strip and place of a reading says it was
/// made by, so a person running a shell of their own reads what this one made and it reads theirs.
pub const SHELL: &str = "so.tilo.shell.strips";

/// How a host names what the layout leaves to it: a workspace as the person reads it (its given
/// name, or the host's word for an unnamed one), and a row by its given name and where it stands.
pub struct Naming<'a> {
    pub workspace: &'a dyn Fn(&str) -> String,
    pub row: &'a dyn Fn(&str, usize) -> String,
}

/// The layout `tree` as the person's layout, by key: every workspace, its rows, every window
/// group and every window, each as the layout has it. `front` is the window group this device has
/// in front (empty for none), and `place` makes each window's place from what the host knows its
/// window shows.
pub fn of(
    tree: &Tree,
    front: String,
    naming: &Naming<'_>,
    mut place: impl FnMut(&Tab, GroupId) -> LayoutPlaceReading,
) -> LayoutReading {
    let mut reading = LayoutReading {
        front,
        ..LayoutReading::default()
    };
    for workspace in &tree.workspaces {
        reading.workspaces.push(LayoutWorkspaceReading {
            key: workspace.id().to_string(),
            name: (naming.workspace)(&workspace.name),
            // A layout holds every workspace the person is in and shows one: the shell draws the
            // rows of the one in front, so the reading says which that is.
            current: workspace.id() == tree.active,
            home: String::new(),
        });
        let front = tree.active_strip_for(workspace);
        for (at, row) in workspace.strips.iter().enumerate() {
            reading.rows.push(LayoutRowReading {
                key: row.id.0.to_string(),
                workspace: workspace.id().to_string(),
                name: (naming.row)(&row.name, at),
                shell: SHELL.to_owned(),
                current: Some(row.id) == front,
            });
            for group in &row.window_groups {
                let canvas = workspace.layout.place_of(group.id);
                let float = workspace.layout.float_of(group.id);
                reading.strips.push(LayoutStripReading {
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
                    reading.places.push(place(tab, group.id));
                }
            }
        }
    }
    reading
}
