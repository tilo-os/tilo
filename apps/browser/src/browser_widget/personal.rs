//! Current-person bookmark operations and projection; never shared document mutations.
use super::*;
use crate::personal_bookmark_view::{self as view, ViewRequest};
use crate::personal_bookmarks::{PersonalBookmarkState, PersonalBookmarks};
use std::rc::Rc;
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
                    return Err("History belongs to its own private record".into());
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
            let resource = request_resource(&state, request)?;
            let action = resource.actions.first()?;
            ActionBinding::unit_for(&resource, &action.id).ok()
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
    pub(super) fn personal_bookmark_resources(&self) -> Resource {
        let mut root = Resource::new(
            crate::personal_bookmarks::SCOPE,
            "browser.personal-bookmarks",
            "Personal bookmarks",
        )
        .with_action(
            crate::effects::BrowserEffect::ImportPersonalBookmarks {
                bundle: String::new(),
            }
            .bound_asking(&["bundle"]),
        );
        let state = view::ViewState {
            bookmarks: &self.personal_bookmarks.bookmarks,
            imported_trees: &self.personal_bookmarks.imported_trees,
        };
        for request in view::visible_requests(&state, &self.personal_bookmark_location) {
            if let Some(resource) = request_resource(&self.personal_bookmarks, request) {
                if let Some(existing) = root.children.iter_mut().find(|row| row.id == resource.id) {
                    for action in resource.actions {
                        if !existing.actions.iter().any(|have| have.id == action.id) {
                            existing.actions.push(action);
                        }
                    }
                } else {
                    root = root.with_child(resource);
                }
            }
        }
        for request in view::navigation_requests(
            &view::ViewState {
                bookmarks: &self.personal_bookmarks.bookmarks,
                imported_trees: &self.personal_bookmarks.imported_trees,
            },
            &self.personal_bookmark_location,
        ) {
            if let Some(resource) = request_resource(&self.personal_bookmarks, request) {
                root = root.with_child(resource);
            }
        }
        root
    }
}
pub(crate) fn request_resource(
    state: &PersonalBookmarkState,
    request: ViewRequest,
) -> Option<Resource> {
    let resource = match request {
        ViewRequest::Open { source, id } => {
            return crate::personal_bookmarks::resolve(
                state,
                &crate::personal_bookmarks::node_id(&source, &id),
            )
            .map(|mut r| {
                r.actions.retain(|a| a.id == "open");
                r
            });
        }
        ViewRequest::Remove { source, id } => {
            return crate::personal_bookmarks::resolve(
                state,
                &crate::personal_bookmarks::node_id(&source, &id),
            )
            .map(|mut r| {
                r.actions
                    .retain(|a| a.id == "remove-personal-imported-node");
                r
            });
        }
        ViewRequest::FlatOpen { url } => {
            return crate::personal_bookmarks::resolve(
                state,
                &crate::personal_bookmarks::flat_id(&url),
            )
            .map(|mut r| {
                r.actions.retain(|a| a.id == "open");
                r
            });
        }
        ViewRequest::FlatRemove { url } => {
            return crate::personal_bookmarks::resolve(
                state,
                &crate::personal_bookmarks::flat_id(&url),
            )
            .map(|mut r| {
                r.actions.retain(|a| a.id == "remove-personal-bookmark");
                r
            });
        }
        ViewRequest::Folder {
            source,
            id: Some(id),
            offset: 0,
        } => {
            return crate::personal_bookmarks::resolve(
                state,
                &crate::personal_bookmarks::node_id(&source, &id),
            )
            .map(|mut r| {
                r.actions
                    .retain(|a| a.id == "show-personal-bookmark-folder");
                r
            });
        }
        ViewRequest::Folder { source, id, offset } => {
            crate::effects::BrowserEffect::ShowPersonalBookmarkFolder {
                source: blake3::Hash::from(source).to_hex().to_string(),
                id,
                offset: u32::try_from(offset).ok()?,
            }
        }
        ViewRequest::Overview { offset } => {
            crate::effects::BrowserEffect::ShowPersonalBookmarkFolder {
                source: String::new(),
                id: None,
                offset: u32::try_from(offset).ok()?,
            }
        }
    };
    let id = format!(
        "{}.navigation:{}",
        crate::personal_bookmarks::SCOPE,
        blake3::hash(&serde_json::to_vec(&resource.arguments()).ok()?).to_hex()
    );
    Some(
        Resource::new(
            id,
            "browser.personal-bookmark-navigation",
            "Bookmark navigation",
        )
        .with_action(resource.bound()),
    )
}
