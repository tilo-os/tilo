//! What the window shows and how to move through it: the library, its
//! timeline and pages, albums, places, the viewer, and the multi-photo
//! selection.

use crate::actions::PhotosEffect;
use crate::strings::{tr, tr_args, tr_with};
use crate::timeline_projection::TimelinePhoto;
use ui::ArgValue;
use ui::widget::{Button, DateIndexIntent, DateIndexedScrollbar, Widget};
use vello::kurbo::Vec2;

use super::{
    COLLECTION_PAGE_SIZE, CollectionPageHit, LIBRARY_PAGE_SIZE, LibraryDestination, PhotosApp,
    SELECTION_ALBUMS_PER_PAGE, SLIDE_SHOW_DWELL, SelectionHit, bound, capture_day,
};

impl PhotosApp {
    /// Drop the open photograph's viewer. The layout tree points at it, so
    /// dropping one that existed asks for a fresh tree.
    pub(super) fn close_viewer(&mut self) {
        if self.viewer.take().is_some() {
            self.structure_dirty = true;
        }
    }

    /// Reveal the bounded library grid.  Kept public for the deterministic
    /// offscreen gallery; ordinary interaction reaches it through the same
    /// toolbar button and semantic action as a user.
    pub fn show_library(&mut self) {
        self.library_mode = true;
        self.people_mode = false;
        self.library_destination = LibraryDestination::Timeline;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.timeline_scroll_y = 0.0;
        self.library_remove_confirm = None;
        self.finish_selection();
    }

    /// Resolve a canonical timeline resource without treating a visual card as
    /// the source of truth. The shared capture value is the only input: a
    /// removed or changed boundary refuses instead of jumping to a nearby,
    /// misleading date.
    pub(super) fn view_timeline(
        &mut self,
        library: &crate::model::PhotoLibrary,
        anchor: Option<&crate::actions::TimelineAnchor>,
    ) -> Result<(), String> {
        self.show_library();
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let mut photos: Vec<_> = library
            .photos
            .values()
            .cloned()
            .map(|photo| {
                let displayed_day = photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day);
                let canonical_day = photo
                    .captured_at
                    .as_deref()
                    .and_then(crate::derived::timeline_day_from_capture);
                (displayed_day, canonical_day, photo.id)
            })
            .collect();
        photos.sort_by(|(left_day, _, left_id), (right_day, _, right_id)| {
            let left_undated = left_day == "Undated";
            let right_undated = right_day == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right_day.cmp(left_day))
                .then_with(|| left_id.cmp(right_id))
        });
        let target = photos.iter().find(|(_, canonical_day, _)| match anchor {
            crate::actions::TimelineAnchor::Month { month } => canonical_day
                .as_deref()
                .is_some_and(|day| day.starts_with(month)),
            crate::actions::TimelineAnchor::Day { day: wanted } => {
                canonical_day.as_deref() == Some(wanted)
            }
        });
        let Some((target_day, _, _)) = target else {
            return Err(tr("photos-error-day-gone"));
        };
        // The previous projection may describe a filtered collection or an
        // older library snapshot. Rebuild against the timeline being opened
        // before resolving its offset, even if that day was already present.
        // This also handles actions arriving before the first painted frame.
        self.timeline_projection
            .replace(library.photos.values().map(|photo| {
                TimelinePhoto {
                    id: photo.id.clone(),
                    name: photo.image.name.clone(),
                    favorite: photo.favorite,
                    day: photo
                        .captured_at
                        .as_deref()
                        .map_or_else(|| "Undated".to_owned(), capture_day),
                    captured_at: photo.captured_at.clone().unwrap_or_default(),
                    aspect_ratio: if photo.image.width > 0 && photo.image.height > 0 {
                        f64::from(photo.image.width) / f64::from(photo.image.height)
                    } else {
                        1.5
                    },
                }
            }));
        let geometry = self.timeline_geometry(&self.layout_theme.clone());
        self.timeline_projection.set_geometry(geometry);
        self.timeline_scroll_y = self
            .timeline_projection
            .day_offset(target_day, self.timeline_viewport_height)
            .unwrap_or(0.0);
        Ok(())
    }

    /// Reveal the People collection without enabling recognition or changing
    /// any document metadata. This is used by the resource action and visual
    /// acceptance scenarios.
    pub fn show_people(&mut self) {
        self.show_library();
        self.people_mode = true;
        self.library_destination = LibraryDestination::People;
    }

    /// Reveal albums as a first-class collection. Opening the collection is
    /// presentation-only; opening a specific album still resolves its stable
    /// resource id and then enters the chronological photo view.
    pub fn show_albums(&mut self) {
        self.show_library();
        self.people_mode = false;
        self.library_destination = LibraryDestination::Albums;
    }

    /// Reveal the whole-library local-analysis dashboard for deterministic
    /// visual acceptance. No scan is started and no result is fabricated.
    pub fn show_library_analysis(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Analysis;
        self.library_analysis_capability = Self::discover_library_analysis_capability().ok();
    }

    pub(super) fn begin_selection(&mut self) {
        self.selection_mode = true;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
        self.selection_all_favorite = false;
        self.notice = Some(tr("photos-selection-hint"));
    }

    pub(super) fn finish_selection(&mut self) {
        self.selection_mode = false;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
        self.selection_all_favorite = false;
    }

    /// Toggle one materialised card, or the inclusive visible range from the
    /// previous anchor. A range is a view operation, so it is intentionally
    /// bounded to the cards the person can currently inspect.
    pub(super) fn toggle_library_selection(&mut self, photo_id: &str, extend: bool) {
        if !self.selection_mode {
            return;
        }
        let visible: Vec<_> = self.library_cards.iter().map(|(id, _, _)| id).collect();
        if extend
            && let Some(anchor) = &self.selection_anchor
            && let (Some(start), Some(end)) = (
                visible.iter().position(|id| **id == *anchor),
                visible.iter().position(|id| **id == photo_id),
            )
        {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            if let Some(range) = visible.get(start..=end) {
                for id in range {
                    self.selected_photo_ids.insert((**id).clone());
                }
            }
            return;
        }
        if !self.selected_photo_ids.insert(photo_id.to_owned()) {
            self.selected_photo_ids.remove(photo_id);
        }
        self.selection_anchor = Some(photo_id.to_owned());
    }

    pub(super) fn select_all_visible(&mut self) {
        self.selected_photo_ids
            .extend(self.library_cards.iter().map(|(id, _, _)| id.clone()));
    }

    pub(super) fn set_selected_favorite(&mut self, favorite: bool) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        let edits: Vec<_> = ids
            .iter()
            .map(|id| crate::model::photo_paths(id).favorite.set(favorite))
            .collect();
        self.commit_group(&edits);
        self.notice = Some(if favorite {
            tr("photos-selection-favorited")
        } else {
            tr("photos-selection-unfavorited")
        });
        self.finish_selection();
    }

    pub(super) fn add_selected_to_destination_album(&mut self) {
        let Some(album_id) = self.selection_destination_album.clone() else {
            self.notice = Some(tr("photos-selection-choose-album"));
            return;
        };
        let library = self.presented();
        let Some(album) = library.album(&album_id) else {
            self.notice = Some(tr("photos-album-gone"));
            return;
        };
        let mut selected: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        selected.sort_unstable();
        if selected.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        // Each photograph filed on its own, so a collaborator filing others
        // meanwhile keeps theirs.
        let paths = crate::model::album_paths(&album_id).photo_ids;
        let edits: Vec<_> = selected
            .iter()
            .filter(|id| !album.photo_ids.contains(id))
            .map(|id| paths.insert(id))
            .collect();
        self.commit_group(&edits);
        self.say_when_kept("Selected photographs added to the album".to_owned());
        self.finish_selection();
    }

    pub(super) fn handle_selection_hit(&mut self, hit: SelectionHit) {
        match hit {
            SelectionHit::Start => self.begin_selection(),
            SelectionHit::SelectAll => self.select_all_visible(),
            SelectionHit::Clear => {
                self.selected_photo_ids.clear();
                self.selection_anchor = None;
            }
            SelectionHit::Favorite => {
                let library = self.presented();
                let all_favorite = !self.selected_photo_ids.is_empty()
                    && self
                        .selected_photo_ids
                        .iter()
                        .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
                self.set_selected_favorite(!all_favorite);
            }
            SelectionHit::EditDetails => self.begin_selected_metadata_editor(),
            SelectionHit::ChooseAlbum => {
                if self.selected_photo_ids.is_empty() || self.library_albums.is_empty() {
                    self.notice = Some(tr("photos-selection-need-album"));
                } else {
                    self.selection_album_chooser = true;
                    self.selection_album_scroll.set_offset(Vec2::ZERO);
                }
            }
            SelectionHit::CancelDestinationChooser | SelectionHit::CloseDestinationChooser => {
                self.selection_album_chooser = false;
            }
            SelectionHit::ApplyDestination => {
                if self.selection_destination_album.is_some() {
                    self.selection_album_chooser = false;
                } else {
                    self.notice = Some(tr("photos-selection-choose-album"));
                }
            }
            SelectionHit::PreviousDestinationPage => {
                self.selection_album_page = self.selection_album_page.saturating_sub(1);
            }
            SelectionHit::NextDestinationPage => {
                let page_count = self
                    .library_albums
                    .len()
                    .div_ceil(SELECTION_ALBUMS_PER_PAGE);
                self.selection_album_page =
                    (self.selection_album_page + 1).min(page_count.saturating_sub(1));
            }
            SelectionHit::AddToAlbum => self.add_selected_to_destination_album(),
            SelectionHit::Done => self.finish_selection(),
        }
    }

    /// Pose the offline Places destination for deterministic visual coverage.
    pub fn show_places_for_shot(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = None;
        self.places_fit_pending = true;
    }

    /// Apply a local map-camera change and give the marker layer a restrained
    /// settle cue. The viewport is presentation state, so the animation never
    /// delays a semantic action or writes shared metadata.
    pub(super) fn set_places_viewport(&mut self, viewport: crate::places::MapViewport) {
        let viewport = viewport.clamped();
        if self.places_viewport == viewport {
            return;
        }
        self.places_viewport = viewport;
        if !self.places_motion_reduced {
            // A settle cue: on at once, gone over the feedback's fade.
            let motion = ui::tokens::EffectiveMotion::default();
            let now = ui::time::now();
            let mut feedback = ui::motion::Feedback::default();
            feedback.set(true, motion, now);
            feedback.set(false, motion, now);
            self.places_camera_feedback = Some(feedback);
        }
    }

    pub(super) fn zoom_places_by(&mut self, delta: f64) {
        self.set_places_viewport(crate::places::MapViewport {
            zoom: self.places_viewport.zoom + delta,
            ..self.places_viewport
        });
    }

    pub(super) fn places_camera_feedback(&mut self) -> f64 {
        let Some(feedback) = self.places_camera_feedback else {
            return 0.0;
        };
        let now = ui::time::now();
        if feedback.is_moving(now) {
            f64::from(feedback.level(now))
        } else {
            self.places_camera_feedback = None;
            0.0
        }
    }

    /// Open the exact durable collection behind a Places resource or marker.
    /// This reads only shared metadata, so it remains usable offline and
    /// deliberately fails if a collaborator removed the addressed place.
    pub(super) fn open_place_collection(
        &mut self,
        library: &crate::model::PhotoLibrary,
        place_key: &str,
    ) -> Result<(), String> {
        let photos: Vec<_> = library
            .photos
            .values()
            .filter(|photo| {
                photo.location.as_ref().is_some_and(|location| {
                    location.is_valid() && location.place_key() == place_key
                })
            })
            .cloned()
            .collect();
        let Some(location) = photos.first().and_then(|photo| photo.location.as_ref()) else {
            return Err(tr("photos-error-place-gone"));
        };
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = Some(place_key.to_owned());
        self.places_list_mode = true;
        self.places_page = 0;
        self.set_places_viewport(crate::places::MapViewport {
            latitude: location.latitude,
            longitude: location.longitude,
            zoom: 12.0,
        });
        Ok(())
    }

    /// Pose the ordinary selection shelf for deterministic visual coverage.
    /// Two real photographs are selected so the capture exercises the active
    /// controls and selected-card treatment, not merely their disabled state.
    pub fn show_selection_for_shot(&mut self) {
        self.show_library();
        self.begin_selection();
        for photo in self.presented().all_photos().take(2) {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
    }

    /// Pose a deliberately long destination list. The selection is local;
    /// this exists only to make the scrolling list visible in the
    /// deterministic accessibility and responsive screenshots.
    pub fn show_selection_destination_chooser_for_shot(&mut self) {
        if let Some(photo) = self.presented().photos.values().next() {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
        self.selection_album_chooser = true;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
    }

    /// Show photograph `photo_id` in this window.
    pub(super) fn show_photo(&mut self, photo_id: &str) {
        self.open_photo = Some(photo_id.to_owned());
        self.library_mode = false;
    }

    /// The photographs this window steps through, in the chronology's order:
    /// the same order the timeline shows them in.
    pub(super) fn photo_order(&self) -> Vec<String> {
        let ids = self.timeline_projection.ordered_ids();
        if !ids.is_empty() {
            return ids;
        }
        let mut ids: Vec<String> = self
            .presented()
            .all_photos()
            .map(|photo| photo.id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Move to the photograph `forward` of the open one, wrapping at either
    /// end so a slide show never stops at the last picture. Returns whether
    /// it moved.
    pub(super) fn step_photo(&mut self, forward: bool) -> bool {
        let order = self.photo_order();
        if order.is_empty() {
            return false;
        }
        let current = self
            .open_photo
            .as_deref()
            .and_then(|open| order.iter().position(|id| id == open));
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % order.len(),
            (Some(index), false) => index.checked_sub(1).unwrap_or(order.len() - 1),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        let Some(id) = order.get(next) else {
            return false;
        };
        if self.open_photo.as_deref() == Some(id.as_str()) && !self.library_mode {
            return false;
        }
        self.show_photo(id);
        self.restart_slide_show();
        true
    }

    /// Zoom the open photograph one step, as a window's Zoom in, Zoom out and
    /// Actual size reach it: the picture's own size, never the window's text
    /// scale, because the photograph is what is being looked at. Answers the
    /// factor shown.
    pub(super) fn zoom_photo(&mut self, step: ui::content_zoom::ZoomStep) -> f32 {
        let factor = self.viewer_zoom.step(step);
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.set_zoom(factor);
        }
        // The window's bar says the level, as it does for any content zoom.
        self.notice = Some(tr_with(
            "photos-zoom-level",
            "percent",
            &ui::format::percent(f64::from(factor)),
        ));
        factor
    }

    /// Show the open photograph full screen, or stop.
    pub(super) fn toggle_presentation(&mut self) {
        if self.library_mode || self.viewer.is_none() {
            return;
        }
        self.presenting = !self.presenting;
        if !self.presenting {
            self.slide_show = None;
        }
    }

    /// Run the slide show (advancing through the collection, full screen), or
    /// stop it where it is.
    pub(super) fn toggle_slide_show(&mut self) {
        if self.library_mode || self.viewer.is_none() {
            return;
        }
        if self.slide_show.take().is_some() {
            return;
        }
        // A slide show is a presentation: the chrome goes and the picture has
        // the screen, which is what showing photographs to someone means.
        self.presenting = true;
        self.slide_show = Some(ui::time::now() + SLIDE_SHOW_DWELL);
    }

    /// Whether the slide show is running.
    pub(super) fn slide_show_running(&self) -> bool {
        self.slide_show.is_some()
    }

    /// Start the interval of the running slide show again: each photograph is
    /// shown for the same while.
    pub(super) fn restart_slide_show(&mut self) {
        if self.slide_show.is_some() {
            self.slide_show = Some(ui::time::now() + SLIDE_SHOW_DWELL);
        }
    }

    /// Advance the slide show if its interval is up. Returns whether the
    /// window must repaint.
    pub(super) fn tick_slide_show(&mut self) -> bool {
        let Some(at) = self.slide_show else {
            return false;
        };
        if ui::time::now() < at {
            return false;
        }
        let order = self.photo_order();
        if order.len() <= 1 {
            // Nothing to advance to: a one-photograph library is not a slide
            // show.
            self.slide_show = None;
            return true;
        }
        self.step_photo(true);
        self.slide_show = Some(ui::time::now() + SLIDE_SHOW_DWELL);
        true
    }

    /// Move through the bounded library shelf without changing any shared
    /// state.  Album editing keeps its unsaved membership locally, so paging
    /// must stay available while the sheet is open: otherwise a phone-sized
    /// sheet can leave most of a large library impossible to include.
    pub(super) fn library_page_step(&self) -> usize {
        if self.layout_theme.text_scale > 1.25 && self.canvas_rect.width() < 620.0 {
            if self.album_editor { 2 } else { 1 }
        } else {
            LIBRARY_PAGE_SIZE
        }
    }

    pub(super) fn page_library(&mut self, forward: bool) {
        let step = self.library_page_step();
        if forward {
            self.library_offset =
                (self.library_offset + step).min(self.library_total.saturating_sub(1));
        } else {
            self.library_offset = self.library_offset.saturating_sub(step);
        }
        let viewport = self.timeline_viewport_height.max(1.0);
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + if forward { viewport } else { -viewport },
            viewport,
        );
    }

    pub(super) fn can_page_library(&self, forward: bool) -> bool {
        let step = self.library_page_step();
        if forward {
            self.library_offset.saturating_add(step) < self.library_total
        } else {
            self.library_offset > 0
        }
    }

    pub(super) fn page_collection(&mut self, hit: CollectionPageHit) -> bool {
        let place_total = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .count();
        let page_size = COLLECTION_PAGE_SIZE;
        let (page, total, forward) = match hit {
            CollectionPageHit::AlbumsPrevious => {
                (&mut self.albums_page, self.library_albums.len(), false)
            }
            CollectionPageHit::AlbumsNext => {
                (&mut self.albums_page, self.library_albums.len(), true)
            }
            CollectionPageHit::PeoplePrevious => {
                (&mut self.people_page, self.library_people.len(), false)
            }
            CollectionPageHit::PeopleNext => {
                (&mut self.people_page, self.library_people.len(), true)
            }
            CollectionPageHit::PlacesPrevious => (&mut self.places_page, place_total, false),
            CollectionPageHit::PlacesNext => (&mut self.places_page, place_total, true),
        };
        let pages = total.max(1).div_ceil(page_size);
        let next = if forward {
            page.saturating_add(1).min(pages.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if next == *page {
            return false;
        }
        *page = next;
        if matches!(
            hit,
            CollectionPageHit::AlbumsPrevious | CollectionPageHit::AlbumsNext
        ) {
            let step = self.albums_viewport.height();
            if step > 0.0 {
                self.albums_scroll =
                    (self.albums_scroll + if forward { step } else { -step }).max(0.0);
            }
        }
        true
    }

    pub(super) fn library_page_label(&self) -> String {
        let first = (self.library_offset + 1).min(self.library_total);
        let last = (self.library_offset + self.library_page_step()).min(self.library_total);
        tr_args(
            "photos-timeline-showing",
            &[
                ("first", ArgValue::Number(first as f64)),
                ("last", ArgValue::Number(last as f64)),
                ("total", ArgValue::Number(self.library_total as f64)),
            ],
        )
    }

    /// Change the local timeline density through any input source.  One step
    /// is shared by the +/- affordances, Ctrl-wheel and pinch, which makes
    /// their resulting layout testable for parity.
    pub(super) fn adjust_timeline_density(&mut self, increase: bool) -> bool {
        let before = self.timeline_density;
        if increase {
            self.timeline_density = self.timeline_density.saturating_add(1).min(4);
        } else {
            self.timeline_density = self.timeline_density.saturating_sub(1);
        }
        if before == self.timeline_density {
            return false;
        }
        true
    }

    pub(super) fn scroll_timeline_rows(&mut self, delta: f64) {
        // Keyboard rows remain a useful semantic step, but the canonical
        // offset is pixels so portrait and justified rows scroll smoothly.
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta * 168.0,
            self.timeline_viewport_height,
        );
    }

    pub(super) fn scroll_timeline_pixels(&mut self, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta,
            self.timeline_viewport_height,
        );
    }

    /// Apply every chronological-index input through the same local scroll
    /// and density helpers used by the ordinary timeline.  The index never
    /// owns a second offset, which is what keeps a pinch anchor and a dragged
    /// date thumb in agreement.
    pub(super) fn apply_timeline_index_intent(&mut self, intent: &DateIndexIntent) -> bool {
        match *intent {
            DateIndexIntent::ScrollTo { position } => {
                self.timeline_scroll_y =
                    DateIndexedScrollbar::offset_for(self.timeline_index_metrics, position);
                true
            }
            DateIndexIntent::AdjustDensity { delta } => match delta.cmp(&0) {
                std::cmp::Ordering::Greater => self.adjust_timeline_density(true),
                std::cmp::Ordering::Less => self.adjust_timeline_density(false),
                std::cmp::Ordering::Equal => false,
            },
            // A preview is presentation only. The widget has retained it for
            // paint and AT's current value; it must not make collaboration
            // history noisy by changing a collection just because it was
            // hovered.
            DateIndexIntent::PreviewChanged(_) => true,
        }
    }

    pub(super) fn handle_collection_page_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.library_mode || self.collection_page_hits.is_empty() {
            return false;
        }
        for (hit, button) in &mut self.collection_page_buttons {
            if self
                .collection_page_hits
                .iter()
                .any(|(visible, _, _)| visible == hit)
            {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
            *ctx.handled = true;
            return true;
        }
        false
    }

    /// Navigate to an extant stable album id without producing a CRDT action.
    /// This is deliberately separate from `perform_effect`: callers need a
    /// useful stale-address error instead of silently falling back to all
    /// photographs if a collaborator removed the album after search indexed
    /// it.
    /// Open photograph `photo_id` in this window, from search, the palette
    /// or an agent.
    pub(super) fn view_photo(
        &mut self,
        library: &crate::model::PhotoLibrary,
        photo_id: &str,
    ) -> Result<(), String> {
        if library.photo(photo_id).cloned().is_none() {
            return Err("that photograph is no longer in the library".to_owned());
        }
        self.show_photo(photo_id);
        self.album_editor = false;
        self.metadata_editor = false;
        Ok(())
    }

    pub(super) fn view_album(
        &mut self,
        library: &crate::model::PhotoLibrary,
        album_id: &str,
    ) -> Result<(), String> {
        if library.album(album_id).cloned().is_none() {
            return Err("that album is no longer in the library".to_owned());
        }
        self.show_library();
        self.active_album = Some(album_id.to_owned());
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }

    /// Enter a metadata-derived library view after verifying the label still
    /// exists. An old global-search result therefore fails honestly when a
    /// collaborator removes the final matching photograph.
    pub(super) fn view_metadata_filter(
        &mut self,
        library: &crate::model::PhotoLibrary,
        folder: Option<String>,
        person: Option<String>,
        tag: Option<String>,
    ) -> Result<(), String> {
        let exists = match (&folder, &person, &tag) {
            (Some(folder), None, None) => library
                .photos
                .values()
                .any(|photo| photo.folder.as_deref() == Some(folder)),
            (None, Some(person), None) => library
                .photos
                .values()
                .any(|photo| photo.people.iter().any(|value| value == person)),
            (None, None, Some(tag)) => library
                .photos
                .values()
                .any(|photo| photo.tags.iter().any(|value| value == tag)),
            _ => false,
        };
        if !exists {
            return Err(tr("photos-error-filter-gone"));
        }
        self.show_library();
        self.active_album = None;
        self.active_folder = folder;
        self.active_person = person;
        self.active_tag = tag;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }

    /// Open one exact intelligence-derived collection. These labels are
    /// still local navigation; only the accepted analysis record is shared.
    pub(super) fn view_analysis_filter(
        &mut self,
        library: &crate::model::PhotoLibrary,
        label: String,
        pet: bool,
    ) -> Result<(), String> {
        let exists = library.photos.values().any(|photo| {
            photo.analysis().is_some_and(|analysis| {
                if pet {
                    analysis.pets.contains(&label)
                } else {
                    analysis.objects.contains(&label)
                }
            })
        });
        if !exists {
            return Err(tr("photos-error-filter-gone"));
        }
        self.show_library();
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = (!pet).then_some(label.clone());
        self.active_pet = pet.then_some(label);
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        Ok(())
    }

    /// Navigate to the stable favorites resource after verifying that it is
    /// still non-empty. A stale global-search result must fail rather than
    /// silently changing the user's view to an unrelated all-photos shelf.
    pub(super) fn view_favorites(
        &mut self,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        if !library.photos.values().any(|photo| photo.favorite) {
            return Err("there are no favorite photographs in the library".to_owned());
        }
        self.show_library();
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = true;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }
}

pub(super) fn build_collection_page_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(CollectionPageHit, Button)> {
    [
        (
            CollectionPageHit::PeoplePrevious,
            "photos-people-previous",
            "photos-previous",
        ),
        (
            CollectionPageHit::PeopleNext,
            "photos-people-next",
            "photos-next",
        ),
        (
            CollectionPageHit::PlacesPrevious,
            "photos-places-previous",
            "photos-previous",
        ),
        (
            CollectionPageHit::PlacesNext,
            "photos-places-next",
            "photos-next",
        ),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let button = Button::new(id, label)
            .tooltip(label)
            .with_accessible_name(label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

impl CollectionPageHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::AlbumsPrevious => PhotosEffect::PreviousAlbumsPage,
            Self::AlbumsNext => PhotosEffect::NextAlbumsPage,
            Self::PeoplePrevious => PhotosEffect::PreviousPeoplePage,
            Self::PeopleNext => PhotosEffect::NextPeoplePage,
            Self::PlacesPrevious => PhotosEffect::PreviousPlacesPage,
            Self::PlacesNext => PhotosEffect::NextPlacesPage,
        }
    }
}
