//! The semantic tree the window exposes, and the actions assistive
//! technology and agents perform through it.

use crate::model::PhotoLibrary;
use crate::strings::{tr, tr_args, tr_count, tr_with};
use automerge::AutoCommit;
use ui::ArgValue;
use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::tilo_resource::Id;
use ui::widget::{TextInput, Widget};
use vello::kurbo::Rect;

use super::{
    AlbumEditorHit, CollectionPageHit, DIRECTORY_FAILURE_DETAIL_LIMIT, DirectoryImportMode,
    DirectoryStatusHit, LibraryAnalysisHit, LibraryDestination, MetadataEditorHit,
    MetadataEditorSection, PhotosApp, PlacesHit, SELECTION_ALBUMS_PER_PAGE, SelectionHit,
    card_action_controls, chrome, metadata_filter_id,
};

impl PhotosApp {
    /// The canvas has no intrinsic source name — it is raw pixel data — so the
    /// app supplies the file name and current edit state that a sighted person
    /// gets from the status bar. The status remains a polite live region for
    /// copy/save/open failures without stealing keyboard focus.
    #[allow(
        clippy::too_many_lines,
        reason = "the synthetic canvas accessibility tree keeps each visual hit target beside its stable semantic route"
    )]
    pub(super) fn accessibility_tree(&self) -> AccessNode {
        let image_label = || {
            self.original.as_ref().map_or_else(
                || tr("photos-image"),
                |(path, _)| path.rsplit('/').next().unwrap_or(path).to_owned(),
            )
        };
        let (image_name, mut image_description) = match self.rendered.as_ref() {
            Some(rendered) if rendered.edits.is_identity() => {
                (image_label(), tr("photos-image-help"))
            }
            Some(_) => (
                format!("{} — {}", image_label(), tr("photos-edited")),
                tr("photos-image-help"),
            ),
            None => self.empty_state_text(),
        };
        if let Some(details) = self.viewer_details.as_ref() {
            let mut shared = Vec::new();
            if let Some(description) = details.description.as_deref() {
                shared.push(format!("{}: {description}", tr("photos-description")));
            }
            if let Some(captured_at) = details.captured_at.as_deref() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-captured"),
                    crate::model::capture_label(captured_at)
                ));
            }
            if let Some(place) = details.place.as_deref() {
                shared.push(format!("{}: {place}", tr("photos-place")));
            }
            if !details.people.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-people"),
                    details.people.join(", ")
                ));
            }
            if !details.tags.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-tags"),
                    details.tags.join(", ")
                ));
            }
            if let Some(folder) = details.folder.as_deref() {
                shared.push(format!("{}: {folder}", tr("photos-folder")));
            }
            if !shared.is_empty() {
                image_description.push_str(". ");
                image_description.push_str(&shared.join(". "));
            }
        }
        let canvas = if self.library_mode {
            let mut cards = Vec::new();
            if self.library_destination == LibraryDestination::Analysis {
                cards.extend(
                    self.library_analysis_buttons
                        .iter()
                        .filter_map(|(hit, button)| {
                            let bounds = button.get_rect();
                            if bounds == Rect::ZERO {
                                return None;
                            }
                            let (name, target) = match hit {
                                LibraryAnalysisHit::Start => {
                                    ("photos-a11y-analyze", "photos:analysis:start")
                                }
                                LibraryAnalysisHit::Pause => {
                                    ("photos-analysis-pause", "photos:analysis:pause")
                                }
                                LibraryAnalysisHit::Resume => {
                                    ("photos-a11y-resume-analysis", "photos:analysis:resume")
                                }
                                LibraryAnalysisHit::RetryFailures => {
                                    ("photos-analysis-retry", "photos:analysis:retry")
                                }
                            };
                            Some(
                                AccessNode::new(Role::Button)
                                    .with_name(tr(name))
                                    .with_bounds(bounds)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(button.is_focusable())
                                            .focused(button.is_focused())
                                            .disabled(!button.is_enabled()),
                                    )
                                    .with_action_target(target),
                            )
                        }),
                );
                cards.extend(
                    self.library_analysis_failure_hits
                        .iter()
                        .map(|(id, bounds)| {
                            let failure = self
                                .library_analysis
                                .failures()
                                .find(|failure| failure.photo_id == *id);
                            AccessNode::new(Role::Button)
                                .with_name(failure.map_or_else(
                                    || tr("photos-a11y-open-failed"),
                                    |failure| {
                                        tr_with("photos-a11y-open-named", "name", &failure.name)
                                    },
                                ))
                                .with_description(failure.map_or_else(
                                    || tr("photos-a11y-analysis-failed"),
                                    |failure| failure.message.clone(),
                                ))
                                .with_bounds(*bounds)
                                .with_state(AccessState::new().focusable(true))
                                .with_action_target(format!("photos:analysis:failure:{id}"))
                                .with_hashed_key(format!("photos:analysis-failure:{id}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-analysis-label"))
                    .with_description(tr("photos-a11y-analysis-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:library-analysis")
                    .with_live(Politeness::Polite)
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Albums {
                cards.extend(self.album_cards.iter().filter_map(|(id, bounds)| {
                    let id = id.as_ref()?;
                    let (_, name, count) = self
                        .library_albums
                        .iter()
                        .find(|(album_id, _, _)| album_id == id)?;
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(tr_args(
                                "photos-people-card-name",
                                &[
                                    ("name", ArgValue::Text(name.clone())),
                                    ("count", ArgValue::Number(*count as f64)),
                                ],
                            ))
                            .with_description(tr("photos-a11y-open-album"))
                            .with_bounds(*bounds)
                            .with_state(AccessState::new().focusable(true))
                            .with_hashed_key(format!("photos:album-card:{id}"))
                            .with_action_target(format!("photos:library-album:{id}")),
                    )
                }));
                cards.extend(self.album_edit_cards.iter().map(|(id, bounds)| {
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-edit-album"))
                        .with_bounds(*bounds)
                        .with_state(AccessState::new().focusable(true))
                        .with_hashed_key(format!("photos:album-edit:{id}"))
                        .with_action_target(format!("photos:album-edit:{id}"))
                }));
                AccessNode::new(Role::List)
                    .with_name(tr("photos-albums"))
                    .with_description(tr("photos-a11y-albums-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:albums")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                if self.people_selection.is_some() && !self.people_merge_mode {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name(tr("photos-a11y-person-name"))
                            .with_value(self.people_name_input.value())
                            .with_bounds(self.people_name_input.get_rect())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.people_name_input.is_focused()),
                            )
                            .with_hashed_key("photos:people:name")
                            .with_action_target("photos:people:name"),
                    );
                }
                cards.extend(
                    self.people_controls
                        .iter()
                        .enumerate()
                        .map(|(index, control)| {
                            AccessNode::new(Role::Button)
                                .with_name(control.label.clone())
                                .with_bounds(if control.is_card() {
                                    control.bounds
                                } else {
                                    control.button.get_rect()
                                })
                                .with_state(
                                    AccessState::new()
                                        .focusable(control.button.is_focusable())
                                        .focused(control.button.is_focused())
                                        .disabled(!control.button.is_enabled()),
                                )
                                .with_action_target(format!("photos:people:control:{index}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-people"))
                    .with_description(tr("photos-a11y-people-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:people")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Places {
                cards.extend(self.places_hits.iter().map(|(hit, bounds)| {
                    let (name, target) = match hit {
                        PlacesHit::Place(key) => (
                            "photos-a11y-open-place",
                            format!("photos:places:open:{key}"),
                        ),
                        PlacesHit::Photo(id) => (
                            "photos-a11y-open-photo",
                            format!("photos:places:photo:{id}"),
                        ),
                    };
                    AccessNode::new(Role::Button)
                        .with_name(tr(name))
                        .with_bounds(*bounds)
                        .with_state(AccessState::new().focusable(true))
                        .with_action_target(target)
                }));
                // These semantic controls intentionally share the same pure
                // camera operation as keyboard arrows and pointer drag. They
                // make panning available to screen readers and agents without
                // introducing coordinate-only automation.
                for (name, target) in [
                    ("photos-a11y-pan-left", "photos:places:pan-left"),
                    ("photos-a11y-pan-right", "photos:places:pan-right"),
                    ("photos-a11y-pan-up", "photos:places:pan-up"),
                    ("photos-a11y-pan-down", "photos:places:pan-down"),
                ] {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(tr(name))
                            .with_description(tr("photos-a11y-pan-description"))
                            .with_bounds(self.places_map_bounds)
                            .with_state(AccessState::new().focusable(true))
                            .with_action_target(target),
                    );
                }
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-places"))
                    .with_description(tr("photos-a11y-places-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:places")
                    .with_children(cards)
            } else {
                cards.extend(
                    self.library_cards
                        .iter()
                        .filter_map(|(id, bounds, favorite)| {
                            let item = self.library_items.iter().find(|item| item.id == *id)?;
                            let name = &item.name;
                            let open_target = format!("photos:library-photo:{id}");
                            let mut children = vec![if self.selection_mode {
                                AccessNode::new(Role::ToggleButton)
                                    .with_name(name.clone())
                                    .with_description(tr("photos-a11y-selected-description"))
                                    .with_bounds(*bounds)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .checked(self.selected_photo_ids.contains(id)),
                                    )
                                    .with_action_target(open_target)
                            } else if self.album_editor {
                                AccessNode::new(Role::ToggleButton)
                                    .with_name(name.clone())
                                    .with_description(tr("photos-a11y-in-album"))
                                    .with_bounds(*bounds)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .checked(self.album_editor_members.contains(id)),
                                    )
                                    .with_action_target(open_target)
                            } else {
                                AccessNode::new(Role::Button)
                                    .with_name(name.clone())
                                    .with_description(tr("photos-open"))
                                    .with_bounds(*bounds)
                                    .with_state(AccessState::new().focusable(true))
                                    .with_action_target(open_target)
                            }];
                            if !self.album_editor && !self.selection_mode {
                                let actions = card_action_controls(*bounds, &self.layout_theme);
                                let favorite_target = format!("photos:library-favorite:{id}");
                                children.push(
                                    AccessNode::new(Role::ToggleButton)
                                        .with_name(tr("photos-favorite"))
                                        .with_bounds(actions.favorite)
                                        .with_state(
                                            AccessState::new().focusable(true).checked(*favorite),
                                        )
                                        .with_action_target(favorite_target),
                                );
                                let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                                children.push(
                                    AccessNode::new(Role::Button)
                                        .with_name(if confirmed {
                                            tr_with(
                                                "photos-a11y-confirm-remove-named",
                                                "name",
                                                name,
                                            )
                                        } else {
                                            tr_with("photos-a11y-remove-named", "name", name)
                                        })
                                        .with_description(tr("photos-a11y-remove-description"))
                                        .with_bounds(actions.remove)
                                        .with_state(AccessState::new().focusable(true))
                                        .with_action_target(format!("photos:library-remove:{id}")),
                                );
                                children.push(
                                    AccessNode::new(Role::Button)
                                        .with_name(tr_with(
                                            "photos-a11y-details-named",
                                            "name",
                                            name,
                                        ))
                                        .with_description(tr("photos-a11y-details-description"))
                                        .with_bounds(actions.details)
                                        .with_state(AccessState::new().focusable(true))
                                        .with_action_target(format!("photos:library-details:{id}")),
                                );
                            }
                            Some(
                                AccessNode::new(Role::ListItem)
                                    .with_name(name.clone())
                                    .with_bounds(*bounds)
                                    .with_hashed_key(format!("photos:library-item:{id}"))
                                    .with_children(children),
                            )
                        }),
                );
                cards.extend(self.album_cards.iter().map(|(id, bounds)| {
                    let label = id
                        .as_ref()
                        .and_then(|id| {
                            self.library_albums
                                .iter()
                                .find(|(album_id, _, _)| album_id == id)
                        })
                        .map_or_else(
                            || tr("photos-a11y-all-photos"),
                            |(_, name, count)| {
                                tr_args(
                                    "photos-people-card-name",
                                    &[
                                        ("name", ArgValue::Text(name.clone())),
                                        ("count", ArgValue::Number(*count as f64)),
                                    ],
                                )
                            },
                        );
                    let target = format!("photos:library-album:{}", id.as_deref().unwrap_or("all"));
                    AccessNode::new(Role::ToggleButton)
                        .with_name(label)
                        .with_bounds(*bounds)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(id.as_ref() == self.active_album.as_ref()),
                        )
                        .with_action_target(target)
                }));
                cards.extend(self.metadata_filter_cards.iter().map(|card| {
                    let label = if card.favorites {
                        tr("photos-favorites")
                    } else {
                        match (&card.folder, &card.person, &card.tag) {
                            (Some(folder), None, None) => {
                                tr_with("photos-a11y-filter-folder", "name", folder)
                            }
                            (None, Some(person), None) => {
                                tr_with("photos-a11y-filter-person", "name", person)
                            }
                            (None, None, Some(tag)) => {
                                tr_with("photos-a11y-filter-tag", "name", tag)
                            }
                            _ => tr("photos-a11y-all-metadata"),
                        }
                    };
                    AccessNode::new(Role::ToggleButton)
                        .with_name(label)
                        .with_bounds(card.bounds)
                        .with_state(AccessState::new().focusable(true).checked(
                            card.folder.as_ref() == self.active_folder.as_ref()
                                && card.person.as_ref() == self.active_person.as_ref()
                                && card.tag.as_ref() == self.active_tag.as_ref()
                                && card.favorites == self.active_favorites
                                && self.active_album.is_none(),
                        ))
                        .with_action_target(if card.favorites {
                            "photos:library-favorites".to_owned()
                        } else {
                            format!(
                                "photos:library-metadata:{}",
                                metadata_filter_id(
                                    card.folder.as_deref(),
                                    card.person.as_deref(),
                                    card.tag.as_deref()
                                )
                            )
                        })
                }));
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                for (forward, enabled, bounds) in &self.library_page_hits {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(if *forward {
                                tr("photos-a11y-next-photos")
                            } else {
                                tr("photos-a11y-previous-photos")
                            })
                            .with_description(self.library_page_label())
                            .with_bounds(*bounds)
                            .with_state(AccessState::new().focusable(*enabled).disabled(!*enabled))
                            .with_action_target(if *forward {
                                "photos:library-page:next"
                            } else {
                                "photos:library-page:previous"
                            }),
                    );
                }
                if self.selection_album_chooser {
                    // A modal needs one focus scope. Leaving the dimmed grid,
                    // filters, or card operations in the AT tree would let a
                    // screen-reader user activate controls a pointer user cannot.
                    cards.clear();
                }
                for (hit, bounds) in &self.selection_hits {
                    let (name, target, disabled) = match hit {
                        SelectionHit::PreviousDestinationPage => (
                            "photos-a11y-previous-albums",
                            "photos:selection:destination-previous",
                            self.selection_album_page == 0,
                        ),
                        SelectionHit::NextDestinationPage => (
                            "photos-a11y-more-albums",
                            "photos:selection:destination-next",
                            (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                                >= self.library_albums.len(),
                        ),
                        SelectionHit::Start => {
                            ("photos-a11y-select", "photos:selection:start", false)
                        }
                        SelectionHit::SelectAll => {
                            ("photos-a11y-select-all", "photos:selection:all", false)
                        }
                        SelectionHit::Clear => (
                            "photos-a11y-clear-selection",
                            "photos:selection:clear",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::Favorite => (
                            if self.selection_all_favorite {
                                "photos-a11y-unfavorite-selection"
                            } else {
                                "photos-a11y-favorite-selection"
                            },
                            "photos:selection:favorite",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::EditDetails => (
                            "photos-a11y-edit-selection",
                            "photos:selection:edit-details",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::ChooseAlbum => (
                            "photos-a11y-choose-album",
                            "photos:selection:choose-album",
                            self.selected_photo_ids.is_empty() || self.library_albums.is_empty(),
                        ),
                        SelectionHit::CancelDestinationChooser => (
                            "photos-a11y-cancel-choice",
                            "photos:selection:destination-cancel",
                            false,
                        ),
                        SelectionHit::CloseDestinationChooser => (
                            "photos-a11y-close-chooser",
                            "photos:selection:destination-close",
                            false,
                        ),
                        SelectionHit::ApplyDestination => (
                            "photos-a11y-use-album",
                            "photos:selection:destination-apply",
                            self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::AddToAlbum => (
                            "photos-a11y-add-selection",
                            "photos:selection:add-to-album",
                            self.selected_photo_ids.is_empty()
                                || self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::Done => (
                            "photos-a11y-finish-selection",
                            "photos:selection:done",
                            false,
                        ),
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(tr(name))
                            .with_bounds(*bounds)
                            .with_state(AccessState::new().focusable(!disabled).disabled(disabled))
                            .with_action_target(target),
                    );
                }
                for (album_id, bounds) in &self.selection_destination_hits {
                    let name = self
                        .library_albums
                        .iter()
                        .find(|(id, _, _)| id == album_id)
                        .map_or_else(
                            || tr("photos-a11y-choose-album"),
                            |(_, name, _)| tr_with("photos-a11y-use-named-album", "name", name),
                        );
                    cards.push(
                        AccessNode::new(Role::ToggleButton)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState::new().focusable(true).checked(
                                self.selection_destination_album.as_deref()
                                    == Some(album_id.as_str()),
                            ))
                            .with_action_target(format!("photos:selection:destination:{album_id}")),
                    );
                }
                if self.metadata_editor {
                    // The inspector owns the interaction scope. Keeping the
                    // dimmed grid's cards in the a11y tree would let a screen
                    // reader activate controls that pointer and keyboard users
                    // correctly cannot reach while editing details.
                    cards.clear();
                    let fields: &[(&str, &str, &TextInput)] = match self.metadata_editor_section {
                        MetadataEditorSection::Organize => &[
                            ("photos-folder", "folder", &self.metadata_folder_input),
                            ("photos-people", "people", &self.metadata_people_input),
                            ("photos-tags", "tags", &self.metadata_tags_input),
                        ],
                        MetadataEditorSection::TimeAndPlace => &[
                            (
                                "photos-captured",
                                "captured",
                                &self.metadata_captured_at_input,
                            ),
                            (
                                "photos-a11y-place-name",
                                "place",
                                &self.metadata_place_input,
                            ),
                            ("photos-latitude", "latitude", &self.metadata_latitude_input),
                            (
                                "photos-longitude",
                                "longitude",
                                &self.metadata_longitude_input,
                            ),
                        ],
                    };
                    for (name, target, input) in fields {
                        // The field's clear toggle, as the field draws it,
                        // routed to the same command an agent runs.
                        let clear = match *target {
                            "folder" => Some("clear-folder"),
                            "people" => Some("clear-people"),
                            "tags" => Some("clear-tags"),
                            "captured" => Some("clear-captured-at"),
                            "place" => Some("clear-location"),
                            _ => None,
                        };
                        let toggles: Vec<AccessNode> = clear
                            .and_then(|command| {
                                let toggle = ui::widget::Widget::accessibility(*input)?
                                    .children
                                    .into_iter()
                                    .next()?;
                                Some(vec![
                                    toggle.with_action_target(format!("photos:metadata:{command}")),
                                ])
                            })
                            .unwrap_or_default();
                        cards.push(
                            AccessNode::new(Role::TextInput)
                                .with_name(tr(name))
                                .with_value(input.value())
                                .with_bounds(input.get_rect())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(input.is_focused()),
                                )
                                .with_hashed_key(format!("photos:metadata:{target}"))
                                .with_action_target(format!("photos:metadata:field:{target}"))
                                .with_children(toggles),
                        );
                    }
                    for (hit, bounds) in &self.metadata_editor_hits {
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("photos-a11y-save-details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("photos-a11y-cancel-details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("photos-a11y-organize-fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => (
                                "photos-a11y-time-place-fields",
                                "photos:metadata:time-and-place",
                            ),
                            MetadataEditorHit::ClearFolder => {
                                ("photos-clear-folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("photos-clear-people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("photos-clear-tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("photos-clear-captured", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("photos-clear-location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => {
                                ("photos-a11y-place-from-map", "photos:metadata:place-on-map")
                            }
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(*bounds)
                                .with_state(AccessState::new().focusable(true))
                                .with_action_target(target),
                        );
                    }
                    for (hit, button) in &self.metadata_editor_buttons {
                        let bounds = button.get_rect();
                        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                            continue;
                        }
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("photos-a11y-save-details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("photos-a11y-cancel-details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("photos-a11y-organize-fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => (
                                "photos-a11y-time-place-fields",
                                "photos:metadata:time-and-place",
                            ),
                            MetadataEditorHit::ClearFolder => {
                                ("photos-clear-folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("photos-clear-people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("photos-clear-tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("photos-clear-captured", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("photos-clear-location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(bounds)
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .disabled(!button.is_enabled()),
                                )
                                .with_action_target(target),
                        );
                    }
                } else if self.album_editor {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name(tr("photos-a11y-album-name"))
                            .with_value(self.album_name_input.value())
                            .with_bounds(self.album_name_input.get_rect())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.album_name_input.is_focused()),
                            )
                            .with_hashed_key("photos:album-editor:name"),
                    );
                    for (hit, bounds) in &self.album_editor_hits {
                        let (name, target) = match hit {
                            AlbumEditorHit::Save => {
                                ("photos-a11y-save-album", "photos:album-editor:save")
                            }
                            AlbumEditorHit::Cancel => {
                                ("photos-a11y-cancel-album", "photos:album-editor:cancel")
                            }
                            AlbumEditorHit::Delete => {
                                ("photos-delete-album", "photos:album-editor:delete")
                            }
                            AlbumEditorHit::ConfirmDelete => (
                                "photos-a11y-confirm-delete-album",
                                "photos:album-editor:confirm-delete",
                            ),
                            AlbumEditorHit::New => continue,
                            AlbumEditorHit::Edit => {
                                ("photos-edit-album", "photos:album-editor:edit")
                            }
                            AlbumEditorHit::PreviousPage => (
                                "photos-a11y-previous-photos",
                                "photos:album-editor:previous",
                            ),
                            AlbumEditorHit::NextPage => {
                                ("photos-a11y-more-photos", "photos:album-editor:next")
                            }
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(*bounds)
                                .with_state(AccessState::new().focusable(true))
                                .with_action_target(target),
                        );
                    }
                } else {
                    for (hit, bounds) in &self.album_editor_hits {
                        let (name, target) = match hit {
                            AlbumEditorHit::New => ("photos-new-album", "photos:album-editor:new"),
                            _ => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(*bounds)
                                .with_state(AccessState::new().focusable(true))
                                .with_action_target(target),
                        );
                    }
                }
                if self.selection_album_chooser {
                    AccessNode::new(Role::Dialog)
                        .with_name(tr("photos-a11y-choose-album"))
                        .with_description(tr("photos-a11y-chooser-description"))
                        .with_bounds(self.canvas_rect)
                        .with_state(AccessState::new().modal(true))
                        .with_hashed_key("photos:destination-album-dialog")
                        .with_children(cards)
                } else {
                    if !self.album_editor && !self.metadata_editor {
                        if self.library_items.is_empty() {
                            // An empty library paints only its empty state:
                            // read that out, and publish no timeline index
                            // that is not on screen.
                            let (title, body) = self.library_empty_state_text();
                            cards.push(
                                AccessNode::new(Role::StaticText)
                                    .with_name(title)
                                    .with_description(body)
                                    .with_bounds(self.canvas_rect)
                                    .with_hashed_key("photos:library-empty"),
                            );
                        } else {
                            cards.push(self.timeline_index.accessibility(
                                self.timeline_index_metrics,
                                &self.timeline_index_markers,
                            ));
                        }
                    }
                    AccessNode::new(Role::List)
                        .with_name(tr("photos-library"))
                        .with_description(tr_count("photos-count-photos", self.library_total))
                        .with_bounds(self.canvas_rect)
                        .with_hashed_key("photos:library-grid")
                        .with_children(cards)
                }
            }
        } else {
            AccessNode::new(Role::Image)
                .with_name(image_name)
                .with_description(image_description)
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:canvas")
        };
        let mut children = vec![canvas];
        children.extend(
            self.empty_state
                .as_ref()
                .and_then(Widget::accessibility)
                .map(|node| node.with_hashed_key("photos:empty-state")),
        );
        children.extend(self.chrome_semantics());
        if let Some(notice) = self.notice.as_deref() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice)
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:notice"),
            );
        }
        if let Some(job) = self.directory_import.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if job.mode == DirectoryImportMode::Reindex {
                        tr("photos-reindex-running")
                    } else {
                        tr("photos-import-running")
                    })
                    .with_description(
                        self.directory_import_summary()
                            .map(|(_, summary, _, _)| summary)
                            .unwrap_or_default(),
                    )
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-progress"),
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-import-stop"))
                    .with_action_target("photos:directory-import:stop")
                    .with_hashed_key("photos:directory-import-stop"),
            ];
            if !job.failures.is_empty() {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-import-details"))
                        .with_description(tr_count(
                            "photos-a11y-inspect-issues",
                            job.failures.len(),
                        ))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-a11y-import-progress"))
                    .with_state(AccessState::new().busy(true))
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if receipt.mode == DirectoryImportMode::Reindex {
                        tr("photos-reindex-complete")
                    } else {
                        tr("photos-import-complete")
                    })
                    .with_description(
                        self.directory_import_summary()
                            .map(|(_, summary, _, _)| summary)
                            .unwrap_or_default(),
                    )
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-result"),
            ];
            if receipt.failure_count > 0 {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-import-details"))
                        .with_description(tr_count(
                            "photos-a11y-inspect-issues",
                            receipt.failures.len(),
                        ))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            status_children.push(
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-import-dismiss"))
                    .with_action_target("photos:directory-import:dismiss")
                    .with_hashed_key("photos:directory-import-dismiss"),
            );
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-a11y-import-result"))
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        }
        if self.directory_import_details_open {
            let (failures, total) = if let Some(job) = self.directory_import.as_ref() {
                (&job.failures[..], job.failures.len())
            } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
                (&receipt.failures[..], receipt.failure_count)
            } else {
                (&[][..], 0)
            };
            let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
            let mut detail_children = failures
                .iter()
                .take(retained)
                .enumerate()
                .map(|(index, failure)| {
                    let name = failure
                        .source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map_or_else(|| tr("photos-import-unnamed"), str::to_owned);
                    AccessNode::new(Role::ListItem)
                        .with_name(name)
                        .with_description(tr_args(
                            "photos-a11y-issue-description",
                            &[
                                ("message", ArgValue::Text(failure.message.clone())),
                                (
                                    "next",
                                    ArgValue::Text(if failure.retryable {
                                        tr("photos-import-can-retry")
                                    } else {
                                        tr("photos-import-needs-change")
                                    }),
                                ),
                            ],
                        ))
                        .with_hashed_key(format!("photos:directory-failure:{index}"))
                })
                .collect::<Vec<_>>();
            detail_children.push(
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-close"))
                    .with_state(AccessState::new().focusable(true))
                    .with_action_target("photos:directory-import:details-close")
                    .with_hashed_key("photos:directory-import:details-close"),
            );
            children.push(
                AccessNode::new(Role::Dialog)
                    .with_name(tr("photos-import-issues-title"))
                    .with_description(tr_args(
                        "photos-import-issues-total",
                        &[
                            ("kept", ArgValue::Number(retained as f64)),
                            ("count", ArgValue::Number(total as f64)),
                        ],
                    ))
                    .with_state(AccessState::new().modal(true))
                    .with_children(detail_children)
                    .with_hashed_key("photos:directory-import-dialog"),
            );
        }
        AccessNode::new(Role::Group)
            .with_name(tr("photos-app"))
            .with_bounds(self.rect)
            .with_semantic_key("photos")
            .with_children(children)
    }

    pub(super) fn perform_directory_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
    ) -> bool {
        let Some(command) = target.strip_prefix("photos:directory-import:") else {
            return false;
        };
        if matches!(action, AccessAction::Focus) {
            let status_hit = match command {
                "stop" => Some(DirectoryStatusHit::Stop),
                "details" => Some(DirectoryStatusHit::Details),
                "dismiss" => Some(DirectoryStatusHit::Dismiss),
                _ => None,
            };
            if let Some(hit) = status_hit {
                let available = match hit {
                    DirectoryStatusHit::Stop => self.directory_import.is_some(),
                    DirectoryStatusHit::Details => {
                        self.directory_import
                            .as_ref()
                            .is_some_and(|job| !job.failures.is_empty())
                            || self
                                .directory_import_receipt
                                .as_ref()
                                .is_some_and(|receipt| receipt.failure_count > 0)
                    }
                    DirectoryStatusHit::Dismiss => {
                        self.directory_import.is_none() && self.directory_import_receipt.is_some()
                    }
                };
                if available {
                    for (candidate, button) in &mut self.directory_import_buttons {
                        button.set_focus(*candidate == hit);
                    }
                }
                return available;
            }
            return self.directory_import_details_open && command == "details-close";
        }
        if !matches!(action, AccessAction::Activate) {
            return false;
        }
        match command {
            "stop" => self.activate_directory_status(DirectoryStatusHit::Stop),
            "details" => self.activate_directory_status(DirectoryStatusHit::Details),
            "dismiss" => self.activate_directory_status(DirectoryStatusHit::Dismiss),
            "details-close" => self.close_directory_details(),
            _ => false,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the library semantics mirror every visible card, editor, and metadata filter target"
    )]
    pub(super) fn perform_library_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        _doc: &AutoCommit,
    ) -> bool {
        if let Some(destination) = target.strip_prefix("photos:library:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let destination = match destination {
                "timeline" => LibraryDestination::Timeline,
                "albums" => LibraryDestination::Albums,
                "people" => LibraryDestination::People,
                "places" => LibraryDestination::Places,
                "analysis" => LibraryDestination::Analysis,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) {
                self.open_nav_target(&chrome::NavTarget::Destination(destination));
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:places:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            if let Some(photo_id) = command.strip_prefix("photo:") {
                if self.presented().photo(photo_id).cloned().is_none() {
                    return false;
                }
                self.show_photo(photo_id);
                return true;
            }
            match command {
                "toggle-list" => self.places_list_mode = !self.places_list_mode,
                "zoom-in" => self.zoom_places_by(1.0),
                "zoom-out" => self.zoom_places_by(-1.0),
                "pan-left" => self.set_places_viewport(self.places_viewport.panned(
                    44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-right" => self.set_places_viewport(self.places_viewport.panned(
                    -44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-up" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-down" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    -44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                _ => {
                    let Some(key) = command.strip_prefix("open:") else {
                        return false;
                    };
                    return self.open_place_collection(&self.presented(), key).is_ok();
                }
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:album-edit:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if self.presented().album(album_id).cloned().is_none() {
                    return false;
                }
                self.active_album = Some(album_id.to_owned());
                self.begin_edit_album();
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:collection-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let hit = match command {
                "albums-previous" => CollectionPageHit::AlbumsPrevious,
                "albums-next" => CollectionPageHit::AlbumsNext,
                "people-previous" => CollectionPageHit::PeoplePrevious,
                "people-next" => CollectionPageHit::PeopleNext,
                "places-previous" => CollectionPageHit::PlacesPrevious,
                "places-next" => CollectionPageHit::PlacesNext,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) {
                self.page_collection(hit);
            }
            return true;
        }
        if target == "photos:people:name" {
            return match action {
                AccessAction::Focus => {
                    self.people_name_input.set_focus(true);
                    true
                }
                AccessAction::SetValue(value) => {
                    self.people_name_input.set_text(value);
                    true
                }
                _ => false,
            };
        }
        if let Some(index) = target
            .strip_prefix("photos:people:control:")
            .and_then(|value| value.parse::<usize>().ok())
        {
            let Some(control) = self.people_controls.get(index) else {
                return false;
            };
            return match action {
                AccessAction::Focus => {
                    for item in &mut self.people_controls {
                        item.button.set_focus(false);
                    }
                    if let Some(control) = self.people_controls.get_mut(index) {
                        control.button.set_focus(true);
                    }
                    true
                }
                AccessAction::Activate if control.button.is_enabled() => {
                    let intent = control.intent.clone();
                    self.handle_people_intent(intent);
                    true
                }
                _ => false,
            };
        }
        if target.starts_with("photos:timeline-index:") {
            if matches!(action, AccessAction::Focus) {
                self.timeline_index.set_focus(true);
                return true;
            }
            if let Some(intent) = self.timeline_index.perform_access_action(
                target,
                action,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            ) {
                return self.apply_timeline_index_intent(&intent);
            }
            return false;
        }
        if let Some(direction) = target.strip_prefix("photos:timeline:density-") {
            return match (direction, action) {
                ("in", AccessAction::Activate) => self.adjust_timeline_density(true),
                ("out", AccessAction::Activate) => self.adjust_timeline_density(false),
                ("in" | "out", AccessAction::Focus) => true,
                _ => false,
            };
        }
        if let Some(command) = target.strip_prefix("photos:analysis:") {
            if let Some(photo_id) = command.strip_prefix("failure:") {
                if matches!(action, AccessAction::Focus) {
                    return self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id);
                }
                if !matches!(action, AccessAction::Activate)
                    || !self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id)
                {
                    return false;
                }
                self.show_photo(photo_id);
                return true;
            }
            let hit = match command {
                "start" => LibraryAnalysisHit::Start,
                "pause" => LibraryAnalysisHit::Pause,
                "resume" => LibraryAnalysisHit::Resume,
                "retry" => LibraryAnalysisHit::RetryFailures,
                _ => return false,
            };
            if matches!(action, AccessAction::Focus) {
                for (candidate, button) in &mut self.library_analysis_buttons {
                    button.set_focus(*candidate == hit);
                }
                return true;
            }
            return matches!(action, AccessAction::Activate) && self.activate_library_analysis(hit);
        }
        if let Some(command) = target.strip_prefix("photos:selection:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if let Some(album_id) = command.strip_prefix("destination:") {
                    if self.presented().album(album_id).cloned().is_none() {
                        self.notice = Some(tr("photos-album-gone"));
                        return false;
                    }
                    self.selection_destination_album = Some(album_id.to_owned());
                    self.notice = Some(tr("photos-selection-album-chosen"));
                    return true;
                }
                let hit = match command {
                    "start" => SelectionHit::Start,
                    "all" => SelectionHit::SelectAll,
                    "clear" => SelectionHit::Clear,
                    "favorite" => SelectionHit::Favorite,
                    "edit-details" => SelectionHit::EditDetails,
                    "choose-album" => SelectionHit::ChooseAlbum,
                    "destination-previous" => SelectionHit::PreviousDestinationPage,
                    "destination-next" => SelectionHit::NextDestinationPage,
                    "destination-cancel" => SelectionHit::CancelDestinationChooser,
                    "destination-close" => SelectionHit::CloseDestinationChooser,
                    "destination-apply" => SelectionHit::ApplyDestination,
                    "add-to-album" => SelectionHit::AddToAlbum,
                    "done" => SelectionHit::Done,
                    _ => return false,
                };
                self.handle_selection_hit(hit);
            }
            return true;
        }
        if let Some(direction) = target.strip_prefix("photos:library-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let forward = match direction {
                "next" => true,
                "previous" => false,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) && self.can_page_library(forward) {
                self.page_library(forward);
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:album-editor:") {
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "new" => self.begin_new_album(),
                "edit" => self.begin_edit_album(),
                "previous-page" if self.can_page_library(false) => self.page_library(false),
                "next-page" if self.can_page_library(true) => self.page_library(true),
                "save" => self.save_album_editor(),
                "cancel" => self.cancel_album_editor(),
                "delete" => self.album_delete_confirm = true,
                "confirm-delete" => self.delete_active_album(),
                _ => return false,
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:metadata:") {
            if let Some(field) = command.strip_prefix("field:") {
                // Organize and Time & Place fields share the same focus slots.
                let focus = match field {
                    "folder" | "captured" => 0,
                    "people" | "place" => 1,
                    "tags" | "latitude" => 2,
                    "longitude" => 3,
                    _ => return false,
                };
                return match action {
                    AccessAction::Focus => {
                        self.set_metadata_editor_focus(focus);
                        true
                    }
                    AccessAction::SetValue(value) => {
                        match field {
                            "folder" => self.metadata_folder_input.set_text(value),
                            "people" => self.metadata_people_input.set_text(value),
                            "tags" => self.metadata_tags_input.set_text(value),
                            "captured" => self.metadata_captured_at_input.set_text(value),
                            "place" => self.metadata_place_input.set_text(value),
                            "latitude" => {
                                self.metadata_latitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            "longitude" => {
                                self.metadata_longitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            _ => return false,
                        }
                        true
                    }
                    _ => false,
                };
            }
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "save" => self.save_metadata_editor(),
                "cancel" => self.cancel_metadata_editor(),
                "organize" => self.set_metadata_editor_section(MetadataEditorSection::Organize),
                "time-and-place" => {
                    self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
                }
                "clear-folder" => self.clear_metadata_dimension(0),
                "clear-people" => self.clear_metadata_dimension(1),
                "clear-tags" => self.clear_metadata_dimension(2),
                "clear-captured-at" => self.clear_metadata_dimension(3),
                "clear-location" => self.clear_metadata_dimension(4),
                "place-on-map" => {
                    let bounds = self.metadata_location_map_bounds;
                    let _ = self.set_metadata_location_from_map_point(bounds.center());
                }
                _ => return false,
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:library-album:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            self.active_album = (album_id != "all").then(|| album_id.to_owned());
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            self.timeline_scroll_y = 0.0;
            return true;
        }
        if let Some(id) = target.strip_prefix("photos:library-details:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                self.begin_metadata_editor(id);
            }
            return true;
        }
        if target == "photos:library-favorites" {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            // The sidebar row and the filter card share this route; both open
            // Favorites as a click does, empty or not, so a reader hears the
            // same "No favorites yet" a sighted user sees.
            if matches!(action, AccessAction::Activate) {
                self.open_nav_target(&chrome::NavTarget::Favorites);
            }
            return true;
        }
        if let Some(filter_id) = target.strip_prefix("photos:library-metadata:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            let Some(card) = self.metadata_filter_cards.iter().find(|card| {
                metadata_filter_id(
                    card.folder.as_deref(),
                    card.person.as_deref(),
                    card.tag.as_deref(),
                ) == filter_id
            }) else {
                return false;
            };
            self.active_album = None;
            self.active_folder.clone_from(&card.folder);
            self.active_person.clone_from(&card.person);
            self.active_tag.clone_from(&card.tag);
            self.active_favorites = card.favorites;
            self.library_offset = 0;
            return true;
        }
        let (photo_id, set_favorite, remove) =
            if let Some(id) = target.strip_prefix("photos:library-photo:") {
                (id, false, false)
            } else if let Some(id) = target.strip_prefix("photos:library-favorite:") {
                (id, true, false)
            } else if let Some(id) = target.strip_prefix("photos:library-remove:") {
                (id, false, true)
            } else {
                return false;
            };
        if self.selection_album_chooser {
            return false;
        }
        if !matches!(action, AccessAction::Activate) {
            if matches!(action, AccessAction::Focus) && self.selection_mode {
                self.selection_anchor = Some(photo_id.to_owned());
            }
            return matches!(action, AccessAction::Focus);
        }
        let Some(item) = self.library_items.iter().find(|item| item.id == photo_id) else {
            return false;
        };
        let favorite = item.favorite;
        if self.selection_mode {
            self.toggle_library_selection(photo_id, false);
        } else if remove {
            if self.library_remove_confirm.as_deref() == Some(photo_id) {
                self.commit(PhotoLibrary::REMOVE_PHOTO.with(Id::from_raw(photo_id)));
                self.library_remove_confirm = None;
                self.notice = Some(tr("photos-removed"));
            } else {
                self.library_remove_confirm = Some(photo_id.to_owned());
                self.notice = Some(tr("photos-remove-confirm"));
            }
        } else if set_favorite {
            self.commit(crate::model::photo_paths(photo_id).favorite.set(!favorite));
        } else if self.album_editor {
            if !self.album_editor_members.insert(photo_id.to_owned()) {
                self.album_editor_members.remove(photo_id);
            }
        } else {
            self.show_photo(photo_id);
        }
        true
    }
}

fn collection_page_access_node(hit: CollectionPageHit, bounds: Rect, enabled: bool) -> AccessNode {
    let (name, target) = match hit {
        CollectionPageHit::AlbumsPrevious => ("photos-a11y-previous-albums", "albums-previous"),
        CollectionPageHit::AlbumsNext => ("photos-a11y-more-albums", "albums-next"),
        CollectionPageHit::PeoplePrevious => ("photos-a11y-previous-people", "people-previous"),
        CollectionPageHit::PeopleNext => ("photos-a11y-more-people", "people-next"),
        CollectionPageHit::PlacesPrevious => ("photos-a11y-previous-places", "places-previous"),
        CollectionPageHit::PlacesNext => ("photos-a11y-more-places", "places-next"),
    };
    AccessNode::new(Role::Button)
        .with_name(tr(name))
        .with_description(tr("photos-a11y-page-hint"))
        .with_bounds(bounds)
        .with_state(AccessState::new().focusable(enabled).disabled(!enabled))
        .with_action_target(format!("photos:collection-page:{target}"))
}
