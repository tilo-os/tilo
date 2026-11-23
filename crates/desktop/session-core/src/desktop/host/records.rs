//! What each window of a host's desktop shows, as the workspace's shared layout names it: the
//! records a joining host adopts and the records it writes back.
//!
//! A shared layout names a window's record by its route ([`DocumentId::route`]): the package
//! primary's route is empty, every other record's is its id ([`wire_document`]). A host that joins a
//! layout adopts the record each of its windows shows ([`adopted`]) and writes every one of them back
//! ([`live`]), so it never holds a window on another record than the layout showed
//! (`tilo_desktop_model::shared_resource::guard` refuses such a write). What a host cannot bind yet —
//! an application whose code has not loaded, one it does not run, or a record it does not read —
//! keeps the layout's spelling verbatim, as the layout keeps the record a window held.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::{Layout, TabId, TileContent};
use ui::resource::{DocumentCheckout, DocumentId, ResourceType};

/// The record `id` travels under on the workspace's shared layout: its route, empty for the
/// package's primary record.
#[must_use]
pub fn wire_document(id: &DocumentId) -> &str {
    id.route()
}

/// The application `tab` of `tree` runs, or `None` for a window that is not an application's.
#[must_use]
pub fn app_of(tree: &Layout, tab: TabId) -> Option<&str> {
    match &tree.tab(tab)?.tile {
        TileContent::LocalApp { app_id, .. } => Some(app_id),
        _ => None,
    }
}

/// What `tab` shows, as presence names it: the application it runs and its record's wire spelling
/// ([`wire_document`]), or `None` for a window no component bound a record to yet.
#[must_use]
pub fn of_window<S: BuildHasher>(
    tree: &Layout,
    bindings: &HashMap<TabId, DocumentCheckout, S>,
    tab: TabId,
) -> Option<(String, String)> {
    let app = app_of(tree, tab)?;
    let checkout = bindings.get(&tab)?;
    Some((app.to_owned(), wire_document(&checkout.id).to_owned()))
}

/// `(tab, record)` for every window of `tree` whose component bound a record, or that keeps the
/// record the layout showed it, in wire spelling.
#[must_use]
pub fn live<S: BuildHasher>(
    bindings: &HashMap<TabId, DocumentCheckout, S>,
    held: &HashMap<TabId, String, S>,
    tree: &Layout,
) -> Vec<(TabId, String)> {
    let live: HashSet<TabId> = tree.tabs().map(|tab| tab.id).collect();
    let mut records: Vec<(TabId, String)> = bindings
        .iter()
        .filter(|(tab, _)| live.contains(tab))
        .map(|(tab, checkout)| (*tab, wire_document(&checkout.id).to_owned()))
        .chain(
            held.iter()
                .filter(|(tab, _)| live.contains(tab) && !bindings.contains_key(tab))
                .map(|(tab, record)| (*tab, record.clone())),
        )
        .collect();
    records.sort_unstable_by_key(|(tab, _)| *tab);
    records
}

/// The window records of `layout` a host takes as its own: bound to the record the layout shows for
/// each window whose application `kind_of` runs (the kind of record its component mounts), and
/// held under the layout's own spelling for every other one, until the application that binds it
/// arrives or for good when none does.
#[must_use]
pub fn adopted(
    layout: &SharedDesktop,
    kind_of: impl Fn(&str) -> Option<ResourceType>,
) -> (HashMap<TabId, DocumentCheckout>, HashMap<TabId, String>) {
    let apps: HashMap<TabId, &str> = layout
        .tree
        .tabs()
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.as_str())),
            _ => None,
        })
        .collect();
    let mut bound = HashMap::new();
    let mut held = HashMap::new();
    for (tab, record) in &layout.records {
        let opened = apps
            .get(tab)
            .and_then(|app| kind_of(app))
            .and_then(|kind| Some((DocumentId::from_route(record)?, kind)));
        match opened {
            Some((id, kind)) => {
                bound.insert(*tab, DocumentCheckout::new(id, kind));
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
    use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth, Workspace, WorkspaceId};

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

    /// One strip (10) with a window per `(tab, app, record)` the layout shows.
    fn layout(windows: &[(TabId, &str, &str)]) -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
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
            tree: Layout {
                active: workspace.id(),
                workspaces: vec![workspace],
                ..Layout::default()
            },
            records: windows
                .iter()
                .map(|(id, _, record)| (*id, (*record).to_owned()))
                .collect(),
            items: Vec::new(),
            authors: windows
                .iter()
                .map(|(id, _, _)| (*id, "ada".to_owned()))
                .collect(),
            opened_by: std::collections::BTreeMap::new(),
        }
    }

    fn running(app: &str) -> Option<ResourceType> {
        ["so.tilo.chat", "so.tilo.notes"]
            .contains(&app)
            .then(|| ResourceType::new("test.record"))
    }

    tilo_test_env::scenario! {
        fn a_hosts_records_follow_the_windows_the_layout_shows;

        phase a_joining_host_adopts_every_record_the_layout_shows;
        // A host that joins a shared layout holds every window on the record the layout shows, the
        // package primary under the route it travels as.
        {
            let layout = layout(&[
                (1, "so.tilo.chat", ""),
                (2, "so.tilo.notes", "derived:default@tab:0000000000000002"),
            ]);
            let (bound, held) = adopted(&layout, running);
            assert!(held.is_empty(), "{held:?}");
            assert_eq!(
                bound[&1].id,
                DocumentId::package_primary(),
                "chat's window shows the workspace's primary record"
            );
            assert_eq!(
                bound[&2].id,
                DocumentId::from_route("derived:default@tab:0000000000000002").unwrap()
            );
            assert_eq!(
                live(&bound, &held, &layout.tree),
                layout.records,
                "the host writes each record back as the layout showed it"
            );
        }

        phase a_window_whose_application_is_not_running_keeps_its_record;
        // An application whose code has not arrived, and one a host does not run, keep their windows'
        // records: the host writes them back verbatim rather than leaving them out.
        {
            let shown = layout(&[
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

            // The host runs chat now: it binds that window to the record the layout showed, and keeps
            // the one whose application it does not run.
            let (bound, held) = adopted(&shown, |app| running(app).filter(|_| app == "so.tilo.chat"));
            assert_eq!(bound[&1].id, DocumentId::package_primary());
            assert!(held.contains_key(&3), "{held:?}");
            assert_eq!(
                live(&bound, &held, &shown.tree),
                vec![
                    (1, String::new()),
                    (3, "derived:default@tab:0000000000000003".to_owned())
                ]
            );

            // A spelling no record reads as a route is held, not dropped, so the host still shows the
            // layout what its window shows.
            let legacy = layout(&[(1, "so.tilo.chat", "@tilo:package-primary")]);
            let (bound, held) = adopted(&legacy, running);
            assert!(bound.is_empty(), "{bound:?}");
            assert_eq!(live(&bound, &held, &legacy.tree), legacy.records);
        }

        phase the_primary_record_travels_as_its_route;
        // The package primary travels as the empty route; every other record travels as its id.
        {
            assert_eq!(wire_document(&DocumentId::package_primary()), "");
            let derived = DocumentId::from_route("derived:default@tab:0000000000000002").unwrap();
            assert_eq!(
                wire_document(&derived),
                "derived:default@tab:0000000000000002"
            );
        }
    }
}
