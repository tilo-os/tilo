//! Current-person bookmark operations and projection; never shared document mutations.
use super::{ActionBinding, BrowserOperation, BrowserWidget, LibrarySection, Theme};
use crate::personal_bookmark_view::{self as view, ViewRequest};
use crate::personal_bookmarks::{PersonalBookmarkState, PersonalBookmarks};
use std::rc::Rc;
use ui::framework::LiveRow;
use ui::tilo_resource::Effect as _;
use ui::widget::Element;

use crate::resource::CATALOG;
fn source(value: &str) -> Result<[u8; 32], String> {
    let hash = blake3::Hash::from_hex(value)
        .map_err(|_| "Invalid personal bookmark source".to_string())?;
    if hash.to_hex().as_str() != value {
        return Err("Invalid personal bookmark source".into());
    }
    Ok(*hash.as_bytes())
}
impl BrowserWidget {
    pub(super) fn perform_personal_bookmark_operation(
        &mut self,
        operation: BrowserOperation,
    ) -> Result<(), String> {
        let record = self
            .person_bookmarks
            .as_ref()
            .ok_or("The current personal bookmark library is unavailable")?;
        match operation {
            BrowserOperation::BookmarkPersonalPage { url, title } => {
                if !record.with(|state| state.bookmarks().contains(&url)) {
                    record
                        .invoke(&PersonalBookmarks::ADD_BOOKMARK.with((url, title)))
                        .map_err(|_| "Personal bookmark could not be saved")?;
                }
            }
            BrowserOperation::ImportPersonalBookmarks { bundle } => {
                let decoded = crate::continuity::ImportBundle::decode(&bundle)
                    .map_err(|_| "Invalid personal bookmark import")?;
                if decoded.chunks.iter().any(|chunk| !chunk.history.is_empty()) {
                    return Err("History belongs to its own private resource".into());
                }
                record
                    .invoke(&PersonalBookmarks::IMPORT_BOOKMARKS.with(bundle))
                    .map_err(|_| "Personal bookmark import could not be saved")?;
            }
            BrowserOperation::RemovePersonalBookmark { url } => {
                let bookmarks = record.with(PersonalBookmarks::bookmarks);
                let entry = bookmarks
                    .entries()
                    .iter()
                    .find(|entry| entry.url == url)
                    .ok_or("Personal bookmark no longer exists")?;
                record
                    .invoke(&PersonalBookmarks::REMOVE_BOOKMARK.with((url, entry.tags.clone())))
                    .map_err(|_| "Personal bookmark could not be removed")?;
            }
            BrowserOperation::RemovePersonalImportedNode {
                source: encoded,
                id,
            } => {
                let source = source(&encoded)?;
                if !record.with(|state| {
                    state.imported_trees().iter().any(|tree| {
                        tree.source == source && tree.nodes.iter().any(|node| node.id == id)
                    })
                }) {
                    return Err("Personal bookmark item no longer exists".into());
                }
                record
                    .invoke(&PersonalBookmarks::REMOVE_IMPORTED_NODE.with((encoded, id)))
                    .map_err(|_| "Personal bookmark item could not be removed")?;
            }
            BrowserOperation::ShowPersonalBookmarkFolder {
                source: encoded,
                id,
                offset,
            } => {
                let source = if encoded.is_empty() {
                    None
                } else {
                    Some(source(&encoded)?)
                };
                let valid = source.map_or_else(
                    || id.is_none(),
                    |source| {
                        record.with(|state| {
                            state.imported_trees().iter().any(|tree| {
                                tree.source == source
                                    && id.as_ref().is_none_or(|id| {
                                        tree.nodes.iter().any(|node| {
                                            node.id == *id
                                                && matches!(
                                                    node.kind,
                                                    crate::imported_folders::NodeKind::Folder
                                                )
                                        })
                                    })
                            })
                        })
                    },
                );
                if !valid || offset as usize > crate::continuity::MAX_BUNDLE_ENTRIES {
                    return Err("Personal bookmark folder no longer exists".into());
                }
                self.personal_bookmark_location = view::Location {
                    source,
                    folder: id,
                    offset: offset as usize,
                };
                self.show_library = true;
                self.show_settings = false;
                self.library_section = LibrarySection::Bookmarks;
            }
            _ => return Err("Not a personal bookmark operation".into()),
        }
        self.follow_personal_bookmarks();
        self.structure_dirty = true;
        Ok(())
    }
    pub(super) fn personal_bookmark_body(&self, theme: &Theme) -> Element {
        let state = Rc::clone(&self.personal_bookmarks);
        let bindings: view::Bindings = Rc::new(move |request| {
            let row = request_resource(&state, request)?;
            let action = row.operations().first()?;
            ActionBinding::unit_for(&row, &action.id).ok()
        });
        let labels = view::LocalizedLabels {
            back: CATALOG.t("browser-personal-bookmarks-back"),
            previous: CATALOG.t("browser-personal-bookmarks-previous"),
            next: CATALOG.t("browser-personal-bookmarks-next"),
            empty: CATALOG.t("browser-personal-bookmarks-empty"),
            remove: CATALOG.t("browser-personal-bookmarks-remove"),
            unnamed_folder: CATALOG.t("browser-personal-bookmarks-unnamed-folder"),
        };
        let personal = view::view(
            &view::ViewState {
                bookmarks: &self.personal_bookmarks.bookmarks,
                imported_trees: &self.personal_bookmarks.imported_trees,
            },
            &self.personal_bookmark_location,
            theme,
            &bindings,
            &labels,
        );
        if self.personal_bookmark_location.source.is_none() && !self.bookmarks.entries().is_empty()
        {
            let workspace = Box::new(ui::widget::Section::titled(
                CATALOG.t("browser-workspace-bookmarks"),
                vec![crate::rows::separated(self.bookmark_rows(), theme)],
            ));
            // With no bookmarks of your own, "No bookmarks here" above the
            // workspace's would contradict the list under it.
            let own = view::visible_requests(
                &view::ViewState {
                    bookmarks: &self.personal_bookmarks.bookmarks,
                    imported_trees: &self.personal_bookmarks.imported_trees,
                },
                &self.personal_bookmark_location,
            );
            if own.is_empty() {
                return workspace;
            }
            return Box::new(
                ui::widget::Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(personal)
                    .push(workspace),
            );
        }
        personal
    }
    pub(super) fn personal_bookmark_resources(&self) -> LiveRow {
        let mut root = LiveRow::typed_with::<crate::row_types::PersonalBookmarksList>(
            crate::personal_bookmarks::SCOPE,
            crate::CATALOG.t("browser-resource-personal-bookmarks"),
            &ui::tilo_resource::RowBindings::new()
                .offer(crate::effects::BrowserEffect::IMPORT_PERSONAL_BOOKMARKS),
        );
        let state = view::ViewState {
            bookmarks: &self.personal_bookmarks.bookmarks,
            imported_trees: &self.personal_bookmarks.imported_trees,
        };
        // An entry of the page is one row: what its open and its removal ask
        // are offered on the same row, once.
        let mut seen: Vec<String> = Vec::new();
        for request in view::visible_requests(&state, &self.personal_bookmark_location) {
            let Some(resource) = requested(&self.personal_bookmarks, &request) else {
                continue;
            };
            if seen.iter().any(|id| id == resource.id()) {
                continue;
            }
            seen.push(resource.id().to_owned());
            root = root.row(resource);
        }
        for request in view::navigation_requests(&state, &self.personal_bookmark_location) {
            if let Some(resource) = request_resource(&self.personal_bookmarks, request) {
                root = root.row(resource);
            }
        }
        root
    }
}
/// The whole row an entry of the page names: its open and its removal
/// together, whether the request asks for either. `None` once the entry is
/// gone, or for a request that names a route rather than an entry.
fn requested(state: &PersonalBookmarkState, request: &ViewRequest) -> Option<LiveRow> {
    use crate::personal_bookmarks::{flat_id, node_id};
    let id = match request {
        ViewRequest::Open { source, id } | ViewRequest::Remove { source, id } => {
            node_id(source, id)
        }
        ViewRequest::FlatOpen { url } | ViewRequest::FlatRemove { url } => flat_id(url),
        ViewRequest::Folder {
            source,
            id: Some(id),
            offset: 0,
        } => node_id(source, id),
        ViewRequest::Folder { .. } | ViewRequest::Overview { .. } => return None,
    };
    crate::personal_bookmarks::resolve(state, &id)
}
pub(crate) fn request_resource(
    state: &PersonalBookmarkState,
    request: ViewRequest,
) -> Option<LiveRow> {
    use crate::imported_folders::NodeKind;
    use crate::personal_bookmarks::{
        Named, flat_id, flat_row, forgotten, named, node_id, opened, removed_from, showing,
    };
    match request {
        ViewRequest::Open { source, id } => {
            let Named::Placement(_, node) = named(state, &node_id(&source, &id))? else {
                return None;
            };
            let NodeKind::Bookmark { url } = &node.kind else {
                return None;
            };
            Some(LiveRow::typed_with::<
                crate::row_types::PersonalBookmarkPlacement,
            >(
                node_id(&source, &id), &node.title, &opened(url)
            ))
        }
        ViewRequest::Remove { source, id } => {
            let Named::Placement(tree, node) = named(state, &node_id(&source, &id))? else {
                return None;
            };
            Some(LiveRow::typed_with::<
                crate::row_types::PersonalBookmarkPlacement,
            >(
                node_id(&source, &id),
                &node.title,
                &removed_from(tree, node),
            ))
        }
        ViewRequest::FlatOpen { url } => {
            let Named::Flat(entry) = named(state, &flat_id(&url))? else {
                return None;
            };
            Some(flat_row(entry, flat_id(&url)))
        }
        ViewRequest::FlatRemove { url } => {
            let Named::Flat(entry) = named(state, &flat_id(&url))? else {
                return None;
            };
            Some(LiveRow::typed_with::<crate::row_types::PersonalBookmark>(
                flat_id(&url),
                &entry.title,
                &forgotten(&url),
            ))
        }
        ViewRequest::Folder {
            source,
            id: Some(id),
            offset: 0,
        } => {
            let Named::Placement(_, node) = named(state, &node_id(&source, &id))? else {
                return None;
            };
            if !matches!(node.kind, NodeKind::Folder) {
                return None;
            }
            Some(LiveRow::typed_with::<
                crate::row_types::PersonalBookmarkFolder,
            >(
                node_id(&source, &id),
                &node.title,
                &showing(&source, Some(node.id.clone()), 0),
            ))
        }
        ViewRequest::Folder { source, id, offset } => {
            navigation_row(&source, id, u32::try_from(offset).ok()?)
        }
        ViewRequest::Overview { offset } => {
            navigation_row(&[0; 32], None, u32::try_from(offset).ok()?)
        }
    }
}
/// The route to another page of the personal bookmarks: `offset` of the folder
/// `folder` of the tree from `source` brought, the overview when both are
/// absent, so each page of the list is a row of its own.
fn navigation_row(source: &[u8; 32], folder: Option<String>, offset: u32) -> Option<LiveRow> {
    let shown = crate::effects::BrowserEffect::ShowPersonalBookmarkFolder {
        source: blake3::Hash::from(*source).to_hex().to_string(),
        id: folder.clone(),
        offset,
    };
    let id = format!(
        "{}.navigation:{}",
        crate::personal_bookmarks::SCOPE,
        blake3::hash(&serde_json::to_vec(&shown.arguments()).ok()?).to_hex()
    );
    Some(LiveRow::typed_with::<
        crate::row_types::PersonalBookmarkNavigation,
    >(
        id,
        crate::CATALOG.t("browser-resource-bookmark-navigation"),
        &crate::personal_bookmarks::showing(source, folder, offset),
    ))
}
