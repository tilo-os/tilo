//! Personal bookmark folders inside the existing Browser library page.
//!
//! This adapter owns no document state and dispatches no ad-hoc callbacks.
//! Every navigation and item command binds to the host's published resource
//! action. Only one folder and one bounded page are materialised at a time.

use std::rc::Rc;

use ui::icons::Icon;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, Column, Divider, Element, EmptyState, ListRow, Row, Text};

use crate::continuity_library::{Bookmark, Bookmarks};
use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};

const PAGE_SIZE: usize = 50;

/// Borrow the actual personal record projection without copying it into a
/// workspace document or a second view-owned storage model.
pub struct ViewState<'a> {
    pub bookmarks: &'a Bookmarks,
    pub imported_trees: &'a [ImportedTree],
}

/// Host-owned ephemeral presentation, reached by the published folder action.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub source: Option<[u8; 32]>,
    pub folder: Option<String>,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewRequest {
    Open {
        source: [u8; 32],
        id: String,
    },
    Remove {
        source: [u8; 32],
        id: String,
    },
    Folder {
        source: [u8; 32],
        id: Option<String>,
        offset: usize,
    },
    FlatOpen {
        url: String,
    },
    FlatRemove {
        url: String,
    },
    Overview {
        offset: usize,
    },
}

/// Copy resolved through the Browser's existing Fluent catalogue by the host.
pub struct LocalizedLabels {
    pub back: String,
    pub previous: String,
    pub next: String,
    pub empty: String,
    pub remove: String,
    pub unnamed_folder: String,
}

pub type Bindings = Rc<dyn Fn(ViewRequest) -> Option<tilo_actions::ActionBinding<()>>>;

enum Entry<'a> {
    Flat(&'a Bookmark),
    Imported(&'a ImportedTree, &'a ImportedNode),
}

fn entries<'a>(state: &'a ViewState<'a>, location: &Location) -> Vec<Entry<'a>> {
    let mut result = Vec::new();
    if location.source.is_none() {
        let placed: std::collections::BTreeSet<_> = state
            .imported_trees
            .iter()
            .flat_map(|tree| tree.nodes.iter())
            .filter_map(|node| match &node.kind {
                NodeKind::Bookmark { url } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        result.extend(
            state
                .bookmarks
                .entries()
                .iter()
                .filter(|entry| {
                    !placed.contains(entry.url.as_str())
                        || entry.tags.iter().any(|tag| !tag.starts_with("import:"))
                })
                .map(Entry::Flat),
        );
    }
    for tree in state.imported_trees {
        if location.source.is_some_and(|source| source != tree.source) {
            continue;
        }
        let mut nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| node.parent.as_deref() == location.folder.as_deref())
            .collect();
        nodes.sort_by_key(|node| node.position);
        result.extend(nodes.into_iter().map(|node| Entry::Imported(tree, node)));
    }
    result
}

fn node_id(source: &[u8; 32], id: &str) -> String {
    format!(
        "browser.personal-view.{}.{}",
        blake3::Hash::from(*source).to_hex(),
        blake3::hash(id.as_bytes()).to_hex()
    )
}

fn bound_button(button: Button, request: ViewRequest, bindings: &Bindings) -> Button {
    match bindings(request) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

fn navigation(location: &Location, offset: usize) -> ViewRequest {
    location
        .source
        .map_or(ViewRequest::Overview { offset }, |source| {
            ViewRequest::Folder {
                source,
                id: location.folder.clone(),
                offset,
            }
        })
}

fn parent_request(state: &ViewState<'_>, location: &Location) -> Option<ViewRequest> {
    let source = location.source?;
    let Some(id) = location.folder.as_deref() else {
        return Some(ViewRequest::Overview { offset: 0 });
    };
    let parent = state
        .imported_trees
        .iter()
        .find(|tree| tree.source == source)
        .and_then(|tree| tree.nodes.iter().find(|node| node.id == id))
        .and_then(|node| node.parent.clone());
    Some(ViewRequest::Folder {
        source,
        id: parent,
        offset: 0,
    })
}

fn page_offset(count: usize, requested: usize) -> usize {
    if count == 0 {
        0
    } else {
        requested.min((count - 1) / PAGE_SIZE * PAGE_SIZE)
    }
}

/// Exactly the Back, Previous and Next routes rendered by the active view,
/// for the host's public resource catalogue and automation surface.
pub fn navigation_requests(state: &ViewState<'_>, location: &Location) -> Vec<ViewRequest> {
    let count = entries(state, location).len();
    let offset = page_offset(count, location.offset);
    let mut requests = Vec::new();
    if let Some(parent) = parent_request(state, location) {
        requests.push(parent);
    }
    if offset > 0 {
        requests.push(navigation(location, offset.saturating_sub(PAGE_SIZE)));
    }
    if offset + PAGE_SIZE < count {
        requests.push(navigation(location, offset + PAGE_SIZE));
    }
    requests
}

fn entry_requests(entry: &Entry<'_>) -> Option<(ViewRequest, ViewRequest)> {
    match entry {
        Entry::Flat(bookmark) => Some((
            ViewRequest::FlatOpen {
                url: bookmark.url.clone(),
            },
            ViewRequest::FlatRemove {
                url: bookmark.url.clone(),
            },
        )),
        Entry::Imported(tree, node) => {
            let open = match &node.kind {
                NodeKind::Folder => ViewRequest::Folder {
                    source: tree.source,
                    id: Some(node.id.clone()),
                    offset: 0,
                },
                NodeKind::Bookmark { .. } => ViewRequest::Open {
                    source: tree.source,
                    id: node.id.clone(),
                },
                NodeKind::Separator => return None,
            };
            Some((
                open,
                ViewRequest::Remove {
                    source: tree.source,
                    id: node.id.clone(),
                },
            ))
        }
    }
}

/// Item routes for exactly the current rendered page. The host publishes these
/// beside navigation routes; off-page resources remain reachable by search.
pub fn visible_requests(state: &ViewState<'_>, location: &Location) -> Vec<ViewRequest> {
    let entries = entries(state, location);
    let offset = page_offset(entries.len(), location.offset);
    entries
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .filter_map(entry_requests)
        .flat_map(|(open, remove)| [open, remove])
        .collect()
}

fn entry_view(entry: &Entry<'_>, bindings: &Bindings, labels: &LocalizedLabels) -> Element {
    let Some((request, remove_request)) = entry_requests(entry) else {
        return Box::new(Divider::horizontal());
    };
    match entry {
        Entry::Flat(bookmark) => {
            let id = crate::personal_bookmarks::flat_id(&bookmark.url);
            let title = if bookmark.title.is_empty() {
                &bookmark.url
            } else {
                &bookmark.title
            };
            let mut row =
                ListRow::new(format!("{id}.personal.open"), title).secondary(&bookmark.url);
            if let Some(binding) = bindings(request) {
                row = row.on_action(binding);
            }
            let remove = bound_button(
                Button::icon_only(
                    &format!("{id}.personal.remove"),
                    Icon::Trash2,
                    &labels.remove,
                ),
                remove_request,
                bindings,
            );
            Box::new(crate::rows::with_buttons(row, vec![remove]))
        }
        Entry::Imported(tree, node) => {
            if matches!(node.kind, NodeKind::Separator) {
                return Box::new(Divider::horizontal());
            }
            let id = node_id(&tree.source, &node.id);
            let (title, icon, secondary) = match &node.kind {
                NodeKind::Folder => (
                    if node.title.is_empty() {
                        &labels.unnamed_folder
                    } else {
                        &node.title
                    },
                    Icon::Folder,
                    None,
                ),
                NodeKind::Bookmark { url } => (
                    if node.title.is_empty() {
                        url
                    } else {
                        &node.title
                    },
                    Icon::Bookmark,
                    Some(url),
                ),
                NodeKind::Separator => return Box::new(Divider::horizontal()),
            };
            let mut row = ListRow::new(format!("{id}.open"), title).icon(icon);
            if matches!(node.kind, NodeKind::Folder) {
                row = row.disclosure();
            }
            if let Some(secondary) = secondary {
                row = row.secondary(secondary);
            }
            if let Some(binding) = bindings(request) {
                row = row.on_action(binding);
            }
            let remove = bound_button(
                Button::icon_only(&format!("{id}.remove"), Icon::Trash2, &labels.remove),
                remove_request,
                bindings,
            );
            Box::new(crate::rows::with_buttons(row, vec![remove]))
        }
    }
}

/// Compose the active personal bookmark folder inside the existing library.
/// Source order, separators and repeated placements are preserved. Every item
/// stays reachable through bounded folder pages rather than a truncated list.
pub fn view(
    state: &ViewState<'_>,
    location: &Location,
    theme: &Theme,
    bindings: &Bindings,
    labels: &LocalizedLabels,
) -> Element {
    let entries = entries(state, location);
    let offset = page_offset(entries.len(), location.offset);
    let mut navigation = navigation_requests(state, location).into_iter();
    let mut body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s0);
    if location.source.is_some()
        && let Some(request) = navigation.next()
    {
        let mut heading = Row::new().no_wrap().push(Box::new(bound_button(
            Button::new("browser.personal-view.back", &labels.back),
            request,
            bindings,
        )));
        if let Some(title) = state
            .imported_trees
            .iter()
            .find(|tree| Some(tree.source) == location.source)
            .and_then(|tree| {
                tree.nodes
                    .iter()
                    .find(|node| Some(&node.id) == location.folder.as_ref())
            })
            .map(|node| node.title.as_str())
        {
            heading = heading.push(Box::new(Text::new(title).with_role(TextRole::Heading)));
        }
        body = body.push(Box::new(heading));
    }
    if entries.is_empty() {
        return Box::new(body.push(Box::new(EmptyState::new(&labels.empty))));
    }
    for entry in entries.iter().skip(offset).take(PAGE_SIZE) {
        body = body.push(entry_view(entry, bindings, labels));
    }
    if offset > 0 || offset + PAGE_SIZE < entries.len() {
        let mut pages = Row::new().no_wrap();
        if offset > 0
            && let Some(request) = navigation.next()
        {
            pages = pages.push(Box::new(bound_button(
                Button::new("browser.personal-view.previous", &labels.previous),
                request,
                bindings,
            )));
        }
        if offset + PAGE_SIZE < entries.len()
            && let Some(request) = navigation.next()
        {
            pages = pages.push(Box::new(bound_button(
                Button::new("browser.personal-view.next", &labels.next),
                request,
                bindings,
            )));
        }
        body = body.push(Box::new(pages));
    }
    Box::new(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::{BookmarkImport, ImportBatch, ImportBundle};
    use crate::personal_bookmarks::PersonalBookmarks;
    use ui::widget::{ControlBindingKind, Widget};

    fn labels() -> LocalizedLabels {
        static CATALOG: ui::i18n::EmbeddedCatalog = ui::i18n::EmbeddedCatalog::new(
            "en-US",
            &[("en-US", include_str!("../locales/en-US.ftl"))],
        );
        LocalizedLabels {
            back: CATALOG.t("browser-personal-bookmarks-back"),
            previous: CATALOG.t("browser-personal-bookmarks-previous"),
            next: CATALOG.t("browser-personal-bookmarks-next"),
            empty: CATALOG.t("browser-personal-bookmarks-empty"),
            remove: CATALOG.t("browser-personal-bookmarks-remove"),
            unnamed_folder: CATALOG.t("browser-personal-bookmarks-unnamed-folder"),
        }
    }

    fn open_ids(widget: &dyn Widget, result: &mut Vec<String>) {
        if let Some(id) = widget
            .test_id()
            .filter(|id| id.rsplit('.').next() == Some("open"))
        {
            result.push(id.to_string());
        }
        for child in widget.children() {
            open_ids(child, result);
        }
    }

    fn imported_fixture(
        with_native_bookmark: bool,
    ) -> crate::personal_bookmarks::PersonalBookmarkState {
        let source = [3; 32];
        let url = "https://example.org/";
        let mut bundle = ImportBundle::from_batch(ImportBatch {
            source,
            bookmarks: vec![BookmarkImport {
                url: url.into(),
                title: "Imported page".into(),
            }],
            history: Vec::new(),
        })
        .unwrap();
        bundle.imported_tree = Some(ImportedTree {
            source,
            nodes: vec![
                ImportedNode {
                    id: "root".into(),
                    parent: None,
                    position: 0,
                    title: "Research".into(),
                    kind: NodeKind::Folder,
                },
                ImportedNode {
                    id: "first".into(),
                    parent: Some("root".into()),
                    position: 0,
                    title: "First placement".into(),
                    kind: NodeKind::Bookmark { url: url.into() },
                },
                ImportedNode {
                    id: "separator".into(),
                    parent: Some("root".into()),
                    position: 1,
                    title: String::new(),
                    kind: NodeKind::Separator,
                },
                ImportedNode {
                    id: "second".into(),
                    parent: Some("root".into()),
                    position: 2,
                    title: "Second placement".into(),
                    kind: NodeKind::Bookmark { url: url.into() },
                },
            ],
        });
        let mut personal = ui::tilo_resource::Replica::<PersonalBookmarks>::new();
        if with_native_bookmark {
            personal
                .apply(
                    PersonalBookmarks::ADD_BOOKMARK
                        .with((url.to_owned(), "My own bookmark".to_owned())),
                )
                .unwrap();
        }
        personal
            .apply(
                PersonalBookmarks::IMPORT_BOOKMARKS.with(serde_json::to_string(&bundle).unwrap()),
            )
            .unwrap();
        personal.state().unwrap().projected()
    }

    #[test]
    fn actual_personal_folder_view_preserves_order_separators_and_bound_open_actions() {
        let state = Rc::new(imported_fixture(true));
        let binding_state = Rc::clone(&state);
        let bindings: Bindings = Rc::new(move |request| {
            let resource =
                crate::browser_widget::personal::request_resource(&binding_state, request)?;
            let action = resource.actions.first()?;
            tilo_actions::ActionBinding::unit_for(&resource, &action.id).ok()
        });
        let adapter = ViewState {
            bookmarks: &state.bookmarks,
            imported_trees: &state.imported_trees,
        };
        let overview = view(
            &adapter,
            &Location::default(),
            &Theme::default(),
            &bindings,
            &labels(),
        );
        let mut ids = Vec::new();
        open_ids(overview.as_ref(), &mut ids);
        // A genuine pre-existing flat bookmark remains; the two imported URL
        // placements are reached through their folder, not flattened into it.
        assert_eq!(ids.len(), 2);
        let imported_only = imported_fixture(false);
        let imported_adapter = ViewState {
            bookmarks: &imported_only.bookmarks,
            imported_trees: &imported_only.imported_trees,
        };
        let imported_overview = view(
            &imported_adapter,
            &Location::default(),
            &Theme::default(),
            &bindings,
            &labels(),
        );
        ids.clear();
        open_ids(imported_overview.as_ref(), &mut ids);
        assert_eq!(ids, vec![format!("{}.open", node_id(&[3; 32], "root"))]);
        let location = Location {
            source: Some([3; 32]),
            folder: Some("root".into()),
            offset: 0,
        };
        let folder = view(&adapter, &location, &Theme::default(), &bindings, &labels());
        let children = folder.children();
        assert_eq!(children.len(), 4);
        assert_eq!(children.get(2).unwrap().name(), "Divider");
        ids.clear();
        open_ids(folder.as_ref(), &mut ids);
        assert_eq!(
            ids,
            vec![
                format!("{}.open", node_id(&[3; 32], "first")),
                format!("{}.open", node_id(&[3; 32], "second"))
            ]
        );
        let controls = ui::widget::controls::control_action_bindings(folder.as_ref());
        for (control_id, item) in ids.iter().zip(["first", "second"]) {
            let control = controls
                .iter()
                .find(|control| control.control_id == *control_id)
                .unwrap();
            let ControlBindingKind::Action(address) = &control.kind else {
                panic!("bookmark must use its public action")
            };
            assert_eq!(
                address.resource,
                crate::personal_bookmarks::node_id(&[3; 32], item)
            );
            assert_eq!(address.action, "open");
            let resource = crate::browser_widget::personal::request_resource(
                &state,
                ViewRequest::Open {
                    source: [3; 32],
                    id: item.into(),
                },
            )
            .unwrap();
            assert_eq!(resource.actions.first().unwrap().id, "open");
        }
        assert!(
            controls
                .iter()
                .all(|control| matches!(control.kind, ControlBindingKind::Action(_)))
        );
    }
}
