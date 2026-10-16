//! The album and metadata editors: opening them over the library, their
//! input, saving their drafts and painting their sheets.

use crate::actions::PhotosEffect;
use crate::model::PhotoLibrary;
use crate::strings::{tr, tr_count};
use std::cell::Cell;
use std::rc::Rc;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Id;
use ui::widget::{Button, ButtonFace, ButtonRole, ScrollMetrics, TextInput, Widget};
use ui::widget::{SegmentedFace, ToggleGroup};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use winit::keyboard::{Key, NamedKey};

use super::paint::OFFLINE_WORLD_LANDMASSES;
use super::{
    AlbumEditorHit, LibraryDestination, MetadataEditorHit, MetadataEditorSection, PhotosApp, bound,
};

impl PhotosApp {
    /// Pose the ordinary library editor for deterministic offscreen coverage.
    /// The production entry point is the visible “Edit album” control; shot
    /// rendering has no window event stream to click it through.
    pub fn show_album_editor_for_shot(&mut self) {
        self.show_library();
        self.active_album = self
            .presented()
            .albums
            .values()
            .next()
            .map(|album| album.id.clone());
        self.begin_edit_album();
    }

    /// Pose the shared metadata editor for visual regression coverage.
    pub fn show_metadata_editor_for_shot(&mut self) {
        self.show_library();
        let library = self.presented();
        if let Some(id) = library
            .photos
            .values()
            .find(|photo| !photo.metadata_summary().is_empty())
            .or_else(|| library.photos.values().next())
            .map(|photo| photo.id.clone())
        {
            self.begin_metadata_editor(&id);
        }
    }

    /// Pose a mixed-value multi-photo edit on its location section. This is
    /// the state most likely to regress silently because blank means “keep”
    /// until the user explicitly clears or replaces the dimension.
    pub fn show_batch_metadata_editor_for_shot(&mut self) {
        self.show_selection_for_shot();
        self.begin_selected_metadata_editor();
        self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
    }

    pub(super) fn begin_new_album(&mut self) {
        // A local `untitled-1` counter collides when two offline replicas
        // create their first album. Mint the durable identity independently
        // of its editable name so concurrent creations remain separate.
        let id = format!("photos.album:{}", tilo_actions::InvocationId::new().0);
        self.album_editor = true;
        self.album_editor_creating = true;
        self.album_editor_id = Some(id);
        self.album_name_input.set_text(&tr("photos-untitled-album"));
        self.album_name_input.set_focus(true);
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-new-hint"));
    }

    /// Open the compact, shared-widget metadata sheet for an explicit card.
    /// Nothing is inferred from pixels: people are only labels the user types.
    pub(super) fn begin_metadata_editor(&mut self, photo_id: &str) {
        self.begin_metadata_editor_for_ids(vec![photo_id.to_owned()]);
    }

    pub(super) fn begin_selected_metadata_editor(&mut self) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        self.begin_metadata_editor_for_ids(ids);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "seeds every metadata field, mixed-value flag and draft input from one library snapshot"
    )]
    pub(super) fn begin_metadata_editor_for_ids(&mut self, photo_ids: Vec<String>) {
        let library = self.presented();
        if photo_ids.is_empty()
            || photo_ids.len() > crate::resource::MAX_BATCH_PHOTOS
            || !photo_ids.iter().all(|id| library.photo(id).is_some())
        {
            self.notice = Some(tr("photos-error-selected-gone"));
            return;
        }
        let photos: Vec<_> = photo_ids
            .iter()
            .filter_map(|id| library.photo(id))
            .collect();
        let common = |values: Vec<String>| {
            values
                .first()
                .filter(|first| values.iter().all(|value| value == *first))
                .cloned()
        };
        let folders: Vec<_> = photos
            .iter()
            .map(|photo| photo.folder.clone().unwrap_or_default())
            .collect();
        let people: Vec<_> = photos
            .iter()
            .map(|photo| photo.people().join(", "))
            .collect();
        let tags: Vec<_> = photos.iter().map(|photo| photo.tags().join(", ")).collect();
        let captured: Vec<_> = photos
            .iter()
            .map(|photo| photo.captured_at.clone().unwrap_or_default())
            .collect();
        let locations: Vec<_> = photos.iter().map(|photo| photo.location.clone()).collect();
        let common_folder = common(folders);
        let common_people = common(people);
        let common_tags = common(tags);
        let common_captured = common(captured);
        let common_location = locations
            .first()
            .filter(|first| locations.iter().all(|value| value == *first))
            .cloned();

        self.metadata_editor = true;
        self.metadata_editor_ids = photo_ids;
        self.metadata_mixed = [
            common_folder.is_none(),
            common_people.is_none(),
            common_tags.is_none(),
            common_captured.is_none(),
            common_location.is_none(),
        ];
        self.metadata_force_clear = [false; 5];
        self.metadata_folder_input
            .set_text(common_folder.as_deref().unwrap_or_default());
        self.metadata_people_input
            .set_text(common_people.as_deref().unwrap_or_default());
        self.metadata_tags_input
            .set_text(common_tags.as_deref().unwrap_or_default());
        self.metadata_captured_at_input
            .set_text(common_captured.as_deref().unwrap_or_default());
        if let Some(Some(location)) = common_location {
            self.metadata_place_input
                .set_text(location.label.as_deref().unwrap_or_default());
            self.metadata_latitude_input
                .set_text(&format!("{:.6}", location.latitude));
            self.metadata_longitude_input
                .set_text(&format!("{:.6}", location.longitude));
            self.metadata_location_map.latitude = location.latitude;
            self.metadata_location_map.longitude = location.longitude;
            self.metadata_location_map.zoom = 4.0;
            self.metadata_location_from_map = location.provenance.source == "map";
        } else {
            self.metadata_place_input.set_text("");
            self.metadata_latitude_input.set_text("");
            self.metadata_longitude_input.set_text("");
            self.metadata_location_map = crate::places::MapViewport {
                zoom: crate::places::MIN_ZOOM,
                ..crate::places::MapViewport::default()
            };
            self.metadata_location_from_map = false;
        }
        // A field's placeholder says what to type, never its label again;
        // one that differs across the chosen photos says it is kept as is.
        let mixed_hint = tr("photos-metadata-mixed");
        for (input, mixed, ordinary) in [
            (
                &mut self.metadata_folder_input,
                self.metadata_mixed[0],
                String::new(),
            ),
            (
                &mut self.metadata_people_input,
                self.metadata_mixed[1],
                tr("photos-metadata-people-placeholder"),
            ),
            (
                &mut self.metadata_tags_input,
                self.metadata_mixed[2],
                tr("photos-metadata-tags-placeholder"),
            ),
            (
                &mut self.metadata_captured_at_input,
                self.metadata_mixed[3],
                tr("photos-metadata-captured-placeholder"),
            ),
            (
                &mut self.metadata_latitude_input,
                self.metadata_mixed[4],
                String::new(),
            ),
            (
                &mut self.metadata_longitude_input,
                self.metadata_mixed[4],
                String::new(),
            ),
        ] {
            input.set_placeholder(if mixed { &mixed_hint } else { &ordinary });
        }
        self.metadata_folder_input.set_focus(true);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_section = MetadataEditorSection::Organize;
        self.metadata_editor_focus = 0;
        self.metadata_editor_scroll = 0.0;
        self.metadata_editor_reveal_focus = true;
        self.album_editor = false;
        // The sheet owns the full privacy explanation. Keeping a duplicate
        // in the persistent status band made it wrap below a phone viewport.
        self.notice = None;
    }

    pub(super) fn cancel_metadata_editor(&mut self) {
        self.metadata_editor = false;
        self.metadata_editor_ids.clear();
        self.metadata_folder_input.set_focus(false);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_focus = 0;
        self.notice = Some(tr("photos-details-discarded"));
    }

    #[expect(
        clippy::too_many_lines,
        reason = "builds the single atomic batch metadata action from all sheet fields in one place"
    )]
    pub(super) fn save_metadata_editor(&mut self) {
        if self.metadata_editor_ids.is_empty() {
            return;
        }
        let optional = |input: &TextInput| {
            let value = input.value().trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let text_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::model::MetadataPatch::Clear
            } else if mixed && input.value().trim().is_empty() {
                crate::model::MetadataPatch::Keep
            } else {
                optional(input).map_or(
                    crate::model::MetadataPatch::Clear,
                    crate::model::MetadataPatch::Set,
                )
            }
        };
        let labels_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::model::MetadataLabelsPatch::Clear
            } else {
                let raw = input.value().trim();
                let (mode, raw) = match raw.strip_prefix('=') {
                    Some(value) => ("set", value),
                    None => match raw.strip_prefix('+') {
                        Some(value) => ("add", value),
                        None => match raw.strip_prefix('-') {
                            Some(value) => ("remove", value),
                            None if self.metadata_editor_ids.len() > 1 && !raw.is_empty() => {
                                ("add", raw)
                            }
                            None => ("set", raw),
                        },
                    },
                };
                let values = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if mixed && values.is_empty() {
                    crate::model::MetadataLabelsPatch::Keep
                } else {
                    match mode {
                        "add" => crate::model::MetadataLabelsPatch::Add(values),
                        "remove" => crate::model::MetadataLabelsPatch::Remove(values),
                        _ if values.is_empty() => crate::model::MetadataLabelsPatch::Clear,
                        _ => crate::model::MetadataLabelsPatch::Set(values),
                    }
                }
            }
        };
        let latitude = self.metadata_latitude_input.value().trim();
        let longitude = self.metadata_longitude_input.value().trim();
        let location = if self.metadata_force_clear[4] {
            crate::model::MetadataPatch::Clear
        } else if self.metadata_mixed[4] && latitude.is_empty() && longitude.is_empty() {
            crate::model::MetadataPatch::Keep
        } else if latitude.is_empty() && longitude.is_empty() {
            crate::model::MetadataPatch::Clear
        } else {
            let (Ok(latitude), Ok(longitude)) = (latitude.parse::<f64>(), longitude.parse::<f64>())
            else {
                self.notice = Some(tr("photos-location-invalid"));
                return;
            };
            crate::model::MetadataPatch::Set(crate::model::PhotoLocation {
                latitude,
                longitude,
                label: optional(&self.metadata_place_input),
                provenance: crate::model::LocationProvenance {
                    source: if self.metadata_location_from_map {
                        "map".to_owned()
                    } else {
                        "user".to_owned()
                    },
                    scope: "shared".to_owned(),
                },
            })
        };
        let batch = crate::model::MetadataBatch {
            folder: text_patch(
                &self.metadata_folder_input,
                self.metadata_mixed[0],
                self.metadata_force_clear[0],
            ),
            people: labels_patch(
                &self.metadata_people_input,
                self.metadata_mixed[1],
                self.metadata_force_clear[1],
            ),
            tags: labels_patch(
                &self.metadata_tags_input,
                self.metadata_mixed[2],
                self.metadata_force_clear[2],
            ),
            captured_at: text_patch(
                &self.metadata_captured_at_input,
                self.metadata_mixed[3],
                self.metadata_force_clear[3],
            ),
            location,
        };
        match self
            .presented()
            .metadata_edits(&self.metadata_editor_ids, &batch)
        {
            Ok(edits) => self.commit_group(&edits),
            Err(refusal) => {
                self.notice = Some(refusal.0);
                return;
            }
        }
        let count = self.metadata_editor_ids.len();
        self.metadata_editor = false;
        self.metadata_editor_ids.clear();
        self.metadata_editor_focus = 0;
        self.notice = Some(if count == 1 {
            tr("photos-details-saved")
        } else {
            tr_count("photos-details-saved-many", count)
        });
        if count > 1 {
            self.finish_selection();
        }
    }

    pub(super) fn set_metadata_editor_focus(&mut self, focus: usize) {
        let max = if self.metadata_editor_section == MetadataEditorSection::Organize {
            4
        } else {
            5
        };
        self.metadata_editor_focus = focus.min(max);
        self.metadata_editor_reveal_focus = true;
        let organize = self.metadata_editor_section == MetadataEditorSection::Organize;
        self.metadata_folder_input.set_focus(organize && focus == 0);
        self.metadata_people_input.set_focus(organize && focus == 1);
        self.metadata_tags_input.set_focus(organize && focus == 2);
        self.metadata_captured_at_input
            .set_focus(!organize && focus == 0);
        self.metadata_place_input.set_focus(!organize && focus == 1);
        self.metadata_latitude_input
            .set_focus(!organize && focus == 2);
        self.metadata_longitude_input
            .set_focus(!organize && focus == 3);
    }

    pub(super) fn set_metadata_editor_section(&mut self, section: MetadataEditorSection) {
        self.metadata_editor_section = section;
        self.metadata_editor_scroll = 0.0;
        self.set_metadata_editor_focus(0);
    }

    pub(super) fn clear_metadata_dimension(&mut self, dimension: usize) {
        let Some(force_clear) = self.metadata_force_clear.get_mut(dimension) else {
            return;
        };
        *force_clear = !*force_clear;
        if !*force_clear {
            return;
        }
        match dimension {
            0 => self.metadata_folder_input.set_text(""),
            1 => self.metadata_people_input.set_text(""),
            2 => self.metadata_tags_input.set_text(""),
            3 => self.metadata_captured_at_input.set_text(""),
            4 => {
                self.metadata_place_input.set_text("");
                self.metadata_latitude_input.set_text("");
                self.metadata_longitude_input.set_text("");
                self.metadata_location_from_map = false;
            }
            _ => {}
        }
    }

    /// Write an explicit offline-map point into the draft fields. No document
    /// update occurs here: batch edits retain their mixed values until the
    /// user deliberately saves this one shared provenance-bearing patch.
    pub(super) fn set_metadata_location_from_map_point(&mut self, point: Point) -> bool {
        let bounds = self.metadata_location_map_bounds;
        if !bounds.contains(point) {
            return false;
        }
        let Some((latitude, longitude)) = self.metadata_location_map.unproject(
            point.x - bounds.x0,
            point.y - bounds.y0,
            bounds.width(),
            bounds.height(),
        ) else {
            return false;
        };
        self.metadata_latitude_input
            .set_text(&format!("{latitude:.6}"));
        self.metadata_longitude_input
            .set_text(&format!("{longitude:.6}"));
        // Re-centre the private draft map so its persistent crosshair is an
        // honest confirmation of the point just chosen.
        self.metadata_location_map.latitude = latitude;
        self.metadata_location_map.longitude = longitude;
        self.metadata_mixed[4] = false;
        self.metadata_force_clear[4] = false;
        self.metadata_location_from_map = true;
        self.notice = Some(tr("photos-location-picked"));
        true
    }

    pub(super) fn activate_metadata_editor_hit(&mut self, hit: MetadataEditorHit) {
        match hit {
            MetadataEditorHit::Save => self.save_metadata_editor(),
            MetadataEditorHit::Cancel => self.cancel_metadata_editor(),
            MetadataEditorHit::Organize => {
                self.set_metadata_editor_section(MetadataEditorSection::Organize);
            }
            MetadataEditorHit::TimeAndPlace => {
                self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
            }
            MetadataEditorHit::ClearFolder => self.clear_metadata_dimension(0),
            MetadataEditorHit::ClearPeople => self.clear_metadata_dimension(1),
            MetadataEditorHit::ClearTags => self.clear_metadata_dimension(2),
            MetadataEditorHit::ClearCapturedAt => self.clear_metadata_dimension(3),
            MetadataEditorHit::ClearLocation => self.clear_metadata_dimension(4),
            MetadataEditorHit::PlaceOnMap => {
                let bounds = self.metadata_location_map_bounds;
                let _ = self.set_metadata_location_from_map_point(bounds.center());
            }
        }
    }

    pub(super) fn begin_edit_album(&mut self) {
        let Some(id) = self.active_album.clone() else {
            self.notice = Some(tr("photos-album-choose"));
            return;
        };
        let Some(album) = self.presented().album(&id).cloned() else {
            self.notice = Some("That album is no longer available".to_owned());
            self.active_album = None;
            return;
        };
        self.album_editor = true;
        self.album_editor_creating = false;
        self.album_editor_id = Some(album.id);
        self.album_name_input.set_text(&album.name);
        self.album_name_input.set_focus(true);
        self.album_editor_members = album.photo_ids.iter().cloned().collect();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
    }

    pub(super) fn cancel_album_editor(&mut self) {
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-discarded"));
    }

    pub(super) fn save_album_editor(&mut self) {
        let Some(id) = self.album_editor_id.clone() else {
            return;
        };
        let name = self.album_name_input.value().trim().to_owned();
        let library = self.presented();
        let stale = if self.album_editor_creating {
            library.album(&id).is_some()
        } else {
            library.album(&id).is_none()
        };
        if stale
            || name.is_empty()
            || !self
                .album_editor_members
                .iter()
                .all(|photo| library.photo(photo).is_some())
        {
            self.notice = Some(if stale {
                tr("photos-album-conflict")
            } else {
                tr("photos-album-invalid")
            });
            return;
        }
        let photo_ids: ui::tilo_resource::Set<String> = library
            .all_photos()
            .filter(|photo| self.album_editor_members.contains(&photo.id))
            .map(|photo| photo.id.clone())
            .collect();
        let album = crate::model::Album {
            id: id.clone(),
            name,
            photo_ids,
        };
        if self.album_editor_creating {
            self.commit(
                PhotoLibrary::paths()
                    .albums
                    .insert_as(&album.item(), usize::MAX, &album),
            );
        } else {
            // A name and a membership saved together, so collaborators never
            // see half of the save.
            let paths = crate::model::album_paths(&id);
            let before = library
                .album(&id)
                .map(|album| album.photo_ids.clone())
                .unwrap_or_default();
            let mut edits = vec![paths.name.replace(album.name)];
            edits.extend(paths.photo_ids.edits(&before, &album.photo_ids));
            self.commit_group(&edits);
        }
        self.active_album = Some(id);
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-saved"));
    }

    pub(super) fn delete_active_album(&mut self) {
        let Some(id) = self
            .album_editor_id
            .clone()
            .or_else(|| self.active_album.clone())
        else {
            return;
        };
        self.commit(PhotoLibrary::REMOVE_ALBUM.with(Id::from_raw(id)));
        self.active_album = None;
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-deleted"));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "embedded TextInput uses the Widget event contract; keyboard and pointer handling stay beside the album-sheet controls"
    )]
    pub(super) fn handle_album_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.album_editor {
            return false;
        }
        // The timeline beside the editor keeps scrolling: choosing photos
        // means finding them.
        if matches!(event, winit::event::WindowEvent::MouseWheel { .. }) {
            return false;
        }
        let mut sink = None;
        self.album_name_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                focus_target: &mut sink,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::PageDown) => {
                    self.scroll_timeline_rows(3.0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::PageUp) => {
                    self.scroll_timeline_rows(-3.0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) => {
                    let steps = if self.album_editor_creating { 3 } else { 4 };
                    self.album_editor_focus = if ctx.modifiers.state().shift_key() {
                        (self.album_editor_focus + steps - 1) % steps
                    } else {
                        (self.album_editor_focus + 1) % steps
                    };
                    self.album_name_input
                        .set_focus(self.album_editor_focus == 0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_album_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_album_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    match (self.album_editor_creating, self.album_editor_focus) {
                        (_, 1) => self.cancel_album_editor(),
                        (false, 2) if self.album_delete_confirm => {
                            self.delete_active_album();
                        }
                        (false, 2) => self.album_delete_confirm = true,
                        (true, 2) | (false, 3) => self.save_album_editor(),
                        _ => {}
                    }
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
            if self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
                && matches!(self.metadata_editor_focus, 2 | 3)
            {
                // Coordinates subsequently authored through the shared text
                // input are no longer accurately described as a map pin.
                self.metadata_location_from_map = false;
            }
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
            && self.set_metadata_location_from_map_point(self.pointer)
        {
            *ctx.handled = true;
            return true;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            if let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
            {
                match hit {
                    AlbumEditorHit::Save => self.save_album_editor(),
                    AlbumEditorHit::Cancel => self.cancel_album_editor(),
                    AlbumEditorHit::Delete => {
                        self.album_delete_confirm = true;
                        self.notice = Some(tr("photos-album-delete-confirm"));
                    }
                    AlbumEditorHit::ConfirmDelete => self.delete_active_album(),
                    _ => {}
                }
                *ctx.handled = true;
                return true;
            }
            if let Some((id, _, _)) = self
                .library_cards
                .iter()
                .find(|(_, rect, _)| rect.contains(self.pointer))
                .cloned()
            {
                if !self.album_editor_members.insert(id.clone()) {
                    self.album_editor_members.remove(&id);
                }
                self.album_delete_confirm = false;
                *ctx.handled = true;
                return true;
            }
        }
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keyboard, pointer and embedded TextInput routing for the metadata sheet stay together under the Widget event contract"
    )]
    pub(super) fn handle_metadata_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.metadata_editor {
            return false;
        }
        // The sheet floats over a custom media surface, so dispatch its
        // retained controls explicitly. They still use the normal widget
        // event contract and publish the same action surface as every other
        // button in the app.
        for (_, button) in &mut self.metadata_editor_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
            return true;
        }
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            let focus_count = if self.metadata_editor_section == MetadataEditorSection::Organize {
                5
            } else {
                6
            };
            match &key.logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.set_metadata_editor_focus(if ctx.modifiers.state().shift_key() {
                        (self.metadata_editor_focus + focus_count - 1) % focus_count
                    } else {
                        (self.metadata_editor_focus + 1) % focus_count
                    });
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 2 => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 1 => {
                    self.save_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }
        // A wheel over the sheet scrolls its field list (clamped at paint,
        // where the content height is known).
        if let winit::event::WindowEvent::MouseWheel { delta, .. } = event
            && self.metadata_editor_body.contains(self.pointer)
        {
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 40.0,
                // Physical pixels, like every pointer position this app
                // converts; without the division a 2× display scrolls twice
                // the trackpad distance.
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    position.y / ctx.scale_factor
                }
            };
            self.metadata_editor_scroll = (self.metadata_editor_scroll - dy).max(0.0);
            *ctx.handled = true;
            return true;
        }
        // A press outside the field list's visible region belongs to the
        // pinned footer (or the scrim), never to an input scrolled under it.
        let press_outside_body = matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) && !self.metadata_editor_body.contains(self.pointer);
        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            if press_outside_body {
                break;
            }
            let mut sink = None;
            input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        // A field's clear toggle answers in the same event.
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
            return true;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && let Some((hit, _)) = self
            .metadata_editor_hits
            .iter()
            .find(|(_, rect)| rect.contains(self.pointer))
            .copied()
        {
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
        }
        true
    }

    /// The album editor in the inspector: the album's name, how many photos
    /// are in it (chosen in the timeline beside it), and its actions.
    #[expect(
        clippy::too_many_lines,
        reason = "the editor lays out its field, count and footer actions in one pass"
    )]
    pub(super) fn draw_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.library_mode || !self.album_editor {
            if self.library_destination != LibraryDestination::Albums {
                self.album_editor_hits.clear();
            }
            self.album_name_input.apply_layout_external(Rect::ZERO);
            return;
        }
        self.album_editor_hits.clear();
        let title = if self.album_editor_creating {
            tr("photos-new-album")
        } else {
            tr("photos-edit-album")
        };
        let Some((frame, local)) =
            self.paint_inspector(scene, transform, text_renderer, theme, &title)
        else {
            self.album_name_input.apply_layout_external(Rect::ZERO);
            return;
        };
        self.album_editor_rect = frame.panel;
        let rhythm = &local.tokens.spacing.rhythm;
        let control_h = ui::widget::ControlSize::Regular.height(&local);
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &local);
        let body = frame.body;
        let mut y = body.y0;
        ctx.text_role(
            &tr("photos-people-name"),
            ui::TextRole::Label,
            local.text_dim,
            Point::new(body.x0, y),
            None,
        );
        y += Self::line_of(&local, ui::TextRole::Label) + f64::from(rhythm.tight);
        self.album_name_input
            .apply_layout_external(Rect::new(body.x0, y, body.x1, y + control_h));
        self.album_name_input
            .draw(ctx.scene, transform, ctx.text, &local, presence);
        y += control_h + f64::from(rhythm.group);
        let guidance = tr("photos-album-editor-hint");
        let guidance_h = ctx
            .measure_role(&guidance, ui::TextRole::Body, Some(body.width() as f32))
            .height;
        ctx.text_role(
            &guidance,
            ui::TextRole::Body,
            local.text_dim,
            Point::new(body.x0, y),
            Some(body.width() as f32),
        );
        y += guidance_h + f64::from(rhythm.related);
        ctx.text_role(
            &tr_count("photos-album-members", self.album_editor_members.len()),
            ui::TextRole::Caption,
            local.text_dim,
            Point::new(body.x0, y),
            None,
        );

        // Footer: delete leading, cancel and save trailing.
        let footer = frame.footer;
        let gap = f64::from(rhythm.related);
        let save_label = tr("photos-save");
        let cancel_label = tr("photos-cancel");
        let save_w = ButtonFace::new(Rect::ZERO, &save_label).intrinsic_width(ctx.text, &local);
        let cancel_w = ButtonFace::new(Rect::ZERO, &cancel_label).intrinsic_width(ctx.text, &local);
        let save = Rect::new(footer.x1 - save_w, footer.y0, footer.x1, footer.y1);
        let cancel = Rect::new(
            save.x0 - gap - cancel_w,
            footer.y0,
            save.x0 - gap,
            footer.y1,
        );
        let mut buttons = vec![
            (
                AlbumEditorHit::Cancel,
                cancel,
                cancel_label,
                ButtonRole::Secondary,
                self.album_editor_focus == 1,
            ),
            (
                AlbumEditorHit::Save,
                save,
                save_label,
                ButtonRole::Primary,
                self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 },
            ),
        ];
        if !self.album_editor_creating {
            let (hit, delete_label) = if self.album_delete_confirm {
                (
                    AlbumEditorHit::ConfirmDelete,
                    tr("photos-confirm-delete-album"),
                )
            } else {
                (AlbumEditorHit::Delete, tr("photos-delete-album"))
            };
            let width =
                ButtonFace::new(Rect::ZERO, &delete_label).intrinsic_width(ctx.text, &local);
            let x1 = (footer.x0 + width).min(cancel.x0 - gap);
            buttons.push((
                hit,
                Rect::new(footer.x0, footer.y0, x1, footer.y1),
                delete_label,
                ButtonRole::Destructive,
                self.album_editor_focus == 2,
            ));
        }
        for (hit, rect, label, role, focused) in &buttons {
            let face = ButtonFace::new(*rect, label).role(*role).focused(*focused);
            let hovered = face.contains(&local, self.pointer);
            Button::paint(
                ctx.scene,
                transform,
                ctx.text,
                &local,
                &face.hovered(hovered),
            );
            self.album_editor_hits.push((*hit, *rect));
        }
    }

    /// The details editor in the inspector: the photographs' folder, people
    /// and tags, or when and where they were taken, and Save.
    #[expect(
        clippy::too_many_lines,
        reason = "the two sections share one scroll region, field rhythm and footer"
    )]
    pub(super) fn draw_metadata_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            input.apply_layout_external(Rect::ZERO);
        }
        for (_, button) in &mut self.metadata_editor_buttons {
            button.apply_layout_external(Rect::ZERO);
        }
        if !self.metadata_editor {
            self.metadata_editor_hits.clear();
            return;
        }
        let count = self.metadata_editor_ids.len();
        let title = if count == 1 {
            tr("photos-photo-details")
        } else {
            tr_count("photos-edit-count", count)
        };
        let Some((frame, local)) =
            self.paint_inspector(scene, transform, text_renderer, theme, &title)
        else {
            self.metadata_editor_hits.clear();
            return;
        };
        let rhythm = &local.tokens.spacing.rhythm;
        let (tight, related, sibling) = (
            f64::from(rhythm.tight),
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
        );
        let control_h = ui::widget::ControlSize::Regular.height(&local);
        let label_h = Self::line_of(&local, ui::TextRole::Label)
            .max(ui::widget::ControlSize::Small.height(&local));
        let body = frame.body;
        let mut hits = Vec::new();

        // The two sections.
        let strip_h = SegmentedFace::height(ui::widget::ControlSize::Regular, &local);
        let strip = Rect::new(body.x0, body.y0, body.x1, body.y0 + strip_h);
        let labels = [tr("photos-organize"), tr("photos-time-and-place")];
        let label_refs = [labels[0].as_str(), labels[1].as_str()];
        let segments = SegmentedFace::partition(
            strip,
            &label_refs,
            ui::widget::ControlSize::Regular,
            text_renderer,
            &local,
        );
        let selected =
            usize::from(self.metadata_editor_section == MetadataEditorSection::TimeAndPlace);
        let hovered = segments
            .iter()
            .position(|segment| segment.contains(self.pointer));
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            &local,
            &SegmentedFace::new(&segments, &label_refs, selected).hovered(hovered),
        );
        hits.extend(
            [MetadataEditorHit::Organize, MetadataEditorHit::TimeAndPlace]
                .into_iter()
                .zip(segments.iter().copied()),
        );

        let fields_top = strip.y1 + sibling;
        let region = Rect::new(body.x0, fields_top, body.x1, body.y1.max(fields_top));
        self.metadata_editor_body = region;
        let row_h = label_h + tight + control_h + sibling;
        // The map gives up height first, so in a short window the
        // coordinates under it stay in view; it never shrinks below a
        // target the pointer can still pick from.
        let preferred_map =
            (region.width() * 0.5).min(f64::from(local.tokens.metrics.panel_width_utility) * 0.6);
        let least_map = 2.0 * f64::from(local.tokens.metrics.min_pointer_target);
        let map_h = preferred_map
            .min(region.height() - 3.0 * row_h - sibling)
            .max(least_map.min(preferred_map));
        let time_and_place = self.metadata_editor_section == MetadataEditorSection::TimeAndPlace;
        let content_h = if time_and_place {
            3.0 * row_h + map_h + sibling
        } else {
            3.0 * row_h
        };
        if std::mem::take(&mut self.metadata_editor_reveal_focus) {
            let focused_row_top = match (time_and_place, self.metadata_editor_focus) {
                (false, focus @ 0..=2) | (true, focus @ 0..=1) => Some(focus as f64 * row_h),
                (true, 2 | 3) => Some(2.0 * row_h + map_h + sibling),
                _ => None,
            };
            if let Some(top) = focused_row_top {
                let bottom = top + row_h - sibling;
                if top < self.metadata_editor_scroll {
                    self.metadata_editor_scroll = top;
                } else if bottom > self.metadata_editor_scroll + region.height() {
                    self.metadata_editor_scroll = bottom - region.height();
                }
            }
        }
        self.metadata_editor_scroll = self
            .metadata_editor_scroll
            .clamp(0.0, (content_h - region.height()).max(0.0));
        let fields_y = fields_top - self.metadata_editor_scroll;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &region,
        );
        // One field: its label, and the input under it. A field a batch edit
        // can clear for every photo carries that as a toggle in its trailing
        // accessory; `clear` is whether it is on.
        let field = |scene: &mut Scene,
                     text_renderer: &mut TextRenderer,
                     input: &mut TextInput,
                     label: &str,
                     clear: Option<bool>,
                     rect: Rect| {
            PaintCtx::new(scene, transform, text_renderer, &local).text_role_in_band(
                label,
                ui::TextRole::Label,
                local.text_dim,
                rect.x0,
                Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + label_h),
                None,
            );
            if let Some(forced) = clear {
                input.set_accessory_pressed(forced);
            }
            input.apply_layout_external(Rect::new(
                rect.x0,
                rect.y0 + label_h + tight,
                rect.x1,
                rect.y0 + label_h + tight + control_h,
            ));
            input.draw(scene, transform, text_renderer, &local, presence);
        };

        let column = |index: f64| {
            let y = fields_y + index * row_h;
            Rect::new(region.x0, y, region.x1, y + row_h - sibling)
        };
        match self.metadata_editor_section {
            MetadataEditorSection::Organize => {
                let force = self.metadata_force_clear;
                for (index, (label, input)) in [
                    (tr("photos-folder"), &mut self.metadata_folder_input),
                    (tr("photos-people"), &mut self.metadata_people_input),
                    (tr("photos-tags"), &mut self.metadata_tags_input),
                ]
                .into_iter()
                .enumerate()
                {
                    let forced = force.get(index).copied().unwrap_or(false);
                    field(
                        scene,
                        text_renderer,
                        input,
                        &label,
                        Some(forced),
                        column(index as f64),
                    );
                }
            }
            MetadataEditorSection::TimeAndPlace => {
                field(
                    scene,
                    text_renderer,
                    &mut self.metadata_captured_at_input,
                    &tr("photos-captured"),
                    Some(self.metadata_force_clear[3]),
                    column(0.0),
                );
                field(
                    scene,
                    text_renderer,
                    &mut self.metadata_place_input,
                    &tr("photos-place"),
                    Some(self.metadata_force_clear[4]),
                    column(1.0),
                );
                // A pick surface for the location: the offline map, with the
                // drafted position marked.
                let map_y = fields_y + 2.0 * row_h;
                let map = Rect::new(region.x0, map_y, region.x1, map_y + map_h);
                self.metadata_location_map_bounds = map;
                self.paint_location_map(scene, transform, &local, map);
                hits.push((MetadataEditorHit::PlaceOnMap, map.intersect(region)));
                let y = map.y1 + sibling;
                let half = (region.width() - related) / 2.0;
                for (label, input, x) in [
                    (
                        tr("photos-latitude"),
                        &mut self.metadata_latitude_input,
                        region.x0,
                    ),
                    (
                        tr("photos-longitude"),
                        &mut self.metadata_longitude_input,
                        region.x0 + half + related,
                    ),
                ] {
                    field(
                        scene,
                        text_renderer,
                        input,
                        &label,
                        None,
                        Rect::new(x, y, x + half, y + row_h - sibling),
                    );
                }
            }
        }
        scene.pop_layer();
        let metrics = ScrollMetrics::new(content_h, region.height(), self.metadata_editor_scroll);
        if metrics.overflows() {
            let gutter = f64::from(local.tokens.metrics.scrollbar_gutter);
            let track = Rect::new(region.x1, region.y0, region.x1 + gutter, region.y1);
            ui::widget::scrollbar::Scrollbar::vertical()
                .draw(scene, transform, track, metrics, &local, false);
        }

        // Footer: cancel and save, trailing.
        let footer = frame.footer;
        let footer_focus = if self.metadata_editor_section == MetadataEditorSection::Organize {
            (3, 4)
        } else {
            (4, 5)
        };
        let mut x1 = footer.x1;
        for (hit, focused) in [
            (
                MetadataEditorHit::Save,
                self.metadata_editor_focus == footer_focus.1,
            ),
            (
                MetadataEditorHit::Cancel,
                self.metadata_editor_focus == footer_focus.0,
            ),
        ] {
            let label = if hit == MetadataEditorHit::Save {
                tr("photos-save-changes")
            } else {
                tr("photos-cancel")
            };
            let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(text_renderer, &local);
            let rect = Rect::new(x1 - width, footer.y0, x1, footer.y1);
            x1 = rect.x0 - related;
            if let Some((_, button)) = self
                .metadata_editor_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_focus(focused);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, &local, presence);
            }
        }
        self.metadata_editor_hits = hits;
    }

    /// The location pick surface: the offline land silhouettes on the
    /// raised well, and the drafted position as a ring in primary ink.
    fn paint_location_map(&self, scene: &mut Scene, transform: Affine, theme: &Theme, map: Rect) {
        use vello::peniko::Fill;
        let colors = &theme.tokens.colors;
        let water = colors.surfaces.raised;
        let land = colors.lift_on(water, 1.0);
        let radius = f64::from(theme.tokens.radii.control);
        let shape = RoundedRect::from_rect(map, radius);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        scene.fill(Fill::NonZero, transform, water, None, &map);
        let viewport = self.metadata_location_map;
        for landmass in OFFLINE_WORLD_LANDMASSES {
            let mut coast = BezPath::new();
            let mut points = 0_usize;
            for &(latitude, longitude) in *landmass {
                let location = crate::model::PhotoLocation {
                    latitude,
                    longitude,
                    label: None,
                    provenance: crate::model::LocationProvenance::default(),
                };
                let Some((x, y)) = viewport.project(&location, map.width(), map.height()) else {
                    continue;
                };
                if points == 0 {
                    coast.move_to((map.x0 + x, map.y0 + y));
                } else {
                    coast.line_to((map.x0 + x, map.y0 + y));
                }
                points += 1;
            }
            if points >= 3 {
                coast.close_path();
                scene.fill(Fill::NonZero, transform, land, None, &coast);
            }
        }
        let pin = self
            .metadata_latitude_input
            .value()
            .trim()
            .parse::<f64>()
            .ok()
            .zip(
                self.metadata_longitude_input
                    .value()
                    .trim()
                    .parse::<f64>()
                    .ok(),
            )
            .and_then(|(latitude, longitude)| {
                viewport.project(
                    &crate::model::PhotoLocation {
                        latitude,
                        longitude,
                        label: None,
                        provenance: crate::model::LocationProvenance::default(),
                    },
                    map.width(),
                    map.height(),
                )
            });
        if let Some((x, y)) = pin {
            scene.stroke(
                &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0),
                transform,
                theme.text_primary,
                None,
                &Circle::new(
                    Point::new(map.x0 + x, map.y0 + y),
                    f64::from(theme.tokens.spacing.s2),
                ),
            );
        }
        scene.pop_layer();
        scene.stroke(
            &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
            transform,
            surface::edge_on(water),
            None,
            &surface::edge_outline(map, radius),
        );
    }
}

/// Keep compact card chrome single-line.  Character iteration, rather than a
/// byte slice, preserves Unicode names in both painting and accessibility.
/// Native controls layered over the custom metadata inspector. The inspector
/// owns responsive geometry; `Button` owns interaction states, focus, action
/// publication, tooltips, and the accessible name. This keeps the sheet from
/// becoming a painted collection of fake controls merely because it is docked
/// over the photo canvas.
pub(super) fn build_metadata_editor_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(MetadataEditorHit, Button)> {
    [
        (MetadataEditorHit::Organize, "organize", "photos-organize"),
        (
            MetadataEditorHit::TimeAndPlace,
            "time-place",
            "photos-time-and-place",
        ),
        (
            MetadataEditorHit::PlaceOnMap,
            "place-on-map",
            "photos-place-on-map",
        ),
        (MetadataEditorHit::Cancel, "cancel", "photos-cancel"),
        (MetadataEditorHit::Save, "save", "photos-save-changes"),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let button = Button::new(&format!("photos-metadata-{id}"), label)
            .tooltip(label)
            .with_accessible_name(label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

/// A metadata field whose trailing accessory toggles "clear this for every
/// photo in the batch": a field's own control, not a separate button.
pub(super) fn clear_toggled_input(
    id: &str,
    clear: MetadataEditorHit,
    pending: &Rc<Cell<Option<MetadataEditorHit>>>,
) -> TextInput {
    let name = tr(match clear {
        MetadataEditorHit::ClearFolder => "photos-clear-folder",
        MetadataEditorHit::ClearPeople => "photos-clear-people",
        MetadataEditorHit::ClearTags => "photos-clear-tags",
        MetadataEditorHit::ClearCapturedAt => "photos-clear-captured",
        _ => "photos-clear-location",
    });
    let slot = Rc::clone(pending);
    TextInput::local().with_id(id).with_trailing_accessory(
        Icon::X,
        &name,
        ui::widget::AccessoryAction::Toggle {
            pressed: false,
            on_toggle: Box::new(move |_| slot.set(Some(clear))),
        },
    )
}

/// The effect each retained control invokes, and back: a button is bound to
/// its effect through the framework, and the effect's handler posts the
/// control's intent for the next sync, which has the document.
impl MetadataEditorHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::Save => PhotosEffect::SaveDetails,
            Self::Cancel => PhotosEffect::CancelDetails,
            Self::Organize => PhotosEffect::ShowDetailsOrganize,
            Self::TimeAndPlace => PhotosEffect::ShowDetailsTimeAndPlace,
            Self::ClearFolder => PhotosEffect::ClearDraftFolder,
            Self::ClearPeople => PhotosEffect::ClearDraftPeople,
            Self::ClearTags => PhotosEffect::ClearDraftTags,
            Self::ClearCapturedAt => PhotosEffect::ClearDraftCapturedAt,
            Self::ClearLocation => PhotosEffect::ClearDraftLocation,
            Self::PlaceOnMap => PhotosEffect::PlaceDraftOnMap,
        }
    }
}
