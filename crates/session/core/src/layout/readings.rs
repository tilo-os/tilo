//! The person's layout as a shell reads it: the reading's types, the layout `reading_of`
//! turns into one, and the catalogue of what a place shows ([`Catalog`]).

use std::collections::BTreeMap;

use tilo_actions::ResourceSpec;
use tilo_desktop_model::reading::SHELL;
use tilo_desktop_model::{GroupId, Layout as Tree, Tab, TabId, TileContent};

use super::{Names, Target};
pub use tilo_desktop_model::reading::{
    LayoutPersonReading, LayoutPlaceReading, LayoutQueryReading, LayoutReading, LayoutRefReading,
    LayoutRowReading, LayoutStripReading, LayoutWorkspaceReading,
};

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
