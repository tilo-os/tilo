//! The person's layout as a shell reads it: the reading's types, the layout `reading_of`
//! turns into one, and the catalogue of what a place shows ([`Catalog`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_actions::ResourceSpec;
use tilo_desktop_model::reading::SHELL;
use tilo_desktop_model::{GroupId, Layout as Tree, Tab, TabId, TileContent};

use super::{Names, Target};
pub use tilo_desktop_model::reading::{
    LayoutPersonReading, LayoutPlaceReading, LayoutQueryReading, LayoutReading,
    LayoutRecentReading, LayoutRefReading, LayoutRowReading, LayoutStripReading,
    LayoutWorkspaceReading,
};

/// The display one of the shell's parts draws on, as that part reads it: its id in the device's
/// layout, what a person calls it, where it stands, the scale it draws at, whether it is the
/// display in use, and what Settings says it is while the person asked which screen is which.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutDisplayReading {
    /// The display's id on this device, as the layout's strips name it: `0` at the first.
    pub id: i64,
    /// The connector the compositor knows it by (`eDP-1`, `HDMI-A-1`).
    pub connector: String,
    /// What a person calls it: the panel, the monitor's own name, else its connector.
    pub name: String,
    /// Where it stands in the device's logical layout.
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    /// The scale it draws at.
    pub scale: f64,
    /// Whether it is the display the person made primary (the only one, on a device with one).
    pub primary: bool,
    /// Whether the person asked Settings which screen is which, so this display is saying so: its
    /// number and name stand on it for a moment.
    #[serde(default)]
    pub identify: bool,
    /// The number Settings gives it on its placement canvas, while it is saying so: `0` when it
    /// says nothing.
    #[serde(default)]
    pub identify_number: i64,
    /// What Settings calls it on that canvas, while it is saying so: empty when it says nothing.
    #[serde(default)]
    pub identify_name: String,
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
/// The display a part stands on, as that part reads it.
pub type DisplayReading = LayoutDisplayReading;

/// The resources the session publishes now, by id: what a place made from a resource takes its app
/// and view from. The session hands it the API's tree each time it changes, and never reads it
/// inside an invocation.
#[derive(Default)]
pub struct Catalog {
    by_id: BTreeMap<String, Target>,
    /// Moves whenever what it holds may have.
    revision: u64,
}

impl Catalog {
    /// Take in `resources` (an action API's whole tree), recursing into children.
    pub fn take(&mut self, resources: &[ResourceSpec]) {
        let mut by_id = BTreeMap::new();
        for resource in resources {
            collect_targets(resource, &mut by_id);
        }
        self.by_id = by_id;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Moves whenever what it holds may have.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
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
        self.revision = self.revision.wrapping_add(1);
    }

    /// Remember `resource` as its app publishes it: the app, the views it publishes, and what it
    /// is called — the same target the API's tree yields for it, so a place made from a resource
    /// the session has just published shows it through its app's own view, never as the app's
    /// window.
    pub fn remember_resource(&mut self, resource: &ResourceSpec) {
        collect_targets(resource, &mut self.by_id);
        self.revision = self.revision.wrapping_add(1);
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

/// The layout `tree` as the person's layout, by key: every workspace, its rows, every window
/// group and every window, each as the layout has it, with what each window shows (`resources`) and
/// what the resource it shows is called (`catalog`).
pub(super) fn reading_of(
    tree: &Tree,
    resources: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> Reading {
    let front = crate::desktop::navigation::active_route(tree)
        .map(|route| route.group.to_string())
        .unwrap_or_default();
    tilo_desktop_model::reading::of(
        tree,
        front,
        &tilo_desktop_model::reading::Naming {
            workspace: &crate::desktop::names::workspace,
            row: &crate::desktop::names::strip_named,
        },
        |tab, strip| place_reading(tab, strip, resources, catalog, names),
    )
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
        leaving: false,
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

/// The windows that just left the layout: each kept in the reading, marked `leaving`, while the
/// shell plays its frame's departure — back into the control the person opened it from — and then
/// let go. A window still open reads live, and a window seen for the first time takes the control
/// the person pressed for it.
#[derive(Debug, Default)]
pub(crate) struct Departures {
    /// What the last reading held of each window, by its strip.
    seen: BTreeMap<String, Window>,
    /// The windows that closed since, and when.
    leaving: BTreeMap<String, Left>,
}

/// One window as the reading held it: its strip, its places, and where they stood.
#[derive(Clone, Debug)]
struct Window {
    /// Where its strip stood among the reading's strips: where it stays while it leaves, so the
    /// shell's frame is the one already drawn rather than a copy built anew.
    index: usize,
    /// Where its row stood: kept when the row goes with its last window.
    row_index: usize,
    /// When the window was first read: what tells a window arriving from one a rebuild shows again.
    since: ui::time::Instant,
    /// The control the person pressed for it: where its frame comes out of and, long after it
    /// opened, where its departure goes back into.
    control: Vec<i64>,
    strip: StripReading,
    places: Vec<PlaceReading>,
    row: RowReading,
}

/// A window that closed, and when it did.
#[derive(Clone, Debug)]
struct Left {
    since: ui::time::Instant,
    was: Window,
}

/// How long after it opened a window is read as coming out of its control: long enough for the
/// shell to build its frame, short enough that a view rebuilt later (a style switched) does not
/// replay the flight.
fn arrival() -> ui::time::Duration {
    ui::time::Duration::from_millis(750)
}

/// How long a closed window's frame keeps its place in the reading: the window motion's exit, the
/// time the shell takes to draw it back into its control.
fn departure() -> ui::time::Duration {
    ui::motion::WindowMotion::OPEN.settle_time(
        ui::motion::Direction::Exit,
        ui::tokens::EffectiveMotion::default(),
        ui::tokens::MotionSource::Pointer,
    )
}

impl Departures {
    /// Where a window was asked for from, in the reading's pixels.
    fn control_of(at: ui::kurbo::Point) -> Vec<i64> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a point on a screen, far inside an i64"
        )]
        let (x, y) = (at.x.round() as i64, at.y.round() as i64);
        vec![x, y]
    }

    /// Bring `reading` to what just changed: a window that closed stays in it where it stood, with
    /// its row and places, marked leaving, until its departure has played; a window still open reads
    /// live, and one seen for the first time takes `control` — where the person pressed for it — if
    /// a pointer asked for it.
    pub(crate) fn keep(
        &mut self,
        reading: &mut Reading,
        now: ui::time::Instant,
        control: Option<ui::kurbo::Point>,
    ) {
        let present: std::collections::BTreeSet<String> = reading
            .strips
            .iter()
            .map(|strip| strip.key.clone())
            .collect();
        for (key, was) in &self.seen {
            if !present.contains(key) {
                self.leaving.entry(key.clone()).or_insert_with(|| Left {
                    since: now,
                    was: was.clone(),
                });
            }
        }
        let hold = departure();
        self.leaving
            .retain(|_, left| now.saturating_duration_since(left.since) < hold);
        let control = control.map(Self::control_of);

        // What each live window reads now, and what a window opening now was asked for from.
        let was = std::mem::take(&mut self.seen);
        let mut seen = BTreeMap::new();
        let rows = &reading.rows;
        let places = &reading.places;
        for (index, strip) in reading.strips.iter_mut().enumerate() {
            let held = was.get(&strip.key);
            let control = held.map_or_else(
                || control.clone().unwrap_or_default(),
                |held| held.control.clone(),
            );
            let since = held.map_or(now, |held| held.since);
            // Only a window that just opened is drawn coming out of its control: a frame a
            // rebuild replaces later grows where it stands, as every window's does.
            strip.control = if now.saturating_duration_since(since) < arrival() {
                control.clone()
            } else {
                Vec::new()
            };
            strip.leaving = false;
            let row_index = rows
                .iter()
                .position(|row| row.key == strip.row)
                .unwrap_or_default();
            let places = places
                .iter()
                .filter(|place| place.strip == strip.key)
                .cloned()
                .collect();
            seen.insert(
                strip.key.clone(),
                Window {
                    index,
                    row_index,
                    since,
                    control,
                    strip: strip.clone(),
                    places,
                    row: rows.get(row_index).cloned().unwrap_or_default(),
                },
            );
        }
        self.seen = seen;
        // A window that came back reads live: its departure is over.
        for strip in &reading.strips {
            self.leaving.remove(&strip.key);
        }

        // The windows that closed stand where they stood, marked leaving, with the row a last
        // window took with it.
        for left in self.leaving.values() {
            let mut strip = left.was.strip.clone();
            strip.leaving = true;
            // Its frame goes back into the control the person opened it from, however long ago
            // that was.
            strip.control = left.was.control.clone();
            let at = left.was.index.min(reading.strips.len());
            reading.strips.insert(at, strip);
            let mut place_at = reading
                .places
                .iter()
                .position(|place| place.strip == left.was.strip.key)
                .unwrap_or(reading.places.len());
            for place in &left.was.places {
                let mut place = place.clone();
                place.leaving = true;
                reading.places.insert(place_at, place);
                place_at += 1;
            }
            if !reading.rows.iter().any(|row| row.key == left.was.row.key) {
                let at = left.was.row_index.min(reading.rows.len());
                reading.rows.insert(at, left.was.row.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(key: &str) -> LayoutStripReading {
        LayoutStripReading {
            key: key.to_owned(),
            workspace: "ws".to_owned(),
            row: "r1".to_owned(),
            width: 500,
            active: format!("{key}:front"),
            ..LayoutStripReading::default()
        }
    }

    fn place(strip: &str) -> LayoutPlaceReading {
        LayoutPlaceReading {
            key: format!("{strip}:front"),
            strip: strip.to_owned(),
            ..LayoutPlaceReading::default()
        }
    }

    fn row() -> LayoutRowReading {
        LayoutRowReading {
            key: "r1".to_owned(),
            workspace: "ws".to_owned(),
            ..LayoutRowReading::default()
        }
    }

    fn read(
        strips: Vec<LayoutStripReading>,
        places: Vec<LayoutPlaceReading>,
        rows: Vec<LayoutRowReading>,
    ) -> Reading {
        LayoutReading {
            strips,
            places,
            rows,
            ..LayoutReading::default()
        }
    }

    const CONTROL: ui::kurbo::Point = ui::kurbo::Point::new(40.0, 900.0);

    tilo_test_env::scenario! {
        fn a_window_that_closes_keeps_its_frame_until_its_departure_plays;

        phase a_window_the_person_opens_takes_the_control_they_pressed;
        {
            let now = ui::time::now();
            let mut departures = Departures::default();
            let mut reading = read(vec![strip("s1")], vec![place("s1")], vec![row()]);
            departures.keep(&mut reading, now, Some(CONTROL));
            assert_eq!(
                reading.strips[0].control,
                vec![40, 900],
                "the dock icon the person pressed"
            );
            assert!(!reading.strips[0].leaving, "a window that stands reads live");
        }

        phase a_window_that_closes_stays_where_it_stood_marked_leaving;
        {
            let now = ui::time::now();
            let mut departures = Departures::default();
            let mut open = read(
                vec![strip("s1"), strip("s2")],
                vec![place("s1"), place("s2")],
                vec![row()],
            );
            departures.keep(&mut open, now, Some(CONTROL));
            // The first window of the row closes.
            let mut closed = read(vec![strip("s2")], vec![place("s2")], vec![row()]);
            departures.keep(&mut closed, now, None);
            assert_eq!(closed.strips[0].key, "s1", "it stays where it stood");
            assert!(closed.strips[0].leaving, "marked leaving");
            assert_eq!(
                closed.strips[0].control,
                vec![40, 900],
                "with the control it goes back to"
            );
            let left = closed
                .places
                .iter()
                .find(|place| place.key == "s1:front")
                .expect("its place stays while it leaves");
            assert!(left.leaving, "and the place is leaving too");
            assert!(closed.strips.iter().any(|strip| strip.key == "s2" && !strip.leaving));
        }

        phase a_rows_last_window_takes_its_row_with_it_while_it_leaves;
        {
            let now = ui::time::now();
            let mut departures = Departures::default();
            let mut open = read(vec![strip("s1")], vec![place("s1")], vec![row()]);
            departures.keep(&mut open, now, Some(CONTROL));
            // Closing the only window takes the row too.
            let mut closed = read(Vec::new(), Vec::new(), Vec::new());
            departures.keep(&mut closed, now, None);
            assert!(closed.strips.iter().any(|strip| strip.key == "s1" && strip.leaving));
            assert!(
                closed.rows.iter().any(|row| row.key == "r1"),
                "the row stands while the window leaves in it"
            );
        }

        phase a_departed_window_is_let_go_when_its_frame_is_gone;
        {
            let now = ui::time::now();
            let mut departures = Departures::default();
            let mut open = read(vec![strip("s1")], vec![place("s1")], vec![row()]);
            departures.keep(&mut open, now, Some(CONTROL));
            let mut closed = read(Vec::new(), Vec::new(), Vec::new());
            departures.keep(&mut closed, now, None);
            assert!(closed.strips.iter().any(|strip| strip.key == "s1"));
            // Once the frame's exit has run, the window is gone from the reading.
            let mut after = read(Vec::new(), Vec::new(), Vec::new());
            departures.keep(&mut after, now + departure(), None);
            assert!(after.strips.is_empty(), "the departed window is let go");
            assert!(after.places.is_empty());
            assert!(after.rows.is_empty());
        }

        phase a_window_that_comes_back_reads_live;
        {
            let now = ui::time::now();
            let mut departures = Departures::default();
            let mut open = read(vec![strip("s1")], vec![place("s1")], vec![row()]);
            departures.keep(&mut open, now, Some(CONTROL));
            departures.keep(&mut read(Vec::new(), Vec::new(), Vec::new()), now, None);
            // Undo brings the window back before its departure ended.
            let mut back = read(vec![strip("s1")], vec![place("s1")], vec![row()]);
            departures.keep(&mut back, now, None);
            assert_eq!(back.strips.len(), 1, "the window stands once, not twice");
            assert!(!back.strips[0].leaving, "and it reads live again");
        }
    }
}
