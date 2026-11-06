//! What each window of this page's desktop shows, as the workspace's shared
//! desk names it: the records a joining page adopts and the records it writes
//! back.
//!
//! A shared desk names a window's record by its route ([`RecordId::route`]):
//! the package primary's route is empty, every other record's is its id
//! ([`wire_record`]). A page that joins a desk adopts the record each of its
//! windows shows ([`adopted`]) and writes every one of them back ([`live`]),
//! so it never holds a window on another record than the desk showed
//! (`tilo_desktop_model::shared_record::guard` refuses such a write). What
//! this page cannot bind yet — an application whose code has not loaded, one
//! it does not run, or a record it does not read — keeps the desk's spelling
//! verbatim, as native keeps the record a window held.

use std::collections::{HashMap, HashSet};

use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::{DesktopTree, TabId, TileContent};
use ui::record::{RecordCheckout, RecordId, RecordKind};

/// The record `id` travels under on the workspace's shared desk: its route,
/// empty for the package's primary record.
pub(crate) fn wire_record(id: &RecordId) -> &str {
    id.route()
}

/// `(tab, record)` for every window of `tree` whose component bound a record,
/// or that keeps the record the desk showed it, in wire spelling.
pub(crate) fn live(
    bindings: &HashMap<TabId, RecordCheckout>,
    held: &HashMap<TabId, String>,
    tree: &DesktopTree,
) -> Vec<(TabId, String)> {
    let mut live = HashSet::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                live.extend(group.tabs.iter().map(|tab| tab.id));
            }
        }
    }
    let mut records: Vec<(TabId, String)> = bindings
        .iter()
        .filter(|(tab, _)| live.contains(tab))
        .map(|(tab, checkout)| (*tab, wire_record(&checkout.id).to_owned()))
        .chain(
            held.iter()
                .filter(|(tab, _)| live.contains(tab) && !bindings.contains_key(tab))
                .map(|(tab, record)| (*tab, record.clone())),
        )
        .collect();
    records.sort_unstable_by_key(|(tab, _)| *tab);
    records
}

/// The window records of `desk` this page takes as its own: bound to the
/// record the desk shows for each window whose application `kind_of` runs
/// (the kind of record its component mounts), and held under the desk's own
/// spelling for every other one, until the application that binds it arrives
/// or for good when none does.
pub(crate) fn adopted(
    desk: &SharedDesktop,
    kind_of: impl Fn(&str) -> Option<RecordKind>,
) -> (HashMap<TabId, RecordCheckout>, HashMap<TabId, String>) {
    let apps: HashMap<TabId, &str> = desk
        .tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.as_str())),
            _ => None,
        })
        .collect();
    let mut bound = HashMap::new();
    let mut held = HashMap::new();
    for (tab, record) in &desk.records {
        let opened = apps
            .get(tab)
            .and_then(|app| kind_of(app))
            .and_then(|kind| Some((RecordId::from_route(record)?, kind)));
        match opened {
            Some((id, kind)) => {
                bound.insert(*tab, RecordCheckout::new(id, kind));
            }
            None => {
                held.insert(*tab, record.clone());
            }
        }
    }
    (bound, held)
}

#[cfg(test)]
mod tests {
    use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth, Workspace, WorkspaceSlot};

    use super::*;

    fn tab(id: TabId, app: &str) -> tilo_desktop_model::Tab {
        tilo_desktop_model::Tab {
            id,
            title: app.to_owned(),
            tile: TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    /// One strip (10) with a window per `(tab, app, record)` the desk shows.
    fn desk(windows: &[(TabId, &str, &str)]) -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
        let mut strip = Strip::new(StripId(10), "Lounge");
        for (id, app, _) in windows {
            strip.window_groups.push(WindowGroup {
                // A group id of its own per window; test window ids are small.
                id: id.saturating_add(100),
                tabs: vec![tab(*id, app)],
                active_tab: *id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        workspace.strips.push(strip);
        SharedDesktop {
            tree: DesktopTree {
                active: workspace.slot,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            records: windows
                .iter()
                .map(|(id, _, record)| (*id, (*record).to_owned()))
                .collect(),
            authors: windows
                .iter()
                .map(|(id, _, _)| (*id, "ada".to_owned()))
                .collect(),
        }
    }

    fn running(app: &str) -> Option<RecordKind> {
        ["so.tilo.chat", "so.tilo.notes"]
            .contains(&app)
            .then(|| RecordKind::new("test.record"))
    }

    /// A page that joins a shared desk holds every window on the record the
    /// desk shows, the package primary under the route it travels as.
    #[test]
    fn a_joining_page_adopts_every_record_the_desk_shows() {
        let desk = desk(&[
            (1, "so.tilo.chat", ""),
            (2, "so.tilo.notes", "derived:default@tab:0000000000000002"),
        ]);
        let (bound, held) = adopted(&desk, running);
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(
            bound[&1].id,
            RecordId::package_primary(),
            "chat's window shows the workspace's primary record"
        );
        assert_eq!(
            bound[&2].id,
            RecordId::from_route("derived:default@tab:0000000000000002").unwrap()
        );
        assert_eq!(
            live(&bound, &held, &desk.tree),
            desk.records,
            "the page writes each record back as the desk showed it"
        );
    }

    /// An application whose code has not arrived, and one this page does not
    /// run, keep their windows' records: the page writes them back verbatim
    /// rather than leaving them out.
    #[test]
    fn a_window_whose_application_is_not_running_keeps_its_record() {
        let shown = desk(&[
            (1, "so.tilo.chat", ""),
            (
                3,
                "so.tilo.elsewhere",
                "derived:default@tab:0000000000000003",
            ),
        ]);
        let (bound, held) = adopted(&shown, |_| None);
        assert!(bound.is_empty(), "nothing runs yet");
        assert_eq!(held[&1], "");
        assert_eq!(
            live(&bound, &held, &shown.tree),
            shown.records,
            "no window's record is lost"
        );

        // The page runs chat now: it binds that window to the record the desk
        // showed, and keeps the one whose application it does not run.
        let (bound, held) = adopted(&shown, |app| running(app).filter(|_| app == "so.tilo.chat"));
        assert_eq!(bound[&1].id, RecordId::package_primary());
        assert!(held.contains_key(&3), "{held:?}");
        assert_eq!(
            live(&bound, &held, &shown.tree),
            vec![
                (1, String::new()),
                (3, "derived:default@tab:0000000000000003".to_owned())
            ]
        );

        // A spelling no record reads as a route is held, not dropped, so the
        // page still shows the desk what its window shows.
        let legacy = desk(&[(1, "so.tilo.chat", "@tilo:package-primary")]);
        let (bound, held) = adopted(&legacy, running);
        assert!(bound.is_empty(), "{bound:?}");
        assert_eq!(live(&bound, &held, &legacy.tree), legacy.records);
    }

    /// The package primary travels as the empty route; every other record
    /// travels as its id.
    #[test]
    fn the_primary_record_travels_as_its_route() {
        assert_eq!(wire_record(&RecordId::package_primary()), "");
        let derived = RecordId::from_route("derived:default@tab:0000000000000002").unwrap();
        assert_eq!(
            wire_record(&derived),
            "derived:default@tab:0000000000000002"
        );
    }
}
