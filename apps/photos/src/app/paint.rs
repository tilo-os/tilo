//! Layout and painting of the library surfaces: the grid and timeline, albums,
//! places, library analysis, the viewer details and the empty state.

use crate::strings::{tr, tr_args, tr_count};
use crate::timeline_projection::TimelineGeometry;
use std::collections::HashMap;
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use ui::ArgValue;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, ButtonRole, DateIndexMarker, DateIndexMarkerKind, EmptyState,
    EmptyStateContent, ModalAction, ModalSpec, PropertyListFace, PropertyRow, ScrollMetrics,
    Widget,
};
use ui::widget::{ListRowFace, MediaCardFace, StatTileFace};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke, Vec2};

use super::{
    AlbumEditorHit, COLLECTION_PAGE_SIZE, CanvasState, CollectionPageHit, LibraryAnalysisHit,
    LibraryDestination, PhotosApp, PlaceItem, PlacesHit, SELECTION_ALBUMS_PER_PAGE, SelectionHit,
    ViewerDetails, captured_label, card_action_controls, displayed_image_rect, thumbnail_key,
    timeline_date_label, timeline_row_height,
};

/// Coarse, bundled coastlines for the private/offline Places overview. These
/// are intentionally only continental silhouettes: they add orientation but
/// cannot be mistaken for current navigation data or a precision basemap.
pub(super) const OFFLINE_WORLD_LANDMASSES: &[&[(f64, f64)]] = &[
    &[
        (72.0, -168.0),
        (70.0, -140.0),
        (60.0, -125.0),
        (50.0, -124.0),
        (32.0, -117.0),
        (18.0, -104.0),
        (8.0, -83.0),
        (20.0, -74.0),
        (45.0, -61.0),
        (58.0, -72.0),
        (72.0, -95.0),
        (72.0, -168.0),
    ],
    &[
        (13.0, -81.0),
        (6.0, -77.0),
        (-8.0, -78.0),
        (-25.0, -70.0),
        (-55.0, -68.0),
        (-52.0, -42.0),
        (-20.0, -40.0),
        (2.0, -50.0),
        (13.0, -61.0),
        (13.0, -81.0),
    ],
    &[
        (36.0, -10.0),
        (51.0, -5.0),
        (61.0, 15.0),
        (70.0, 40.0),
        (58.0, 65.0),
        (72.0, 110.0),
        (62.0, 165.0),
        (45.0, 145.0),
        (20.0, 121.0),
        (7.0, 106.0),
        (22.0, 78.0),
        (10.0, 43.0),
        (31.0, 31.0),
        (36.0, -10.0),
    ],
    &[
        (35.0, -17.0),
        (31.0, 31.0),
        (12.0, 44.0),
        (-12.0, 43.0),
        (-35.0, 20.0),
        (-30.0, 10.0),
        (-5.0, 8.0),
        (15.0, -17.0),
        (35.0, -17.0),
    ],
    &[
        (-10.0, 113.0),
        (-18.0, 146.0),
        (-39.0, 147.0),
        (-44.0, 115.0),
        (-25.0, 112.0),
        (-10.0, 113.0),
    ],
    &[
        (-63.0, -180.0),
        (-70.0, -120.0),
        (-72.0, -60.0),
        (-68.0, 0.0),
        (-72.0, 60.0),
        (-69.0, 120.0),
        (-63.0, 180.0),
        (-63.0, -180.0),
    ],
];

impl PhotosApp {
    /// The photograph's shared details as label/value rows: when and where
    /// it was taken, who and what is in it. The name and size are already
    /// in the header and the status bar.
    fn viewer_detail_rows(details: &ViewerDetails) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        if let Some(description) = details.description.as_deref() {
            rows.push((tr("photos-description"), description.to_owned()));
        }
        if let Some(captured_at) = details.captured_at.as_deref() {
            rows.push((tr("photos-captured"), captured_label(captured_at)));
        }
        if let Some(place) = details.place.as_deref() {
            rows.push((tr("photos-place"), place.to_owned()));
        }
        if !details.people.is_empty() {
            rows.push((tr("photos-people"), details.people.join(", ")));
        }
        if !details.tags.is_empty() {
            rows.push((tr("photos-tags"), details.tags.join(", ")));
        }
        if let Some(folder) = details.folder.as_deref() {
            rows.push((tr("photos-folder"), folder.to_owned()));
        }
        rows
    }

    /// Show the photograph's shared details on the floating material over
    /// the picture itself, inside the bounds the picture is drawn in. Chrome
    /// over media is a floating panel; the picture keeps everything else.
    pub(super) fn draw_viewer_details(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (Some(details), Some(viewer), Some(rendered)) = (
            self.viewer_details.as_ref(),
            self.viewer.as_ref(),
            self.rendered.as_ref(),
        ) else {
            return;
        };
        let rows = Self::viewer_detail_rows(details);
        if rows.is_empty() {
            return;
        }
        let Some(image) = displayed_image_rect(
            viewer,
            f64::from(rendered.rgba.w),
            f64::from(rendered.rgba.h),
        ) else {
            return;
        };
        let inset = f64::from(theme.tokens.spacing.s3);
        let pad = f64::from(theme.tokens.spacing.s4);
        let width =
            (image.width() - 2.0 * inset).min(f64::from(theme.tokens.metrics.panel_width_list));
        // A picture too small to hold the panel keeps all its pixels; the
        // details stay in its accessibility description.
        if width < f64::from(theme.tokens.metrics.panel_width_utility) * 0.75 {
            return;
        }
        let property_rows: Vec<PropertyRow<'_>> = rows
            .iter()
            .map(|(label, value)| PropertyRow::new(label, value))
            .collect();
        let face = PropertyListFace::new(&property_rows);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let body_h = face.height(text_renderer, &ink, width - 2.0 * pad);
        let height = body_h + 2.0 * pad;
        // The details never take over the picture: where they would cover
        // more than a quarter of it, the picture keeps its pixels and the
        // details stay in its accessibility description.
        if height > image.height() - 2.0 * inset || width * height > image.area() / 4.0 {
            return;
        }
        let panel = Rect::new(
            image.x0 + inset,
            image.y1 - inset - height,
            image.x0 + inset + width,
            image.y1 - inset,
        );
        ui::widget::floating_surface::paint(scene, transform, panel, theme);
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, &ink),
            panel.inset(-pad),
        );
    }

    /// Nothing to show in the content column: the shared empty state, with
    /// one action to open a photograph where that is the way forward.
    pub(super) fn draw_empty_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        body: &str,
    ) {
        if let Some(empty) = &mut self.empty_state {
            empty.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &PresenceCtx::empty(),
            );
            return;
        }
        EmptyState::paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.canvas_rect,
            &EmptyStateContent {
                icon: Some(Icon::Image),
                illustration: None,
                title,
                message: Some(body),
                action_label: None,
                error: self.canvas_state == CanvasState::Error && !self.library_mode,
            },
            self.pointer,
        );
    }

    /// One line of `role` under `theme`.
    pub(super) fn line_of(theme: &Theme, role: ui::TextRole) -> f64 {
        f64::from(theme.text(role).line_height_px())
    }

    /// Lay out the canvas: the `ImageViewer` if a photograph is open, else a
    /// placeholder leaf sized to soak the remaining height. `flex_basis: 0`
    /// with `min_size: 0` is what stops a large image's intrinsic size from
    /// pushing the layout wider than the window — the failure mode where
    /// content is silently clipped because the container grew past its
    /// parent instead of the content being scaled to fit. Split out of
    /// `build_layout` to keep that dispatch under the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "the canvas layout dispatch keeps each surface's retained widget nodes together"
    )]
    pub(super) fn build_canvas_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        let canvas = if people_surface {
            self.layout_people_controls(theme);
            let mut children = Vec::new();
            let show_name = self.people_selection.is_some() && !self.people_merge_mode;
            if show_name {
                let field = self.people_name_input.build_layout(taffy, theme, node_map);
                let rect = self.people_name_bounds;
                let _ = taffy.set_style(
                    field,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size::from_lengths(rect.width() as f32, rect.height() as f32),
                        ..Default::default()
                    },
                );
                children.push(field);
            }
            for control in &mut self.people_controls {
                let node = control.button.build_layout(taffy, theme, node_map);
                // A card opens by itself; its button carries only the
                // control's name and action, at no size.
                let rect = if control.is_card() {
                    Rect::new(
                        self.canvas_rect.x0,
                        self.canvas_rect.y0,
                        self.canvas_rect.x0,
                        self.canvas_rect.y0,
                    )
                } else {
                    control.bounds
                };
                // A control's width is left to the button, which measures
                // its own label; the surface reads it back after layout.
                let width = if control.is_card() {
                    Dimension::length(0.0)
                } else {
                    Dimension::auto()
                };
                let _ = taffy.set_style(
                    node,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size {
                            width,
                            height: Dimension::length(rect.height() as f32),
                        },
                        ..Default::default()
                    },
                );
                children.push(node);
            }
            taffy
                .new_with_children(
                    Style {
                        position: Position::Relative,
                        size: Size::from_percent(1.0, 1.0),
                        ..Default::default()
                    },
                    &children,
                )
                .unwrap()
        } else if let Some(empty) = &mut self.empty_state {
            empty.build_layout(taffy, theme, node_map)
        } else {
            match self.viewer.as_mut() {
                Some(v) => v.build_layout(taffy, theme, node_map),
                None => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::auto(),
                        },
                        flex_grow: 1.0,
                        ..Default::default()
                    })
                    .unwrap(),
            }
        };
        // The photograph is media: it takes the whole content column, and
        // the library surfaces lay out their own gutters.
        let inset = 0.0;
        taffy
            .set_style(
                canvas,
                Style {
                    size: Size::auto(),
                    min_size: Size::from_lengths(0.0, 0.0),
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: Dimension::length(0.0),
                    margin: taffy::Rect {
                        left: LengthPercentageAuto::length(inset),
                        right: LengthPercentageAuto::length(inset),
                        top: LengthPercentageAuto::length(inset),
                        bottom: LengthPercentageAuto::length(inset),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        canvas
    }

    /// Lay out the status bar's band: zero height with nothing to say (a band
    /// kept anyway would leave a rule and a strip of empty chrome under an
    /// empty state), else `status_bar::height`.
    #[allow(
        clippy::unwrap_used,
        reason = "this taffy node is freshly built with a fixed, valid style"
    )]
    pub(super) fn build_status_node(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> NodeId {
        let height = if self.has_status() {
            ui::chrome::status_bar::height(theme)
        } else {
            0.0
        };
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(height as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn draw_collection_pager(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        bounds: Rect,
        page: usize,
        total: usize,
        previous: CollectionPageHit,
        next: CollectionPageHit,
    ) {
        let theme = ctx.theme;
        self.collection_page_hits.clear();
        let page_size = COLLECTION_PAGE_SIZE;
        let pages = total.max(1).div_ceil(page_size);
        if pages <= 1 {
            return;
        }
        let previous_enabled = page > 0;
        let next_enabled = page + 1 < pages;
        let presence = PresenceCtx::empty();
        let height = ui::widget::ControlSize::Regular.height(theme);
        let label = tr_args(
            "photos-page-of",
            &[
                ("page", ArgValue::Number((page + 1) as f64)),
                ("pages", ArgValue::Number(pages as f64)),
            ],
        );
        let label_w = ctx.measure_role(&label, ui::TextRole::Caption, None).width;
        let gap = f64::from(theme.tokens.spacing.s3);
        let mut x = bounds.x1;
        let y0 = bounds.y0 + (bounds.height() - height) / 2.0;
        let mut rects = Vec::new();
        for (hit, enabled) in [(next, next_enabled), (previous, previous_enabled)] {
            if let Some((_, button)) = self
                .collection_page_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                let label = tr(if hit == previous {
                    "photos-previous"
                } else {
                    "photos-next"
                });
                let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme);
                let rect = Rect::new(x - width, y0, x, y0 + height);
                x = rect.x0 - gap;
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(ctx.scene, ctx.transform, ctx.text, theme, &presence);
                rects.push((hit, rect, enabled));
                if hit == next {
                    x -= label_w + gap;
                }
            }
        }
        if let Some((_, next_rect, _)) = rects.first() {
            ctx.text_role_in_band(
                &label,
                ui::TextRole::Caption,
                theme.text_dim,
                next_rect.x0 - gap - label_w,
                Rect::new(bounds.x0, y0, bounds.x1, y0 + height),
                None,
            );
        }
        self.collection_page_hits.extend(rects);
    }

    /// The content column inside its gutters: the page edge of every library
    /// surface.
    pub(super) fn content_frame(&self, theme: &Theme) -> Rect {
        let rhythm = &theme.tokens.spacing.rhythm;
        let side = f64::from(rhythm.group);
        let top = f64::from(rhythm.section);
        let bounds = self.canvas_rect;
        Rect::new(
            bounds.x0 + side,
            bounds.y0 + top,
            (bounds.x1 - side).max(bounds.x0 + side),
            (bounds.y1 - top).max(bounds.y0 + top),
        )
    }

    /// The album overview: one media card per album, its cover the first
    /// photograph in it, in a grid that reflows with the width. Covers come
    /// from the bounded thumbnail cache; originals are never decoded here.
    #[expect(
        clippy::too_many_lines,
        reason = "the album grid measures, scrolls and paints its rows in one pass"
    )]
    pub(super) fn draw_albums(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.album_cards.clear();
        self.album_edit_cards.clear();
        self.album_editor_hits.clear();
        self.collection_page_hits.clear();

        let content = self.content_frame(theme);
        if self.library_albums.is_empty() {
            let title = tr("photos-albums-empty-title");
            let body = tr("photos-albums-empty-body");
            let action = tr("photos-new-album");
            self.album_editor_hits.extend(
                EmptyState::paint(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    self.canvas_rect,
                    &EmptyStateContent {
                        icon: Some(Icon::GalleryVerticalEnd),
                        illustration: None,
                        title: &title,
                        message: Some(&body),
                        action_label: Some(&action),
                        error: false,
                    },
                    self.pointer,
                )
                .map(|rect| (AlbumEditorHit::New, rect)),
            );
            return;
        }

        let rhythm = &theme.tokens.spacing.rhythm;
        let column_gap = f64::from(rhythm.section);
        let row_gap = f64::from(rhythm.group);
        let extent = ui::widget::MediaSize::Regular.extent(theme);
        let columns = ui::widget::card_columns(content.width(), extent, column_gap).max(1);
        let card_w = ui::widget::card_column_width(content.width(), columns, column_gap);
        let metas: Vec<String> = self
            .library_albums
            .iter()
            .map(|(_, _, count)| tr_count("photos-count-photos", *count))
            .collect();
        // Every album is in the grid; the grid scrolls in the column.
        let viewport = Rect::new(
            content.x0,
            self.canvas_rect.y0,
            content.x1,
            self.canvas_rect.y1,
        );
        let mut rows = Vec::new();
        let mut content_h = content.y0 - viewport.y0;
        for (row_index, row) in self.library_albums.chunks(columns).enumerate() {
            let text_h = row
                .iter()
                .enumerate()
                .map(|(column, (_, name, _))| {
                    MediaCardFace::new(ui::widget::MediaArt::Placeholder)
                        .aspect(ui::widget::MediaAspect::Square)
                        .title(name)
                        .meta(
                            metas
                                .get(row_index * columns + column)
                                .map_or("", String::as_str),
                        )
                        .text_height(text_renderer, theme, card_w)
                })
                .fold(0.0, f64::max);
            rows.push((content_h, card_w + text_h));
            content_h += card_w + text_h + row_gap;
        }
        content_h += f64::from(rhythm.section) - row_gap;
        let metrics = ScrollMetrics::new(content_h, viewport.height(), self.albums_scroll);
        self.albums_scroll = self.albums_scroll.clamp(0.0, metrics.max_scroll());
        self.albums_viewport = viewport;
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &Rect::new(
                self.canvas_rect.x0,
                viewport.y0,
                self.canvas_rect.x1,
                viewport.y1,
            ),
        );
        let mut placed = Vec::new();
        for ((top, card_h), (row_index, row)) in rows
            .iter()
            .zip(self.library_albums.chunks(columns).enumerate())
        {
            let y = viewport.y0 + top - self.albums_scroll;
            if y + card_h < viewport.y0 || y > viewport.y1 {
                continue;
            }
            for (column, (id, name, _)) in row.iter().enumerate() {
                let x = content.x0 + column as f64 * (card_w + column_gap);
                let card = Rect::new(x, y, x + card_w, y + card_h);
                let cover = self
                    .library_album_covers
                    .get(id)
                    .and_then(|covers| covers.first())
                    .and_then(|cover| self.thumbnail_for(cover));
                let art = cover.map_or(ui::widget::MediaArt::Placeholder, |image| {
                    ui::widget::MediaArt::Image(image)
                });
                let visible = card.intersect(viewport);
                MediaCardFace::new(art)
                    .aspect(ui::widget::MediaAspect::Square)
                    .title(name)
                    .meta(
                        metas
                            .get(row_index * columns + column)
                            .map_or("", String::as_str),
                    )
                    .interaction(ui::widget::Interaction {
                        hovered: visible.contains(self.pointer),
                        ..Default::default()
                    })
                    .paint(
                        &mut PaintCtx::new(scene, transform, text_renderer, theme),
                        card,
                    );
                placed.push((Some(id.clone()), visible));
            }
        }
        scene.pop_layer();
        self.album_cards.extend(placed);
        if metrics.overflows() {
            ui::widget::scrollbar::Scrollbar::vertical().draw(
                scene,
                transform,
                Self::scroll_track(viewport, theme),
                ScrollMetrics::new(content_h, viewport.height(), self.albums_scroll),
                theme,
                false,
            );
        }
    }

    /// The scroll thumb's track beside a scrolling region of the content
    /// column: in the column's trailing gutter, clear of what scrolls.
    pub(super) fn scroll_track(region: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let x0 = region.x1 + (f64::from(theme.tokens.spacing.rhythm.group) - gutter) / 2.0;
        Rect::new(x0, region.y0, x0 + gutter, region.y1)
    }

    /// Photo analysis: how much of the library is searchable, the run in
    /// progress and its controls, the local model, and the photographs that
    /// failed. Pixels stay on this device; the compact results are shared.
    #[expect(
        clippy::too_many_lines,
        reason = "the analysis page lays out its stats, run, model and failures in one pass"
    )]
    pub(super) fn draw_library_analysis(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use crate::library_analysis::LibraryAnalysisPhase;

        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.library_analysis_failure_hits.clear();

        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let (related, sibling, group) = (
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
            f64::from(rhythm.group),
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;

        // What is searchable now, ready, and waiting for its bytes.
        let (analyzed, available, awaiting) = self.library_analysis_coverage;
        let values = [
            analyzed.to_string(),
            available.to_string(),
            awaiting.to_string(),
        ];
        let labels = [
            tr("photos-analysis-searchable"),
            tr("photos-analysis-ready"),
            tr("photos-analysis-waiting"),
        ];
        let tiles = [
            StatTileFace::new(&labels[0], &values[0]),
            StatTileFace::new(&labels[1], &values[1]),
            StatTileFace::new(&labels[2], &values[2]),
        ];
        // The tiles reflow as a `StatGroup` does: all in one row where they
        // fit, else one per row, never an uneven 2 + 1.
        let tile_min = (Self::line_of(theme, ui::TextRole::Body) * 7.0).round();
        let fits = ui::widget::card_columns(frame.width(), tile_min, group);
        let columns = (1..=tiles.len().min(fits))
            .rev()
            .find(|columns| tiles.len().is_multiple_of(*columns))
            .unwrap_or(1);
        let tile_w = ui::widget::card_column_width(frame.width(), columns, group);
        let tile_h = tiles
            .iter()
            .map(|tile| tile.height(ctx.text, theme))
            .fold(0.0, f64::max);
        for (index, tile) in tiles.iter().enumerate() {
            let column = index % columns;
            let row = index / columns;
            let x = frame.x0 + column as f64 * (tile_w + group);
            let top = y + row as f64 * (tile_h + sibling);
            tile.paint(&mut ctx, Rect::new(x, top, x + tile_w, top + tile_h));
        }
        y += 3usize.div_ceil(columns) as f64 * (tile_h + sibling) - sibling + group;

        // The run.
        let phase = self.library_analysis.phase;
        let phase_label = match phase {
            LibraryAnalysisPhase::Idle => tr("photos-analysis-idle"),
            LibraryAnalysisPhase::Running => tr("photos-analysis-running"),
            LibraryAnalysisPhase::Paused => tr("photos-analysis-paused"),
            LibraryAnalysisPhase::Complete => tr("photos-analysis-finished"),
        };
        ctx.text_role(
            &phase_label,
            ui::TextRole::Heading,
            theme.text_primary,
            Point::new(frame.x0, y),
            None,
        );
        y += Self::line_of(theme, ui::TextRole::Heading) + f64::from(rhythm.tight);
        // Without a model on this device there is nothing to start; the
        // page says what it needs instead of offering a dead button.
        let has_model = self.library_analysis_capability.is_some();
        let summary = match phase {
            LibraryAnalysisPhase::Idle if has_model => tr("photos-analysis-idle-body"),
            LibraryAnalysisPhase::Idle => tr("photos-analysis-needs-model"),
            _ => tr_args(
                "photos-analysis-progress",
                &[
                    (
                        "done",
                        ArgValue::Number(self.library_analysis.completed as f64),
                    ),
                    (
                        "total",
                        ArgValue::Number(self.library_analysis.total as f64),
                    ),
                    (
                        "failed",
                        ArgValue::Number(self.library_analysis.failed as f64),
                    ),
                ],
            ),
        };
        let summary_h = ctx
            .measure_role(&summary, ui::TextRole::Body, Some(frame.width() as f32))
            .height;
        ctx.text_role(
            &summary,
            ui::TextRole::Body,
            theme.text_dim,
            Point::new(frame.x0, y),
            Some(frame.width() as f32),
        );
        y += summary_h + sibling;
        if phase == LibraryAnalysisPhase::Idle {
            self.library_analysis_progress
                .apply_layout_external(Rect::ZERO);
        } else {
            self.library_analysis_progress
                .set_progress(self.library_analysis.progress() as f32);
            let bar_h = f64::from(theme.tokens.radii.detail) * 2.0;
            self.library_analysis_progress
                .apply_layout_external(Rect::new(frame.x0, y, frame.x1, y + bar_h));
            self.library_analysis_progress
                .draw(ctx.scene, transform, ctx.text, theme, presence);
            y += bar_h + related;
            if let Some(name) = self.library_analysis.current_name.as_deref() {
                ctx.text_role(
                    name,
                    ui::TextRole::Caption,
                    theme.text_dim,
                    Point::new(frame.x0, y),
                    Some(frame.width() as f32),
                );
                y += Self::line_of(theme, ui::TextRole::Caption);
            }
            y += related;
        }
        let visible_hit = match phase {
            LibraryAnalysisPhase::Running => LibraryAnalysisHit::Pause,
            LibraryAnalysisPhase::Paused => LibraryAnalysisHit::Resume,
            _ => LibraryAnalysisHit::Start,
        };
        let button_h = ui::widget::ControlSize::Regular.height(theme);
        let mut x = frame.x0;
        for (hit, button) in &mut self.library_analysis_buttons {
            let visible = (*hit == visible_hit && (has_model || *hit != LibraryAnalysisHit::Start))
                || (*hit == LibraryAnalysisHit::RetryFailures
                    && self.library_analysis.has_failures()
                    && phase != LibraryAnalysisPhase::Running);
            button.set_enabled(match *hit {
                LibraryAnalysisHit::Start => {
                    self.library_analysis_capability.is_some() && available > 0
                }
                LibraryAnalysisHit::Pause => phase == LibraryAnalysisPhase::Running,
                LibraryAnalysisHit::Resume => phase == LibraryAnalysisPhase::Paused,
                LibraryAnalysisHit::RetryFailures => self.library_analysis.has_failures(),
            });
            if visible {
                let label = tr(match hit {
                    LibraryAnalysisHit::Start => "photos-analysis-start",
                    LibraryAnalysisHit::Pause => "photos-analysis-pause",
                    LibraryAnalysisHit::Resume => "photos-analysis-resume",
                    LibraryAnalysisHit::RetryFailures => "photos-analysis-retry",
                });
                let icon = match hit {
                    LibraryAnalysisHit::Start => Icon::Sparkles,
                    LibraryAnalysisHit::Pause => Icon::Pause,
                    LibraryAnalysisHit::Resume => Icon::Play,
                    LibraryAnalysisHit::RetryFailures => Icon::Refresh,
                };
                let width = ButtonFace::new(Rect::ZERO, &label)
                    .icon(icon)
                    .intrinsic_width(ctx.text, theme);
                button.apply_layout_external(Rect::new(x, y, x + width, y + button_h));
                button.draw(ctx.scene, transform, ctx.text, theme, presence);
                x += width + related;
            } else {
                button.apply_layout_external(Rect::ZERO);
            }
        }
        if x > frame.x0 {
            y += button_h + group;
        }

        // The model that does the work.
        let model_rows: Vec<(String, String, bool)> = self
            .library_analysis_capability
            .as_ref()
            .map(|capability| {
                vec![
                    (
                        tr("photos-analysis-model"),
                        capability.model_label.clone(),
                        false,
                    ),
                    (
                        tr("photos-analysis-revision"),
                        capability.revision.chars().take(10).collect(),
                        true,
                    ),
                ]
            })
            .unwrap_or_default();
        let rows: Vec<PropertyRow<'_>> = model_rows
            .iter()
            .map(|(label, value, identifier)| {
                let row = PropertyRow::new(label, value);
                if *identifier {
                    row.with_kind(ui::TextKind::Identifier)
                } else {
                    row
                }
            })
            .collect();
        let list = PropertyListFace::new(&rows);
        let list_w = frame
            .width()
            .min(f64::from(theme.tokens.metrics.panel_width_sheet));
        let list_h = list.height(ctx.text, theme, list_w);
        if !rows.is_empty() && y + list_h <= frame.y1 {
            list.paint(
                &mut ctx,
                Rect::new(frame.x0, y, frame.x0 + list_w, y + list_h),
            );
        }
        y += list_h + group;

        // Photographs the model could not read.
        if self.library_analysis.has_failures() && y < frame.y1 {
            ctx.text_role(
                &tr("photos-analysis-needs-attention"),
                ui::TextRole::Heading,
                theme.text_primary,
                Point::new(frame.x0, y),
                None,
            );
            y += Self::line_of(theme, ui::TextRole::Heading) + related;
            let failures: Vec<_> = self
                .library_analysis
                .failures()
                .rev()
                .take(4)
                .map(|failure| {
                    (
                        failure.photo_id.clone(),
                        failure.name.clone(),
                        failure.message.clone(),
                    )
                })
                .collect();
            for (index, (photo_id, name, message)) in failures.iter().enumerate() {
                let mut face = ListRowFace::new(name)
                    .secondary(message)
                    .status(ui::widget::RowStatus::Error)
                    .disclosure();
                if index + 1 < failures.len() {
                    face = face.separator(false);
                }
                let h = face.height(ctx.text, theme);
                if y + h > frame.y1 {
                    break;
                }
                let row = Rect::new(frame.x0, y, frame.x1, y + h);
                let face = face.interaction(ui::widget::Interaction {
                    hovered: row.contains(self.pointer),
                    ..Default::default()
                });
                face.paint(&mut ctx, row);
                self.library_analysis_failure_hits
                    .push((photo_id.clone(), row));
                y += h;
            }
        }
    }

    /// The timeline: the photographs of each capture day in justified rows
    /// at their own aspect ratios under the day's heading, scrolled by the
    /// date index at the trailing edge. Only the rows in view are
    /// materialised; the rest stay reachable through search and resources.
    #[expect(
        clippy::too_many_lines,
        reason = "the timeline lays out, paints and records the hit targets of the rows in view in one pass"
    )]
    pub(super) fn draw_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.album_cards.clear();
        self.metadata_filter_cards.clear();
        self.library_page_hits.clear();
        self.library_details_cards.clear();
        self.places_hits.clear();
        self.collection_page_hits.clear();
        if self.library_items.is_empty() {
            let (title, body) = Self::library_empty_state_text();
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
            return;
        }
        let frame = self.content_frame(theme);
        let index_w = ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH;
        let top = frame.y0;
        let viewport_height = (self.canvas_rect.y1 - top).max(0.0);
        self.timeline_viewport_height = viewport_height;
        let timeline_left = frame.x0;
        let geometry = self.timeline_geometry(theme);
        let timeline_width = geometry.width;
        let anchor = self.timeline_projection.anchor_at(self.timeline_scroll_y);
        let geometry_changed = self.timeline_projection.set_geometry(geometry);
        if geometry_changed
            && let Some(anchor) = anchor.as_ref()
            && let Some(restored) = self
                .timeline_projection
                .restore_anchor(anchor, viewport_height)
        {
            self.timeline_scroll_y = restored;
        }
        self.timeline_scroll_y = self
            .timeline_projection
            .clamp_scroll(self.timeline_scroll_y, viewport_height);
        self.timeline_index_metrics = ScrollMetrics::new(
            self.timeline_projection.total_height(),
            viewport_height,
            self.timeline_scroll_y,
        );
        self.timeline_index_markers = self
            .timeline_projection
            .date_markers(viewport_height)
            .into_iter()
            .map(|(day, position, major)| {
                DateIndexMarker::new(
                    day.clone(),
                    timeline_date_label(&day),
                    position,
                    if major {
                        DateIndexMarkerKind::Major
                    } else {
                        DateIndexMarkerKind::Minor
                    },
                )
            })
            .collect();
        self.timeline_index
            .set_rect(Rect::new(frame.x1 - index_w, top, frame.x1, frame.y1));
        let visible_days = self
            .timeline_projection
            .visible_days(self.timeline_scroll_y, viewport_height);
        let photo_images: HashMap<_, _> = self
            .observed_content
            .iter()
            .flat_map(|content| content.library.photos.values())
            .map(|photo| (photo.id.as_str(), &photo.image))
            .collect();
        let visible_items: Vec<_> = self
            .timeline_projection
            .visible_photos(self.timeline_scroll_y, viewport_height)
            .into_iter()
            .filter_map(|entry| {
                let image = photo_images.get(entry.photo.id.as_str())?;
                Some((
                    entry.photo.id.clone(),
                    thumbnail_key(image),
                    entry.photo.favorite,
                    Rect::new(
                        timeline_left + entry.x,
                        top + entry.y,
                        timeline_left + entry.x + entry.width,
                        top + entry.y + entry.height,
                    ),
                ))
            })
            .collect();
        // The moving content is clipped to its region, so a fast scroll
        // never paints over the header row or the status bar.
        let clip = Rect::new(
            self.canvas_rect.x0,
            top,
            self.canvas_rect.x1,
            self.canvas_rect.y1,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        for day in visible_days {
            let heading_y = top + day.y;
            ctx.text_role(
                &timeline_date_label(&day.key),
                ui::TextRole::Subheading,
                theme.text_primary,
                Point::new(timeline_left, heading_y),
                Some(timeline_width as f32),
            );
        }
        let picking = self.selection_mode || self.album_editor;
        for (id, key, favorite, card) in &visible_items {
            let image = self.library_thumbnails.get(key);
            let art = image.map_or(
                ui::widget::MediaArt::Placeholder,
                ui::widget::MediaArt::Image,
            );
            let hovered = card.contains(self.pointer) && clip.contains(self.pointer);
            let chosen = (self.selection_mode && self.selected_photo_ids.contains(id))
                || (self.album_editor && self.album_editor_members.contains(id));
            MediaCardFace::new(art)
                .aspect(ui::widget::MediaAspect::Ratio(
                    card.width() / card.height().max(1.0),
                ))
                .interaction(ui::widget::Interaction {
                    hovered: hovered && !picking,
                    ..Default::default()
                })
                .paint(&mut ctx, *card);
            let fully_visible = card.y0 >= top && card.y1 <= self.canvas_rect.y1;
            if picking {
                Self::paint_pick_mark(&mut ctx, *card, chosen, hovered);
            } else if hovered && fully_visible {
                self.paint_card_actions(&mut ctx, id, *card, *favorite);
            }
            if fully_visible {
                if !picking {
                    let actions = card_action_controls(*card, theme);
                    self.library_details_cards
                        .push((id.clone(), actions.details));
                }
                self.library_cards.push((id.clone(), *card, *favorite));
            }
        }
        ctx.scene.pop_layer();
        if !self.album_editor && !self.metadata_editor && !self.selection_album_chooser {
            self.timeline_index.paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            );
        }
    }

    /// The timeline's row geometry in the content column under `theme`: the
    /// width beside the date index, the row height for the density, the
    /// tight gap between photographs and the height of a day's heading.
    pub(super) fn timeline_geometry(&self, theme: &Theme) -> TimelineGeometry {
        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let index_w = ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH;
        TimelineGeometry {
            width: (frame.width() - index_w - f64::from(rhythm.related)).max(1.0),
            target_height: timeline_row_height(theme, self.timeline_density),
            gap: f64::from(rhythm.tight),
            header_height: Self::line_of(theme, ui::TextRole::Subheading)
                + f64::from(rhythm.related),
            day_gap: f64::from(rhythm.group),
        }
    }

    /// While photographs are being chosen, each shows whether it is in:
    /// a check on the inverted fill when chosen, an empty ring otherwise.
    fn paint_pick_mark(ctx: &mut PaintCtx<'_>, card: Rect, chosen: bool, hovered: bool) {
        use vello::peniko::Fill;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let inset = f64::from(theme.tokens.spacing.s2);
        let side = f64::from(theme.tokens.metrics.icon_lg);
        let mark = Rect::new(
            card.x1 - inset - side,
            card.y0 + inset,
            card.x1 - inset,
            card.y0 + inset + side,
        );
        let disc = Circle::new(mark.center(), side / 2.0);
        if chosen {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.intent.accent,
                None,
                &disc,
            );
            Icon::Check.draw(
                ctx.scene,
                ctx.transform,
                mark.inset(-f64::from(theme.tokens.spacing.s1)),
                colors.intent.on_accent,
            );
        } else {
            ui::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &disc,
                ui::floating_surface::Material::Floating,
                theme,
            );
            if hovered {
                ui::widget::paint_focus_ring(ctx.scene, ctx.transform, mark, side / 2.0, theme);
            }
        }
    }

    /// The hover actions of a photograph: details at the leading corner,
    /// favourite and remove at the trailing one, each on the floating
    /// material over the picture.
    fn paint_card_actions(&self, ctx: &mut PaintCtx<'_>, id: &str, card: Rect, favorite: bool) {
        let theme = ctx.theme;
        let actions = card_action_controls(card, theme);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let confirming = self.library_remove_confirm.as_deref() == Some(id);
        let radius = f64::from(theme.tokens.radii.control);
        for group in [actions.details, actions.remove.union(actions.favorite)] {
            ui::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &RoundedRect::from_rect(group, radius),
                ui::floating_surface::Material::Floating,
                theme,
            );
        }
        let buttons = [
            (
                actions.details,
                Icon::Pencil,
                tr("photos-edit-details"),
                ButtonRole::Plain,
            ),
            (
                actions.remove,
                if confirming {
                    Icon::Check
                } else {
                    Icon::Trash2
                },
                if confirming {
                    tr("photos-confirm-remove")
                } else {
                    tr("photos-remove")
                },
                if confirming {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Plain
                },
            ),
            (
                actions.favorite,
                Icon::Star,
                if favorite {
                    tr("photos-unfavorite")
                } else {
                    tr("photos-favorite")
                },
                ButtonRole::Plain,
            ),
        ];
        for (rect, icon, name, role) in &buttons {
            let face = ButtonFace::icon_only(*rect, *icon, name).role(*role);
            let hovered = face.contains(&ink, self.pointer);
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                &ink,
                &face.hovered(hovered),
            );
        }
    }

    /// The places of the library on an offline map, with the list of places
    /// beside it on a wide window, or the located photographs as a list. The
    /// map is a bundled low-detail land silhouette, never a tile service:
    /// opening Places sends no coordinate off this device.
    #[expect(
        clippy::too_many_lines,
        reason = "the map, its markers and their hit targets share one geometry pass"
    )]
    pub(super) fn draw_places(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;
        self.places_hits.clear();
        self.collection_page_hits.clear();
        let place_clusters =
            crate::places::clusters(self.places_items.iter().map(|item| &item.photo));
        if self.places_items.is_empty() {
            let title = tr("photos-places-empty-title");
            let body = tr("photos-places-empty-body");
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.canvas_rect,
                &EmptyStateContent {
                    icon: Some(Icon::Globe),
                    illustration: None,
                    title: &title,
                    message: Some(&body),
                    action_label: None,
                    error: false,
                },
                self.pointer,
            );
            return;
        }
        if self.places_list_mode {
            let frame = self.content_frame(theme);
            self.draw_places_list(scene, transform, text_renderer, theme, frame);
            return;
        }
        let bounds = self.canvas_rect;
        let show_rail = self.places_open_key.is_none()
            && bounds.width() >= f64::from(theme.tokens.metrics.compact_breakpoint);
        let rail_w = if show_rail {
            f64::from(theme.tokens.metrics.inspector_width)
        } else {
            0.0
        };
        let map = Rect::new(bounds.x0, bounds.y0, bounds.x1 - rail_w, bounds.y1);
        self.places_map_bounds = map;
        // Arriving at Places frames every place on the map, clear of its
        // edges by a marker and its label.
        if std::mem::take(&mut self.places_fit_pending) {
            let points: Vec<(f64, f64)> = place_clusters
                .iter()
                .map(|cluster| (cluster.latitude, cluster.longitude))
                .collect();
            let margin = 3.0 * f64::from(theme.tokens.metrics.min_pointer_target);
            self.places_viewport =
                crate::places::MapViewport::fitting(&points, map.width(), map.height(), margin);
        }

        // The map is content: water on the raised well, land one step off it,
        // coasts derived from the land.
        let colors = &theme.tokens.colors;
        let water = colors.surfaces.raised;
        let land = colors.lift_on(water, 1.0);
        scene.fill(Fill::NonZero, transform, water, None, &map);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &map,
        );
        for landmass in OFFLINE_WORLD_LANDMASSES {
            let mut coast = BezPath::new();
            let mut visible_points = 0_usize;
            for &(latitude, longitude) in *landmass {
                let location = crate::model::PhotoLocation {
                    latitude,
                    longitude,
                    label: None,
                    provenance: crate::model::LocationProvenance::default(),
                };
                let Some((x, y)) =
                    self.places_viewport
                        .project(&location, map.width(), map.height())
                else {
                    continue;
                };
                let point = (map.x0 + x, map.y0 + y);
                if visible_points == 0 {
                    coast.move_to(point);
                } else {
                    coast.line_to(point);
                }
                visible_points += 1;
            }
            if visible_points >= 3 {
                coast.close_path();
                scene.fill(Fill::NonZero, transform, land, None, &coast);
                scene.stroke(
                    &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
                    transform,
                    surface::edge_on(land),
                    None,
                    &coast,
                );
            }
        }
        let camera_feedback = self.places_camera_feedback();
        let marker_r = f64::from(theme.tokens.metrics.min_pointer_target) * 0.75;
        let ring = f64::from(theme.tokens.spacing.s1) * 0.5;
        for cluster in &place_clusters {
            let location = crate::model::PhotoLocation {
                latitude: cluster.latitude,
                longitude: cluster.longitude,
                label: Some(cluster.label.clone()),
                provenance: crate::model::LocationProvenance::default(),
            };
            let Some((px, py)) = self
                .places_viewport
                .project(&location, map.width(), map.height())
            else {
                continue;
            };
            let point = Point::new(map.x0 + px, map.y0 + py);
            if !map.contains(point) {
                continue;
            }
            let radius = marker_r * (1.0 + camera_feedback * 0.10);
            // A place is its photograph, ringed in the ground so it reads
            // against land and water alike.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.content,
                None,
                &Circle::new(point, radius),
            );
            let inner = Circle::new(point, radius - ring * 2.0);
            if let Some(image) = cluster
                .photo_ids
                .first()
                .and_then(|photo_id| self.thumbnail_for(photo_id))
            {
                let side = inner.radius * 2.0;
                let scale = (side / f64::from(image.width)).max(side / f64::from(image.height));
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &inner,
                );
                scene.draw_image(
                    image,
                    transform
                        * Affine::translate((
                            point.x - f64::from(image.width) * scale * 0.5,
                            point.y - f64::from(image.height) * scale * 0.5,
                        ))
                        * Affine::scale(scale),
                );
                scene.pop_layer();
            } else {
                scene.fill(Fill::NonZero, transform, land, None, &inner);
            }
            let count = cluster.count().to_string();
            let badge_w = ui::widget::BadgeFace::intrinsic_width(&count, text_renderer, theme);
            let badge_h = ui::widget::BadgeFace::height(theme);
            let badge = Rect::new(
                point.x + radius * 0.5,
                point.y - radius - badge_h * 0.25,
                point.x + radius * 0.5 + badge_w,
                point.y - radius + badge_h * 0.75,
            );
            ui::widget::BadgeFace::new(badge, &count).paint(scene, transform, text_renderer, theme);
            let hit = Rect::new(
                point.x - radius,
                point.y - radius,
                point.x + radius,
                point.y + radius,
            );
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), hit));
            if !show_rail || map.width() >= f64::from(theme.tokens.metrics.panel_width_sheet) {
                let label_w =
                    ui::widget::BadgeFace::intrinsic_width(&cluster.label, text_renderer, theme);
                let label_x =
                    (point.x - label_w * 0.5).clamp(map.x0, (map.x1 - label_w).max(map.x0));
                let label_y = point.y + radius + f64::from(theme.tokens.spacing.s1);
                ui::widget::BadgeFace::new(
                    Rect::new(label_x, label_y, label_x + label_w, label_y + badge_h),
                    &cluster.label,
                )
                .paint(scene, transform, text_renderer, theme);
            }
        }
        scene.pop_layer();
        if show_rail {
            let rail = Rect::new(map.x1, bounds.y0, bounds.x1, bounds.y1);
            self.draw_places_overview_rail(
                scene,
                transform,
                text_renderer,
                theme,
                rail,
                &place_clusters,
            );
        }
    }

    /// The places beside the map, one row each, so the next decision is a
    /// place to open rather than a marker to find.
    fn draw_places_overview_rail(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        clusters: &[crate::places::PlaceCluster],
    ) {
        let inset = f64::from(theme.tokens.spacing.rhythm.section);
        let frame = rail.inset(-inset);
        let counts: Vec<String> = clusters
            .iter()
            .map(|cluster| tr_count("photos-count-photos", cluster.count()))
            .collect();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;
        for (index, (cluster, count)) in clusters.iter().zip(&counts).enumerate() {
            let art = cluster
                .photo_ids
                .first()
                .and_then(|id| self.thumbnail_for(id));
            let mut face = ListRowFace::new(&cluster.label)
                .secondary(count)
                .disclosure();
            face = match art {
                Some(image) => face.leading(ui::widget::RowLeading::Avatar(
                    ui::widget::AvatarFace::new(&cluster.label, &cluster.key)
                        .square()
                        .image(Some(image)),
                )),
                None => face.leading(ui::widget::RowLeading::Icon(Icon::Globe)),
            };
            if index + 1 < clusters.len() {
                face = face.separator(false);
            }
            let h = face.height(ctx.text, theme);
            if y + h > frame.y1 {
                break;
            }
            let row = Rect::new(frame.x0, y, frame.x1, y + h);
            let face = face.interaction(ui::widget::Interaction {
                hovered: row.contains(self.pointer),
                ..Default::default()
            });
            face.paint(&mut ctx, row);
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), row));
            y += h;
        }
    }

    /// The located photographs as a list, a page at a time.
    fn draw_places_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: Rect,
    ) {
        let items: Vec<PlaceItem> = self
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
            .cloned()
            .collect();
        let rows: Vec<(String, String, String, String)> = items
            .iter()
            .skip(self.places_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
            .map(|item| {
                (
                    item.photo.id.clone(),
                    item.photo.image.name.clone(),
                    item.photo
                        .location
                        .as_ref()
                        .map_or_else(String::new, crate::model::PhotoLocation::display_name),
                    item.photo
                        .captured_at
                        .as_deref()
                        .map(timeline_date_label)
                        .unwrap_or_default(),
                )
            })
            .collect();
        let pager_h = ui::widget::ControlSize::Regular.height(theme);
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;
        for (index, (id, name, place, date)) in rows.iter().enumerate() {
            let mut face = ListRowFace::new(name).secondary(place).meta(date);
            if let Some(image) = self.thumbnail_for(id) {
                face = face.leading(ui::widget::RowLeading::Avatar(
                    ui::widget::AvatarFace::new(name, id)
                        .square()
                        .image(Some(image)),
                ));
            }
            if index + 1 < rows.len() {
                face = face.separator(false);
            }
            let h = face.height(ctx.text, theme);
            if y + h > frame.y1 - pager_h {
                break;
            }
            let row = Rect::new(frame.x0, y, frame.x1, y + h);
            let face = face.interaction(ui::widget::Interaction {
                hovered: row.contains(self.pointer),
                ..Default::default()
            });
            face.paint(&mut ctx, row);
            self.places_hits.push((PlacesHit::Photo(id.clone()), row));
            y += h;
        }
        self.draw_collection_pager(
            &mut ctx,
            Rect::new(frame.x0, frame.y1 - pager_h, frame.x1, frame.y1),
            self.places_page,
            items.len(),
            CollectionPageHit::PlacesPrevious,
            CollectionPageHit::PlacesNext,
        );
    }

    /// The album picker for a selection: a modal sheet over the window, one
    /// row per album, the chosen one lifted. Painted in the top layer.
    #[expect(
        clippy::too_many_lines,
        reason = "the sheet measures, lays out and paints its rows and paged actions in one pass"
    )]
    pub(super) fn draw_selection_destination_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.selection_destination_hits.clear();
        self.selection_hits.clear();
        if !self.library_mode || !self.selection_album_chooser {
            return;
        }
        let apply_enabled = self.selection_destination_album.is_some();
        let window = presence.viewport.get().unwrap_or(self.rect);
        let max_width = f64::from(theme.tokens.metrics.panel_width_sheet);

        let visible_albums: Vec<(String, String, String)> = self
            .library_albums
            .iter()
            .skip(self.selection_album_page * SELECTION_ALBUMS_PER_PAGE)
            .take(SELECTION_ALBUMS_PER_PAGE)
            .map(|(id, name, count)| {
                (
                    id.clone(),
                    name.clone(),
                    tr_count("photos-count-photos", *count),
                )
            })
            .collect();
        let faces: Vec<ListRowFace<'_>> = visible_albums
            .iter()
            .enumerate()
            .map(|(index, (id, name, count))| {
                let chosen = self.selection_destination_album.as_deref() == Some(id.as_str());
                let next_chosen = visible_albums.get(index + 1).is_some_and(|(next, _, _)| {
                    self.selection_destination_album.as_deref() == Some(next.as_str())
                });
                let face = ListRowFace::new(name)
                    .leading(ui::widget::RowLeading::Icon(Icon::GalleryVerticalEnd))
                    .meta(count)
                    .selected(chosen);
                if index + 1 < visible_albums.len() {
                    face.separator(next_chosen)
                } else {
                    face
                }
            })
            .collect();
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let heights: Vec<f64> = faces
            .iter()
            .map(|face| face.height(text_renderer, &sheet))
            .collect();
        let body_h: f64 = heights.iter().sum();
        let (cancel, choose, sheet_title) = (
            tr("photos-cancel"),
            tr("photos-choose-album"),
            tr("photos-add-to-album"),
        );
        let previous = tr("photos-a11y-previous-albums");
        let next = tr("photos-a11y-more-albums");
        let base_actions = [
            ModalAction::secondary(&previous).enabled(self.selection_album_page > 0),
            ModalAction::secondary(&next).enabled(
                (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                    < self.library_albums.len(),
            ),
            ModalAction::secondary(&cancel),
            ModalAction::primary(&choose).enabled(apply_enabled),
        ];
        let base_spec = ModalSpec::new(&sheet_title)
            .actions(&base_actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(false);
        let layout = ui::widget::modal_dialog::layout(window, &base_spec, text_renderer, theme);
        let actions: Vec<ModalAction<'_>> = base_actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                action.hovered(
                    layout
                        .actions
                        .get(index)
                        .is_some_and(|rect| rect.contains(self.pointer)),
                )
            })
            .collect();
        let spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(false)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ui::widget::modal_dialog::paint_backdrop(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let scroll = &mut self.selection_album_scroll;
        scroll.set_viewport(layout.body, theme);
        scroll.set_content_size(layout.body.width(), body_h);
        let shown = scroll.content_rect();
        let visible = scroll.visible_content();
        let to_window = Vec2::new(shown.x0, shown.y0) - scroll.offset();
        let pointer = shown
            .contains(self.pointer)
            .then(|| scroll.to_content(self.pointer));
        let hits = &mut self.selection_destination_hits;
        scroll.paint(scene, transform, &sheet, |scene, content| {
            let mut ctx = PaintCtx::new(scene, content, text_renderer, &sheet);
            let mut y = 0.0;
            for ((face, h), (id, _, _)) in faces.iter().zip(&heights).zip(&visible_albums) {
                let row = Rect::new(0.0, y, shown.width(), y + h);
                if row.y1 >= visible.y0 && row.y0 <= visible.y1 {
                    let face = face.clone().interaction(ui::widget::Interaction {
                        hovered: pointer.is_some_and(|point| row.contains(point)),
                        ..Default::default()
                    });
                    face.paint(&mut ctx, row);
                }
                hits.push((id.clone(), row + to_window));
                y += h;
            }
        });
        ui::widget::modal_dialog::paint_actions(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let mut rects = layout.actions.iter().copied();
        self.selection_hits.push((
            SelectionHit::PreviousDestinationPage,
            rects.next().unwrap_or(Rect::ZERO),
        ));
        self.selection_hits.push((
            SelectionHit::NextDestinationPage,
            rects.next().unwrap_or(Rect::ZERO),
        ));
        let cancel_rect = rects.next().unwrap_or(Rect::ZERO);
        let apply_rect = rects.next().unwrap_or(Rect::ZERO);
        self.selection_hits
            .push((SelectionHit::CancelDestinationChooser, cancel_rect));
        if let Some(close) = layout.close {
            self.selection_hits
                .push((SelectionHit::CloseDestinationChooser, close));
        }
        if apply_enabled {
            self.selection_hits
                .push((SelectionHit::ApplyDestination, apply_rect));
        }
    }
}
