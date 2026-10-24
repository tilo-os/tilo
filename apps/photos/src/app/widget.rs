//! The toolkit traits: how the window syncs from its document, lays out,
//! draws, handles input and presents itself as an app.

use crate::model::{LibraryPhoto, PhotoLibrary};
use crate::strings::{tr, tr_with};
use crate::timeline_projection::TimelinePhoto;
use std::collections::{HashMap, HashSet};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, Action as AccessAction};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Id;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{ModalHit, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::keyboard::{Key, NamedKey};

use super::{
    AlbumEditorHit, COLLECTION_PAGE_SIZE, LibraryDestination, LibraryItem, PeopleIntent,
    PeopleSelection, PhotosApp, PlaceItem, PlacesHit, capture_day, card_action_controls,
    wheel_pixels,
};

impl Widget for PhotosApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    /// Shared controls remain retained children even in the painted media
    /// surface, with their own focus, action custody and accessibility.
    fn children(&self) -> Vec<&dyn Widget> {
        self.empty_state
            .as_ref()
            .map(|empty| empty as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        children.extend(
            self.empty_state
                .as_mut()
                .map(|empty| empty as &mut (dyn Widget + '_)),
        );
        children
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.places_motion_reduced = reduced;
        if reduced {
            // Reduced motion keeps the camera operation immediate and every
            // marker fully present; only the decorative settle cue disappears.
            self.places_camera_feedback = None;
        }
    }

    /// A preview of a photograph on screen is still being decoded: a capture
    /// waits for it rather than showing a placeholder.
    fn has_pending_visual_work(&self) -> bool {
        self.staged.is_some()
            || !self.thumbnail_queued.is_empty()
            || (self.thumbnail_generation.is_some()
                && self.thumbnail_visible_ids.iter().any(|id| {
                    !self.library_thumbnails.contains_key(id)
                        && !self.thumbnail_unavailable.contains(id)
                }))
    }

    fn needs_animation_frame(&self) -> bool {
        let now = ui::time::now();
        self.chrome.inspector.is_animating()
            || (!self.places_motion_reduced
                && (self
                    .places_camera_feedback
                    .is_some_and(|feedback| feedback.is_moving(now))
                    || self.people_detail_transition.is_moving(now)))
            || self
                .directory_import_details_scroll
                .needs_animation_frame(&self.layout_theme)
            || self
                .selection_album_scroll
                .needs_animation_frame(&self.layout_theme)
    }

    /// The modal lists glide on after a flick, as the wheel scrolls them;
    /// that changes no model.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.directory_import_details_scroll.advance();
        self.selection_album_scroll.advance();
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn take_structure_dirty(&mut self) -> bool {
        let viewer = self
            .viewer
            .as_mut()
            .is_some_and(Widget::take_structure_dirty);
        std::mem::take(&mut self.structure_dirty) | viewer
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn layout_needs_measure(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "synchronization derives the bounded library, filters, search, and thumbnail work from one document snapshot"
    )]
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        if self.copy.failed() {
            self.notice = Some(tr("photos-copy-failed"));
        }
        self.apply_window_toolbar();
        self.open_chosen_places();
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.follow_cancellations();
        self.open_launched();
        self.reconcile_content();
        self.apply_staged();
        self.drain_action_control_pending();
        self.drain_opens();
        self.drain_analysis_results();
        self.drain_share_outcome();
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.import_content = None;
            self.open_path(&path);
            self.settle(if self.notice.is_none() {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.open_failed".into(),
                        message: tr("photos-open-failed"),
                        details: None,
                        retryable: false,
                    },
                }
            });
        }
        self.drain_picker();
        self.drain_directory_import();
        self.refresh();
        let revision = Some(self.presented_revision);
        let document_changed = revision != self.search_revision;
        if document_changed {
            self.search_refresh_error = match self.refresh_library_index() {
                Ok(()) => None,
                Err(error) => Some(tr_with("photos-search-stale", "reason", &error.to_string())),
            };
            self.thumbnail_unavailable.clear();
            self.search_revision = revision;
        }
        let library = self.presented();
        self.library_analysis_coverage = library.photos.values().fold(
            (0usize, 0usize, 0usize),
            |(analyzed, available, awaiting), photo| {
                if photo.analysis().is_some_and(|analysis| {
                    analysis
                        .vision_provenance_for_content(&photo.image.content)
                        .is_some()
                }) {
                    (analyzed + 1, available, awaiting)
                } else if crate::model::bytes_of(&photo.image, &self.content).is_some() {
                    (analyzed, available + 1, awaiting)
                } else {
                    (analyzed, available, awaiting + 1)
                }
            },
        );
        self.selection_all_favorite = !self.selected_photo_ids.is_empty()
            && self
                .selected_photo_ids
                .iter()
                .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
        if self
            .selection_destination_album
            .as_ref()
            .is_some_and(|id| library.album(id).is_none())
        {
            self.selection_destination_album = None;
            self.selection_album_chooser = false;
            self.notice = Some(tr("photos-album-gone"));
        }
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .values()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        let matches_filter = |photo: &LibraryPhoto| {
            membership.is_none_or(|ids| ids.contains(&photo.id))
                && (!self.active_favorites || photo.favorite)
                && self
                    .active_folder
                    .as_ref()
                    .is_none_or(|folder| photo.folder.as_ref() == Some(folder))
                && self
                    .active_person
                    .as_ref()
                    .is_none_or(|person| photo.people.contains(person))
                && self
                    .active_tag
                    .as_ref()
                    .is_none_or(|tag| photo.tags.contains(tag))
                && self.active_object.as_ref().is_none_or(|object| {
                    photo
                        .vision
                        .as_ref()
                        .is_some_and(|vision| vision.objects.contains(object))
                })
                && self.active_pet.as_ref().is_none_or(|pet| {
                    photo
                        .vision
                        .as_ref()
                        .is_some_and(|vision| vision.pets.contains(pet))
                })
        };
        self.library_total = library
            .photos
            .values()
            .filter(|photo| matches_filter(photo))
            .count();
        self.library_photo_count = library.photos.len();
        self.library_favorite_count = library
            .photos
            .values()
            .filter(|photo| photo.favorite)
            .count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        let mut timeline_items: Vec<_> = library
            .photos
            .values()
            .filter(|photo| matches_filter(photo))
            .map(|photo| LibraryItem {
                id: photo.id.clone(),
                name: photo.image.name.clone(),
                favorite: photo.favorite,
                captured_at: photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day),
            })
            .collect();
        // A timeline reads newest-first.  The id tie-breaker makes peers with
        // the same capture-day stable without making the date a mutable sort
        // key in the shared document.
        timeline_items.sort_by(|left, right| {
            let left_undated = left.captured_at == "Undated";
            let right_undated = right.captured_at == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right.captured_at.cmp(&left.captured_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        let projection_filter = format!(
            "{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}",
            self.active_album,
            self.active_folder,
            self.active_person,
            self.active_tag,
            self.active_object,
            self.active_pet,
            self.active_favorites
        );
        if self.timeline_projection_revision != revision
            || self.timeline_projection_filter != projection_filter
        {
            self.timeline_projection.reconcile(
                library
                    .photos
                    .values()
                    .filter(|photo| matches_filter(photo))
                    .map(|photo| TimelinePhoto {
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
                    }),
            );
            self.timeline_projection_revision = revision;
            self.timeline_projection_filter = projection_filter;
            self.library_total = self.timeline_projection.len();
        }
        // Markers are rebuilt from exact pixel day offsets during draw, once
        // the real viewport and responsive geometry are known.
        self.timeline_index_markers.clear();
        let visible_ids: HashSet<_> = self
            .timeline_projection
            .visible_photos(self.timeline_scroll_y, self.timeline_viewport_height)
            .into_iter()
            .map(|entry| entry.photo.id.clone())
            .collect();
        self.library_items = if visible_ids.is_empty() {
            timeline_items.into_iter().take(120).collect()
        } else {
            timeline_items
                .into_iter()
                .filter(|item| visible_ids.contains(&item.id))
                .take(120)
                .collect()
        };
        self.places_items = library
            .photos
            .values()
            .filter(|photo| {
                photo
                    .location
                    .as_ref()
                    .is_some_and(crate::model::PhotoLocation::is_valid)
            })
            .cloned()
            .map(|photo| PlaceItem { photo })
            .collect();
        self.library_albums = library
            .albums
            .values()
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        self.library_album_covers = library
            .albums
            .values()
            .map(|album| {
                let covers = album
                    .photo_ids
                    .iter()
                    .filter(|id| library.photo(id).is_some())
                    .take(4)
                    .cloned()
                    .collect();
                (album.id.clone(), covers)
            })
            .collect();
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        let mut tags = std::collections::BTreeMap::<String, usize>::new();
        for photo in library.all_photos() {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in photo.people.iter() {
                *people.entry(person.clone()).or_default() += 1;
            }
            for tag in photo.tags.iter() {
                *tags.entry(tag.clone()).or_default() += 1;
            }
        }
        self.people_approved = crate::people::approved_people(&library);
        self.library_folders = folders.into_iter().take(6).collect();
        self.library_people = people.into_iter().collect();
        self.albums_page = self
            .albums_page
            .min(self.library_albums.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.people_page = self
            .people_page
            .min(self.library_people.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.places_page = self
            .places_page
            .min(self.places_items.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.library_person_covers.clear();
        for (person, _) in self
            .library_people
            .iter()
            .skip(self.people_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
        {
            if let Some(photo) = library
                .photos
                .values()
                .find(|photo| photo.people.iter().any(|label| label == person))
            {
                self.library_person_covers
                    .insert(person.clone(), photo.id.clone());
            }
        }
        // The dedicated People management surface may show approved people
        // outside the current browse-shelf page, so add their bounded covers
        // without changing collection-page navigation.
        for person in &self.people_approved {
            if self.library_person_covers.contains_key(&person.label) {
                continue;
            }
            if let Some(photo) = library
                .photos
                .values()
                .find(|photo| photo.people.iter().any(|label| label == &person.label))
            {
                self.library_person_covers
                    .insert(person.label.clone(), photo.id.clone());
            }
        }
        self.library_tags = tags.into_iter().take(6).collect();
        // Request exactly the sorted, virtualised timeline slice, not the
        // document's original insertion order.  Otherwise a visible newest
        // photo can remain a placeholder while an off-screen old one decodes.
        let mut thumbnail_sources: Vec<_> = self
            .library_items
            .iter()
            .take(24)
            .filter_map(|item| library.photo(&item.id).cloned())
            .collect();
        if self.library_destination == LibraryDestination::Places {
            thumbnail_sources = self
                .places_items
                .iter()
                .take(24)
                .map(|item| item.photo.clone())
                .collect();
        } else if self.library_destination == LibraryDestination::Albums {
            thumbnail_sources = self
                .library_albums
                .iter()
                .filter_map(|(id, _, _)| self.library_album_covers.get(id)?.first())
                .filter_map(|photo_id| library.photo(photo_id).cloned())
                .take(24)
                .collect();
        } else {
            for cover in self.library_person_covers.values() {
                if thumbnail_sources.iter().all(|photo| &photo.id != cover)
                    && let Some(photo) = library.photo(cover).cloned()
                {
                    thumbnail_sources.push(photo);
                }
            }
            thumbnail_sources.truncate(24);
        }
        if self.library_destination == LibraryDestination::People {
            for cluster in self.people_controller.all_clusters() {
                for photo_id in cluster.visible_photo_ids().into_iter().take(4) {
                    if thumbnail_sources.iter().all(|photo| photo.id != photo_id)
                        && let Some(photo) = library.photo(&photo_id).cloned()
                    {
                        thumbnail_sources.push(photo);
                    }
                }
            }
            thumbnail_sources.truncate(40);
        }
        // The sidebar shows each album by its cover, whatever is open.
        for cover in self
            .library_albums
            .iter()
            .filter_map(|(id, _, _)| self.library_album_covers.get(id)?.first())
            .take(24)
        {
            if thumbnail_sources.iter().all(|photo| &photo.id != cover)
                && let Some(photo) = library.photo(cover).cloned()
            {
                thumbnail_sources.push(photo);
            }
        }
        self.drain_thumbnail_results();
        self.schedule_library_thumbnail(thumbnail_sources, document_changed);
        self.refresh_chrome_text(doc);
        self.refresh_empty_state(doc);
        self.publish_notice();
        // A narrow window can show the sidebar as its only screen, and the
        // shell paints no content behind it: the places come from the
        // library here, not only from a paint.
        self.show_places();
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.notice_said.deadline())
            .min()
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    #[allow(
        clippy::unwrap_used,
        reason = "root's taffy node always builds from freshly-built children"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // A docked inspector takes its column beside the content; in a
        // narrow window it floats over it and takes nothing. The sidebar is
        // the shell's, beside this window's content.
        self.chrome.inspector.scale_for(theme);
        self.sync_inspector();
        let beside = self.chrome.inspector.resolve(self.rect);
        let mut inspector_inset = (self.rect.x1 - beside.x1).max(0.0);
        // A panel floating over an empty view would hide what the empty
        // state offers (Open, on an empty library): it stands in the part
        // the panel leaves, where that still holds it.
        if self.empty_state.is_some()
            && self.chrome.inspector.is_overlay()
            && self.chrome.inspector.is_open()
        {
            let covered =
                self.chrome.inspector.resolved_overlay_width() + f64::from(theme.tokens.spacing.s2);
            if self.rect.width() - covered >= f64::from(theme.tokens.metrics.grid_min_width) {
                inspector_inset = covered;
            }
        }
        // The layout tree points at the People buttons, and the framework
        // syncs again before it measures; rebuilding them only here keeps
        // those pointers alive until the next tree replaces them.
        self.refresh_people_controls();

        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy, theme);

        let style = Style {
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            padding: taffy::Rect {
                left: LengthPercentage::length(0.0),
                right: LengthPercentage::length(inspector_inset as f32),
                top: LengthPercentage::length(0.0),
                bottom: LengthPercentage::length(0.0),
            },
            ..Default::default()
        };
        // The toolbar is the window bar's: the canvas starts at the top.
        taffy.widget_node(style, &[canvas, status], node_map, self)
    }

    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let here = self.rect.origin();
        let child_rect = |child: NodeId| ui::widget::layout_rect(taffy, child, here);

        let children = taffy.children(node).unwrap();
        if let Some(&canvas) = children.first() {
            self.canvas_rect = child_rect(canvas);
            let canvas_origin = Point::new(self.canvas_rect.x0, self.canvas_rect.y0);
            let people_surface = self.library_mode
                && (self.library_destination == LibraryDestination::People || self.people_mode);
            if people_surface {
                let canvas_children = taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .apply_layout(taffy, field, canvas_origin);
                    }
                    index += 1;
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control.button.apply_layout(taffy, button, canvas_origin);
                        if !control.is_card() {
                            control.width = control.button.get_rect().width();
                        }
                    }
                    index += 1;
                }
                // The tree was placed from the canvas the previous frame
                // had; place the surface again on the canvas it has now, so
                // the painted headings and the controls always agree.
                let theme = self.layout_theme.clone();
                self.layout_people_controls(&theme);
                if show_name {
                    self.people_name_input
                        .apply_layout_external(self.people_name_bounds);
                }
                for control in &mut self.people_controls {
                    let bounds = if control.is_card() {
                        Rect::from_origin_size(canvas_origin, (0.0, 0.0))
                    } else {
                        control.bounds
                    };
                    control.button.apply_layout_external(bounds);
                }
            } else if let Some(empty) = &mut self.empty_state {
                empty.apply_layout(taffy, canvas, here);
            } else if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(1) {
            self.status_rect = child_rect(status);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // No window ground: the framework paints `surfaces.content` under
        // the app; the sidebar is the shell's.
        self.chrome.inspector.tick(theme);
        self.chrome.inspector.resolve(self.rect);
        self.place_chrome();

        if self.library_mode {
            if self.library_destination == LibraryDestination::Albums {
                self.draw_albums(scene, transform, text_renderer, theme);
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                self.draw_people(scene, transform, text_renderer, theme);
                if self.people_selection.is_some() && !self.people_merge_mode {
                    self.people_name_input
                        .draw(scene, transform, text_renderer, theme, presence);
                }
                for control in &mut self.people_controls {
                    if !control.is_card() {
                        control
                            .button
                            .draw(scene, transform, text_renderer, theme, presence);
                    }
                }
            } else if self.library_destination == LibraryDestination::Places {
                self.draw_places(scene, transform, text_renderer, theme, presence);
            } else if self.library_destination == LibraryDestination::Analysis {
                self.draw_library_analysis(scene, transform, text_renderer, theme, presence);
            } else {
                self.draw_library(scene, transform, text_renderer, theme);
            }
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
            self.draw_viewer_details(scene, transform, text_renderer, theme);
        } else {
            self.draw_empty_state(scene, transform, text_renderer, theme);
        }

        self.draw_directory_import_status(scene, transform, text_renderer, theme);
        self.paint_status(scene, transform, text_renderer, theme);
        // The inspector comes last: floating in a narrow window it stands
        // over everything else.
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
        if self.library_mode
            && !self.people_mode
            && self.library_destination == LibraryDestination::Timeline
        {
            self.draw_metadata_editor(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.library_mode && self.selection_album_chooser {
            self.draw_selection_destination_sheet(scene, transform, text_renderer, theme, presence);
        }
        self.draw_directory_import_details(scene, transform, text_renderer, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event entry point preserves explicit precedence between album chips, grid cards, toolbar, and viewer"
    )]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.reconcile_content();
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            let next = ctx.logical(position);
            if self.library_mode
                && self.library_destination == LibraryDestination::Places
                && let Some(previous) = self.places_drag_origin.replace(next)
            {
                self.set_places_viewport(self.places_viewport.panned(
                    next.x - previous.x,
                    next.y - previous.y,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                ));
            }
            self.pointer = next;
        }
        if self.directory_import_details_open {
            let theme = self.layout_theme.clone();
            self.directory_import_details_scroll.set_focused(true);
            self.directory_import_details_scroll
                .handle_event(event, ctx, &theme);
            if *ctx.handled {
                return;
            }
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
            {
                match &key.logical_key {
                    Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                        self.close_directory_details();
                    }
                    _ => return,
                }
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) {
                if let Some(layout) = self.directory_import_details_layout.clone() {
                    match layout.hit(self.pointer) {
                        Some(ModalHit::Close | ModalHit::Scrim | ModalHit::Action(0)) => {
                            self.close_directory_details();
                        }
                        Some(ModalHit::Card | ModalHit::Action(_)) | None => {}
                    }
                }
                *ctx.handled = true;
                return;
            }
            // A modal swallows pointer/keyboard input that it does not use;
            // cursor motion still reaches it so hover feedback remains live.
            if !matches!(event, winit::event::WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return;
            }
        }
        if self.library_mode && self.selection_album_chooser {
            let theme = self.layout_theme.clone();
            self.selection_album_scroll.set_focused(true);
            self.selection_album_scroll.handle_event(event, ctx, &theme);
            if *ctx.handled {
                return;
            }
        }
        if !self.selection_album_chooser && self.handle_chrome_event(ctx) {
            self.drain_pending();
            return;
        }
        if let Some(empty) = &mut self.empty_state {
            empty.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        for (_, button) in &mut self.directory_import_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if self.directory_import_pending.get().is_none()
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .directory_import_buttons
                .iter()
                .find(|(_, button)| button.is_enabled() && button.get_rect().contains(self.pointer))
        {
            self.directory_import_pending.set(Some(*hit));
        }
        if let Some(hit) = self.directory_import_pending.take() {
            if self.activate_directory_status(hit) {
                *ctx.handled = true;
            }
            return;
        }
        if *ctx.handled {
            return;
        }
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        if people_surface {
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
                && matches!(key.logical_key, Key::Named(NamedKey::Escape))
            {
                self.handle_people_intent(PeopleIntent::Back);
                *ctx.handled = true;
                return;
            }
            if let winit::event::WindowEvent::MouseWheel { delta, .. } = event
                && self.people_selection.is_none()
                && self.people_layout.lanes.contains(self.pointer)
            {
                let theme = self.layout_theme.clone();
                self.people_scroll += wheel_pixels(delta, &theme);
                self.layout_people_controls(&theme);
                *ctx.handled = true;
                return;
            }
            let root_children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
            if let Some(&canvas) = root_children.get(1) {
                let canvas_children = ctx.taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .handle_event(event, &mut ctx.reborrow(ctx.path, field));
                    }
                    index += 1;
                    if self.people_name_input.take_submit() {
                        let intent = match self.people_selection {
                            Some(PeopleSelection::Approved(_)) => PeopleIntent::SaveName,
                            Some(PeopleSelection::Suggestion(_)) => PeopleIntent::ApproveSuggestion,
                            None => PeopleIntent::Back,
                        };
                        self.handle_people_intent(intent);
                        *ctx.handled = true;
                        return;
                    }
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control
                            .button
                            .handle_event(event, &mut ctx.reborrow(ctx.path, button));
                    }
                    index += 1;
                }
            }
            let mut pending = self.people_pending.borrow_mut().take();
            if pending.is_none()
                && matches!(
                    event,
                    winit::event::WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        button: winit::event::MouseButton::Left,
                        ..
                    }
                )
            {
                let lanes = self.people_layout.lanes;
                pending = self
                    .people_controls
                    .iter()
                    .find(|control| {
                        control.is_card()
                            && control.bounds.contains(self.pointer)
                            && lanes.contains(self.pointer)
                    })
                    .map(|control| control.intent.clone());
            }
            if let Some(intent) = pending {
                self.handle_people_intent(intent);
                *ctx.handled = true;
                return;
            }
            if *ctx.handled {
                return;
            }
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && self.places_map_bounds.contains(self.pointer)
            && !self
                .places_hits
                .iter()
                .any(|(_, rect)| rect.contains(self.pointer))
        {
            self.places_drag_origin = Some(self.pointer);
            *ctx.handled = true;
            return;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            self.places_drag_origin = None;
        }
        if self.library_mode && self.library_destination == LibraryDestination::Analysis {
            for (_, button) in &mut self.library_analysis_buttons {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                if sink.is_some() {
                    *ctx.focus_target = sink;
                }
            }
            if let Some(hit) = self.library_analysis_pending.take() {
                self.activate_library_analysis(hit);
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) && let Some((photo_id, _)) = self
                .library_analysis_failure_hits
                .iter()
                .find(|(_, bounds)| bounds.contains(self.pointer))
                .cloned()
            {
                self.show_photo(&photo_id);
                *ctx.handled = true;
                return;
            }
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Albums
            && !self.people_mode
            && self.albums_viewport.contains(self.pointer)
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            self.albums_scroll += wheel_pixels(delta, &self.layout_theme);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(point) => point.y / 80.0,
            };
            self.zoom_places_by(amount * 0.25);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .places_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            match hit {
                PlacesHit::Place(key) => {
                    let _ = self.open_place_collection(&self.presented(), &key);
                }
                PlacesHit::Photo(id) => {
                    self.show_photo(&id);
                }
            }
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        -44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        -44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Character(key) if key == "+" || key == "=" => self.zoom_places_by(1.0),
                Key::Character(key) if key == "-" => self.zoom_places_by(-1.0),
                Key::Character(key) if key.eq_ignore_ascii_case("l") => {
                    self.places_list_mode = !self.places_list_mode;
                }
                _ => return,
            }
            *ctx.handled = true;
            return;
        }

        // Give the shared chronological index first refusal only while its
        // narrow track/preview lane owns the pointer (or it holds a drag / AT
        // focus). Ordinary scrolling anywhere else remains the timeline's
        // smooth wheel path below.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
        {
            let index_owns_event = self.timeline_index.contains_interaction_point(self.pointer)
                || self.timeline_index.is_dragging()
                || self.timeline_index.is_focused();
            if index_owns_event
                && matches!(
                    event,
                    winit::event::WindowEvent::CursorMoved { .. }
                        | winit::event::WindowEvent::MouseInput { .. }
                        | winit::event::WindowEvent::MouseWheel { .. }
                        | winit::event::WindowEvent::KeyboardInput { .. }
                )
            {
                let was_dragging = self.timeline_index.is_dragging();
                if let Some(intent) = self.timeline_index.handle_event(
                    event,
                    ctx.scale_factor,
                    self.timeline_index_metrics,
                    &self.timeline_index_markers,
                ) {
                    let _ = self.apply_timeline_index_intent(&intent);
                    *ctx.handled = true;
                    return;
                }
                if was_dragging || self.timeline_index.is_dragging() {
                    *ctx.handled = true;
                    return;
                }
            }
        }

        // Ctrl + wheel is the cross-platform density gesture.  Keep an
        // ordinary wheel for chronological browsing; otherwise a trackpad
        // scroll would unexpectedly rearrange the entire library.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && ctx.modifiers.state().control_key()
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if amount != 0.0 {
                // Natural scroll semantics: up means see more at once.
                self.adjust_timeline_density(amount < 0.0);
                *ctx.handled = true;
                return;
            }
        }

        // Tilo's vendored Wayland winit maps zwp_pointer_gestures_v1 to this
        // same event; macOS/iOS use it natively.  Ctrl-wheel above remains the
        // fallback on compositors that do not advertise pinch gestures.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::PinchGesture { delta, .. } = event
            && delta.is_finite()
            && *delta != 0.0
        {
            self.adjust_timeline_density(*delta > 0.0);
            *ctx.handled = true;
            return;
        }

        if self.library_mode && self.handle_metadata_editor_event(event, ctx) {
            return;
        }

        if self.library_mode && self.handle_album_editor_event(event, ctx) {
            return;
        }

        if self.handle_collection_page_event(event, ctx) {
            return;
        }

        if self.library_mode
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("a"))
                && self.selection_mode
            {
                self.select_all_visible();
                *ctx.handled = true;
                return;
            }
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value == " ")
            {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
                *ctx.handled = true;
                return;
            }
            if self.selection_mode && matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
                self.finish_selection();
                *ctx.handled = true;
                return;
            }
            let forward = match &key.logical_key {
                Key::Named(NamedKey::PageDown | NamedKey::ArrowRight) => Some(true),
                Key::Named(NamedKey::PageUp | NamedKey::ArrowLeft) => Some(false),
                _ => None,
            };
            if let Some(forward) = forward {
                if self.album_editor {
                    self.page_library(forward);
                } else {
                    self.scroll_timeline_rows(if forward { 3.0 } else { -3.0 });
                }
                *ctx.handled = true;
                return;
            }
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_edit_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = Some(album_id);
            self.begin_edit_album();
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((forward, enabled, _)) = self
                .library_page_hits
                .iter()
                .find(|(_, _, rect)| rect.contains(self.pointer))
                .copied()
            && enabled
        {
            self.page_library(forward);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && self
                .selection_album_scroll
                .content_rect()
                .contains(self.pointer)
            && let Some((album_id, _)) = self
                .selection_destination_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.selection_destination_album = Some(album_id);
            self.notice = Some(tr("photos-selection-album-chosen"));
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .selection_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.handle_selection_hit(hit);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
        {
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(),
                AlbumEditorHit::Edit => self.begin_edit_album(),
                _ => {}
            }
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = album_id;
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some(card) = self
                .metadata_filter_cards
                .iter()
                .find(|card| card.bounds.contains(self.pointer))
                .cloned()
        {
            if card.favorites {
                let _ = self.view_favorites(&self.presented());
            } else {
                self.active_album = None;
                self.active_folder = card.folder;
                self.active_person = card.person;
                self.active_tag = card.tag;
                self.active_favorites = false;
                self.library_offset = 0;
                self.timeline_scroll_y = 0.0;
                self.notice = None;
            }
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((id, _)) = self
                .library_details_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.begin_metadata_editor(&id);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let pixels = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
            };
            self.scroll_timeline_pixels(pixels);
            *ctx.handled = true;
            return;
        }

        // A library card is a direct manipulation surface, but it is still
        // only a front end for a canonical action. The trailing star and
        // remove affordance have their own hit areas, so destructive and
        // metadata changes do not unexpectedly open the editor.
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((photo_id, card, favorite)) = self
                .library_cards
                .iter()
                .find(|(_, card, _)| card.contains(self.pointer))
                .cloned()
        {
            if self.selection_mode {
                self.toggle_library_selection(&photo_id, ctx.modifiers.state().shift_key());
            } else if !self.album_editor
                && card_action_controls(card, &self.layout_theme)
                    .favorite
                    .union(card_action_controls(card, &self.layout_theme).remove)
                    .contains(self.pointer)
            {
                let actions = card_action_controls(card, &self.layout_theme);
                if actions.favorite.contains(self.pointer) {
                    self.library_remove_confirm = None;
                    self.commit(crate::model::photo_paths(&photo_id).favorite.set(!favorite));
                } else if self.library_remove_confirm.as_deref() == Some(&photo_id) {
                    self.commit(PhotoLibrary::REMOVE_PHOTO.with(Id::from_raw(photo_id)));
                    self.library_remove_confirm = None;
                    self.notice = Some(tr("photos-removed"));
                } else {
                    self.library_remove_confirm = Some(photo_id);
                    self.notice = Some(tr("photos-remove-confirm"));
                }
            } else {
                self.library_remove_confirm = None;
                self.show_photo(&photo_id);
            }
            *ctx.handled = true;
            return;
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(1)) {
            v.handle_event(event, &mut ctx.reborrow(ctx.path, canvas));
        }
        self.drain_pending();
    }

    fn name(&self) -> &'static str {
        "PhotosApp"
    }

    /// Hand the concrete app back out of a `Box<dyn Widget>`.
    ///
    /// A compositor hosting this in a tab holds it as a trait object, so
    /// "open this file in the photo viewer that is already on screen" has no
    /// route in without it. Opening still goes through
    /// [`PhotosApp::open_path`] — this only recovers the type, it does not add
    /// a second way to change what is shown.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.accessibility_tree())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self
            .empty_state
            .as_mut()
            .is_some_and(|empty| empty.perform_semantic_action(target, action, state, path))
        {
            return true;
        }
        let (state, _) = ui::doc::document(state, path);
        self.reconcile_content();
        self.perform_directory_semantic_action(target, action)
            || self.perform_library_semantic_action(target, action, state)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.canvas_rect),
            _ => None,
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's tab vocabulary has no picture variant. Photos edits a
        // file and writes the result back, which is what `Editor` names; the
        // glyph below is what actually tells the two apart in a tab pill.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        // The open file's own name, so two photographs open at once are two
        // distinguishable tabs rather than two tabs called "Photos". Nothing
        // open falls back to the app's name, which is the only true thing to
        // say about an empty window.
        self.open_image_path()
            .and_then(|p| p.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.app_name)
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font image glyph, matching the convention every other hosted
        // app's tab pill uses.
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}

impl PhotosApp {
    /// A new notice is said in the window's bar for a while.
    fn publish_notice(&mut self) {
        if self.notice != self.notice_last {
            self.notice_last.clone_from(&self.notice);
            match self.notice.clone() {
                Some(notice) => self.notice_said.show(notice),
                None => {
                    self.notice_said.dismiss();
                }
            }
        }
        self.notice_said.expire();
        self.notice_said.publish(|text| {
            ui::window_toolbar::ToolbarNotice::new(text.clone(), ui::widget::toast::ToastKind::Info)
        });
    }
}
