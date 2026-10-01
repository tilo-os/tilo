//! Offline-map download and cache-management panel.
//!
//! This component owns the panel's transient UI state, widget event routing,
//! hit-testing, and drawing. The map app remains responsible only for effects:
//! starting/deleting tile downloads and recording collaborative region rows.

use ui::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Autocomplete, AutocompleteOutcome, Button, ButtonFace, Suggestion, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::draw;
use crate::geo;
use crate::model::DownloadedRegion;
use crate::net::RegionHit;
use crate::tiles::{DownloadProgress, GeoBounds, TileLayer};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(280);

/// Why the Download button is inert: the OpenStreetMap tile server's usage
/// policy forbids fetching tiles nobody is viewing.
pub(crate) const OFFLINE_AREAS_REFUSED: &str =
    "OpenStreetMap's tile server doesn't allow saving areas";
const MAX_EXTRA_ZOOM: u8 = 6;

/// Effect requested by a pointer interaction inside the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OfflineAction {
    Close,
    ZoomDelta(i8),
    Download,
    DeleteRegion(String),
    ClearCache,
    Consume,
}

/// Fully resolved footprint requested by the Download button.
#[derive(Clone, Debug)]
pub(crate) struct DownloadRequest {
    pub bounds: GeoBounds,
    pub z0: u8,
    pub z1: u8,
    pub name: Option<String>,
}

/// Inputs from the map and tile layer needed to render one panel frame.
#[derive(Clone, Copy)]
pub(crate) struct OfflineFrame<'a> {
    pub viewport: Rect,
    pub viewport_bounds: GeoBounds,
    pub viewport_zoom: u8,
    pub regions: &'a [DownloadedRegion],
    pub progress: DownloadProgress,
    /// Whether the tile server permits saving areas; the Download button is
    /// inert and says why when it does not.
    pub downloads_allowed: bool,
}

/// Transient state for the offline-map overlay.
pub(crate) struct OfflinePanel {
    open: bool,
    zoom_extra: u8,
    cache_bytes: u64,
    input: Autocomplete,
    results: Vec<RegionHit>,
    query: String,
    debounce: Option<Instant>,
    pick: Option<RegionHit>,
    field_rect: Option<Rect>,
    targets: Vec<(Rect, OfflineAction)>,
}

impl OfflinePanel {
    pub fn new(cache_bytes: u64) -> Self {
        Self {
            open: false,
            zoom_extra: 2,
            cache_bytes,
            input: Autocomplete::new()
                .with_id("maps_download_search")
                .with_placeholder("Search a city or place to download")
                .with_font_size(14.0),
            results: Vec::new(),
            query: String::new(),
            debounce: None,
            pick: None,
            field_rect: None,
            targets: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn modal_capture(&self) -> Option<Rect> {
        self.open
            .then(|| self.targets.first().map(|(rect, _)| *rect))
            .flatten()
    }

    pub fn open(&mut self, cache_bytes: u64) {
        self.open = true;
        self.cache_bytes = cache_bytes;
        self.reset_search();
        self.input.begin_edit();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.reset_search();
        self.targets.clear();
    }

    pub fn set_cache_bytes(&mut self, bytes: u64) {
        self.cache_bytes = bytes;
    }

    pub fn set_demo(&mut self, cache_bytes: u64, query: &str, results: Vec<RegionHit>) {
        self.open = true;
        self.zoom_extra = 2;
        self.cache_bytes = cache_bytes;
        self.input.set_text(query);
        self.input.begin_edit();
        self.query = query.to_string();
        self.results = results;
        self.input
            .set_suggestions_open(Self::suggestions(&self.results));
    }

    pub fn query_matches(&self, query: &str) -> bool {
        self.open && query.trim() == self.query.trim()
    }

    pub fn adopt_results(&mut self, results: Result<Vec<RegionHit>, String>) {
        if let Ok(hits) = results {
            self.input.set_suggestions_open(Self::suggestions(&hits));
            self.results = hits;
        } else {
            self.results.clear();
            self.input.set_suggestions_open(Vec::new());
        }
    }

    /// When the typed city settles into a live geocode.
    pub const fn query_deadline(&self) -> Option<Instant> {
        self.debounce
    }

    /// Return the settled query exactly once when live geocoding should begin.
    pub fn take_due_query(&mut self, now: Instant) -> Option<String> {
        let deadline = self.debounce?;
        if now < deadline {
            return None;
        }
        self.debounce = None;
        let query = self.query.trim().to_string();
        (!query.is_empty()).then_some(query)
    }

    /// Route an event to the shared autocomplete field.
    pub fn route_field_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        mouse: Point,
    ) -> bool {
        if let (
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            },
            Some(field),
        ) = (event, self.field_rect)
            && field.contains(mouse)
            && !self.input.is_editing()
        {
            self.input.begin_edit();
        }
        let mut doc = AutoCommit::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return false;
        };
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        self.input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: scale,
                modifiers: &modifiers,
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        self.reconcile_field();
        handled
    }

    /// Hit-test the panel's last drawn frame, with the topmost target winning.
    pub fn action_at(&self, point: Point) -> Option<OfflineAction> {
        self.targets
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, action)| action.clone())
    }

    /// Semantics for the modal overlay. While this is open the map behind it
    /// must disappear from the accessibility tree just as it does from pointer
    /// hit testing; otherwise a screen-reader user can accidentally operate
    /// obscured controls.
    pub fn accessibility(&self) -> Option<AccessNode> {
        self.open.then(|| {
            let children = self
                .accessibility_targets()
                .into_iter()
                .map(|(rect, _action, label, target)| {
                    let role = if target == "maps.offline.search" {
                        Role::TextInput
                    } else {
                        Role::Button
                    };
                    AccessNode::new(role)
                        .with_name(label)
                        .with_action_target(target)
                        .with_bounds(rect)
                        .with_state(AccessState::new().focusable(true))
                })
                .collect();
            AccessNode::new(Role::Dialog)
                .with_name("Offline maps")
                .with_state(AccessState::new().modal(true))
                .with_children(children)
        })
    }

    pub fn accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.accessibility_targets()
            .get(*index)
            .map(|(rect, ..)| *rect)
    }

    pub fn semantic_action(&self, target: &str) -> Option<OfflineAction> {
        self.accessibility_targets()
            .into_iter()
            .find_map(|(_, action, _, candidate)| (candidate == target).then_some(action))
    }

    fn accessibility_targets(&self) -> Vec<(Rect, OfflineAction, String, String)> {
        let mut targets = Vec::new();
        if let Some(field) = self.field_rect.filter(|rect| rect.area() > 0.0) {
            targets.push((
                field,
                OfflineAction::Consume,
                "Area to download".to_string(),
                "maps.offline.search".to_string(),
            ));
        }
        // The first Close target is the full scrim. Expose the smaller,
        // explicit close control instead, and deduplicate repeated commands.
        let close = self
            .targets
            .iter()
            .filter(|(rect, action)| *action == OfflineAction::Close && rect.area() > 0.0)
            .min_by(|(a, _), (b, _)| a.area().total_cmp(&b.area()))
            .map(|(rect, _)| *rect);
        if let Some(rect) = close {
            targets.push((
                rect,
                OfflineAction::Close,
                "Close offline maps".to_string(),
                "maps.offline.close".to_string(),
            ));
        }
        for (rect, action) in &self.targets {
            let (label, target) = match action {
                OfflineAction::ZoomDelta(-1) => (
                    "Decrease download detail",
                    "maps.offline.zoom-out".to_string(),
                ),
                OfflineAction::ZoomDelta(1) => (
                    "Increase download detail",
                    "maps.offline.zoom-in".to_string(),
                ),
                OfflineAction::Download => (
                    "Download selected area",
                    "maps.offline.download".to_string(),
                ),
                OfflineAction::DeleteRegion(id) => (
                    "Delete downloaded area",
                    format!("maps.offline.region:{id}:delete"),
                ),
                OfflineAction::ClearCache => {
                    ("Clear offline map cache", "maps.offline.clear".to_string())
                }
                OfflineAction::Close | OfflineAction::Consume | OfflineAction::ZoomDelta(_) => {
                    continue;
                }
            };
            if rect.area() > 0.0 && !targets.iter().any(|(_, _, _, seen)| seen == &target) {
                targets.push((*rect, action.clone(), label.to_string(), target));
            }
        }
        targets
    }

    pub fn adjust_zoom(&mut self, delta: i8) {
        let value = i16::from(self.zoom_extra) + i16::from(delta);
        self.zoom_extra = value.clamp(0, i16::from(MAX_EXTRA_ZOOM)) as u8;
    }

    pub fn download_request(
        &self,
        viewport_bounds: GeoBounds,
        viewport_zoom: u8,
        viewport: Rect,
    ) -> DownloadRequest {
        let (bounds, z0, name) = self.pick.as_ref().map_or_else(
            || (viewport_bounds, viewport_zoom, None),
            |pick| {
                let bounds = GeoBounds::new(pick.sw, pick.ne);
                let z0 = region_base_zoom(pick.sw, pick.ne, viewport);
                (bounds, z0, Some(pick.name.clone()))
            },
        );
        DownloadRequest {
            bounds,
            z0,
            z1: (z0 + self.zoom_extra).min(19),
            name,
        }
    }

    fn suggestions(hits: &[RegionHit]) -> Vec<Suggestion> {
        hits.iter()
            .map(|hit| {
                let suggestion = Suggestion::new(hit.name.clone());
                if hit.context.is_empty() {
                    suggestion
                } else {
                    suggestion.with_detail(hit.context.clone())
                }
            })
            .collect()
    }

    fn reconcile_field(&mut self) {
        let text = self.input.text().to_string();
        if text != self.query {
            text.clone_into(&mut self.query);
            self.pick = None;
            if text.trim().is_empty() {
                self.debounce = None;
                self.results.clear();
                self.input.set_suggestions_open(Vec::new());
            } else {
                self.debounce = Some(ui::time::now() + SEARCH_DEBOUNCE);
            }
        }
        match self.input.take_outcome() {
            AutocompleteOutcome::Accepted(value) => {
                if let Some(hit) = self.results.iter().find(|hit| hit.name == value).cloned() {
                    self.input.set_text(&hit.name);
                    hit.name.clone_into(&mut self.query);
                    self.pick = Some(hit);
                }
                self.input.close();
                self.debounce = None;
            }
            AutocompleteOutcome::Cancelled => self.input.close(),
            AutocompleteOutcome::None => {}
        }
    }

    fn reset_search(&mut self) {
        self.input.set_text("");
        self.input.close();
        self.input.set_suggestions(Vec::new());
        self.query.clear();
        self.results.clear();
        self.debounce = None;
        self.pick = None;
        self.field_rect = None;
    }

    #[cfg(test)]
    pub fn is_editing(&self) -> bool {
        self.input.is_editing()
    }

    #[cfg(test)]
    pub fn select_result(&mut self, index: usize) -> Option<RegionHit> {
        let hit = self.results.get(index)?.clone();
        self.input.set_text(&hit.name);
        hit.name.clone_into(&mut self.query);
        self.pick = Some(hit.clone());
        Some(hit)
    }

    #[cfg(test)]
    pub fn zoom_extra(&self) -> u8 {
        self.zoom_extra
    }

    #[cfg(test)]
    pub fn cache_bytes(&self) -> u64 {
        self.cache_bytes
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixed overlay is one top-to-bottom immediate-mode draw pass; its state, hit \
                  regions, and visuals remain together in this component"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: OfflineFrame<'_>,
    ) {
        const MAX_ROWS: usize = 4;
        self.targets.clear();
        let colors = &theme.tokens.colors;
        // One rhythm from the theme: an `s6` gutter, `s2` between a label
        // and its control or between related rows, `s4` between sections,
        // `s3` between the two action buttons.
        let sp = &theme.tokens.spacing;
        let (s2, s3, s4, gutter) = (
            f64::from(sp.s2),
            f64::from(sp.s3),
            f64::from(sp.s4),
            f64::from(sp.s6),
        );
        let scale = text.text_scale();
        let line = |size: f32| ui::text_metrics::line_height_px(size, scale);
        let (title_h, label_h, body_h, small_h) = (line(18.0), line(13.0), line(12.0), line(11.5));
        let field_h = 36.0_f64.max(label_h + 12.0);
        let stepper_h = 26.0_f64.max(line(18.0) + 8.0);
        let bar_h = 8.0;
        let row_h = 34.0_f64.max(line(13.0) + line(11.0) + 8.0);
        let button_h = 38.0_f64.max(label_h + 12.0);
        draw::fill_rect(
            scene,
            transform,
            frame.viewport,
            draw::alpha(Color::BLACK, 0.45),
        );
        self.targets.push((frame.viewport, OfflineAction::Close));

        let shown = frame.regions.len().min(MAX_ROWS);
        let overflow = frame.regions.len() > shown;
        let list_h = if frame.regions.is_empty() {
            small_h
        } else {
            shown as f64 * (row_h + s2) - s2 + if overflow { s2 + small_h } else { 0.0 }
        };
        // The card is exactly the stack it holds — the same sums the painter
        // advances by below — so nothing is clipped or left as dead space.
        let stack_h = gutter
            + title_h
            + s2
            + label_h
            + s2
            + field_h
            + s2
            + label_h
            + s4
            + stepper_h
            + s2
            + body_h
            + s2
            + bar_h
            + s2
            + body_h
            + s4
            + label_h
            + s2
            + list_h
            + s4
            + button_h
            + gutter;
        let height = stack_h.ceil().min(frame.viewport.height() - gutter);
        let width = 440.0_f64.min(frame.viewport.width() - 2.0 * gutter);
        let center = frame.viewport.center();
        let card = Rect::new(
            center.x - width / 2.0,
            center.y - height / 2.0,
            center.x + width / 2.0,
            center.y + height / 2.0,
        );
        draw::card(scene, transform, theme, card, 16.0, colors.surfaces.raised);
        self.targets.push((card, OfflineAction::Consume));

        let left = card.x0 + gutter;
        let right = card.x1 - gutter;
        let mut y = card.y0 + gutter;
        PaintCtx::new(scene, transform, text, theme).text(
            "Offline maps",
            &TextStyle::new(18.0).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(left, y),
            None,
        );
        // Close (×): a 26 px target centred on the title line.
        let close = Rect::new(
            right - stepper_h,
            y + (title_h - stepper_h) / 2.0,
            right,
            y + (title_h + stepper_h) / 2.0,
        );
        draw::text_center(
            &mut PaintCtx::new(scene, transform, text, theme),
            "✕",
            Point::new(
                close.center().x,
                vmid_scaled(close.y0, stepper_h, 14.0, f64::from(scale)),
            ),
            14.0,
            theme.text_dim,
        );
        self.targets.push((close, OfflineAction::Close));
        y += title_h + s2;

        PaintCtx::new(scene, transform, text, theme).text(
            "Area to download",
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(left, y),
            None,
        );
        y += label_h + s2;
        let field = Rect::new(left, y, right, y + field_h);
        self.field_rect = Some(field);
        self.input.apply_layout_external(field);
        self.input
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
        y += field_h + s2;

        let request =
            self.download_request(frame.viewport_bounds, frame.viewport_zoom, frame.viewport);
        if let Some(pick) = &self.pick {
            PaintCtx::new(scene, transform, text, theme).text(
                &pick.name,
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                theme.accent_primary,
                Point::new(left, y),
                None,
            );
            if !pick.context.is_empty() {
                let name_width = draw::measure_w(text, &pick.name, 13.0) + 10.0;
                draw::text_clip(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    &pick.context,
                    Point::new(left + name_width, y),
                    11.5,
                    theme.text_dim,
                    (right - left - name_width).max(20.0),
                );
            }
        } else {
            draw::text_clip(
                &mut PaintCtx::new(scene, transform, text, theme),
                "Search a city or place above, or download the current view.",
                Point::new(left, y),
                11.5,
                theme.text_dim,
                right - left,
            );
        }
        y += label_h + s4;

        // Detail row: the label sits on the stepper's centre line; the two
        // stepper buttons are the shared face with the range between them.
        let row = Rect::new(left, y, right, y + stepper_h);
        PaintCtx::new(scene, transform, text, theme).text(
            "Detail (zoom levels)",
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(left, vmid_scaled(row.y0, stepper_h, 13.0, f64::from(scale))),
            None,
        );
        let range_w = 58.0;
        let plus = Rect::new(right - stepper_h, row.y0, right, row.y1);
        let minus = Rect::new(
            plus.x0 - range_w - stepper_h,
            row.y0,
            plus.x0 - range_w,
            row.y1,
        );
        for (button, label, name) in [(minus, "–", "Less detail"), (plus, "+", "More detail")] {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(button, label)
                    .radius(7.0)
                    .text_size(18.0)
                    .accessible_name(name),
            );
        }
        draw::text_center(
            &mut PaintCtx::new(scene, transform, text, theme),
            &format!("z{}–z{}", request.z0, request.z1),
            Point::new(
                (minus.x1 + plus.x0) / 2.0,
                vmid_scaled(row.y0, stepper_h, 13.0, f64::from(scale)),
            ),
            13.0,
            theme.accent_primary,
        );
        self.targets.push((minus, OfflineAction::ZoomDelta(-1)));
        self.targets.push((plus, OfflineAction::ZoomDelta(1)));
        y += stepper_h + s2;

        let estimate = TileLayer::estimate_download(request.bounds, request.z0, request.z1);
        let readout = if !frame.downloads_allowed {
            OFFLINE_AREAS_REFUSED.to_string()
        } else if frame.progress.is_active() {
            format!(
                "Downloading {}/{} tiles…",
                frame.progress.done, frame.progress.total
            )
        } else if frame.progress.total > 0 && frame.progress.done >= frame.progress.total {
            let failed = if frame.progress.failed > 0 {
                format!(" ({} failed)", frame.progress.failed)
            } else {
                String::new()
            };
            format!("Downloaded {} tiles{failed}", frame.progress.total)
        } else {
            format!(
                "≈ {estimate} tiles ({})",
                ui::format::bytes_si(u64::from(estimate) * 18_000)
            )
        };
        let readout = text.elide_to_width(&readout, 12.0, ui::num::f32_of_f64(right - left));
        draw::text(
            scene,
            text,
            transform,
            &readout,
            left,
            y,
            12.0,
            theme.text_dim,
        );
        y += body_h + s2;
        let bar = Rect::new(left, y, right, y + bar_h);
        // The progress track is the base tier with its derived edge, so an
        // empty bar still reads as a bar.
        draw::edged(scene, transform, bar, bar_h / 2.0, colors.surfaces.base);
        let fraction = frame.progress.fraction();
        if fraction > 0.0 {
            draw::fill_round(
                scene,
                transform,
                Rect::new(
                    bar.x0,
                    bar.y0,
                    bar.x0 + (bar.width() * fraction).max(4.0),
                    bar.y1,
                ),
                4.0,
                theme.accent_primary,
            );
        }
        y += bar_h + s2;
        draw::text(
            scene,
            text,
            transform,
            &format!("Cached on disk: {}", ui::format::bytes_si(self.cache_bytes)),
            left,
            y,
            12.0,
            theme.text_primary,
        );
        y += body_h + s4;

        PaintCtx::new(scene, transform, text, theme).text(
            "Downloaded areas",
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(left, y),
            None,
        );
        y += label_h + s2;
        if frame.regions.is_empty() {
            draw::text(
                scene,
                text,
                transform,
                "No saved areas yet — search a place above to download it.",
                left,
                y,
                11.5,
                theme.text_dim,
            );
            y += small_h;
        } else {
            let (name_h, meta_h) = (line(13.0), line(11.0));
            for (i, region) in frame.regions.iter().take(shown).enumerate() {
                if i > 0 {
                    y += s2;
                }
                let row = Rect::new(left, y, right, y + row_h);
                // Each saved area is the base tier in the card with its
                // derived edge; its text block centres on the row.
                draw::edged(scene, transform, row, 8.0, colors.surfaces.base);
                let trash = Rect::new(
                    row.x1 - s2 - stepper_h,
                    row.center().y - stepper_h / 2.0,
                    row.x1 - s2,
                    row.center().y + stepper_h / 2.0,
                );
                let available = (trash.x0 - row.x0 - s3 - s2).max(40.0);
                let name_y = row.y0 + (row_h - name_h - meta_h) / 2.0;
                draw::text_clip(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    &region.name,
                    Point::new(row.x0 + s3, name_y),
                    13.0,
                    theme.text_primary,
                    available,
                );
                draw::text_clip(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    &format!("z{}–z{} · {} tiles", region.z0, region.z1, region.tiles),
                    Point::new(row.x0 + s3, name_y + name_h),
                    11.0,
                    theme.text_dim,
                    available,
                );
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(trash, "")
                        .radius(7.0)
                        .accessible_name("Remove downloaded area"),
                );
                draw_trash(scene, transform, trash, colors.semantic.danger);
                self.targets
                    .push((trash, OfflineAction::DeleteRegion(region.id.clone())));
                y += row_h;
            }
            if overflow {
                y += s2;
                draw::text(
                    scene,
                    text,
                    transform,
                    &format!("+{} more", frame.regions.len() - shown),
                    left,
                    y,
                    11.0,
                    theme.text_dim,
                );
                y += small_h;
            }
        }
        y += s4;

        let button_width = (right - left - s3) / 2.0;
        let download = Rect::new(left, y, left + button_width, y + button_h);
        let clear = Rect::new(left + button_width + s3, y, right, y + button_h);
        let label = self.pick.as_ref().map_or_else(
            || "Download current view".to_string(),
            |pick| format!("Download {}", pick.name),
        );
        let label = text.elide_to_width(&label, 13.0, ui::num::f32_of_f64(button_width - 2.0 * s3));
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(download, &label)
                .primary()
                .disabled(!frame.downloads_allowed)
                .radius(9.0)
                .text_size(13.0),
        );
        self.targets.push((
            download,
            if frame.downloads_allowed {
                OfflineAction::Download
            } else {
                OfflineAction::Consume
            },
        ));
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(clear, "Clear all")
                .danger_outline()
                .radius(9.0)
                .text_size(13.0),
        );

        self.targets.push((clear, OfflineAction::ClearCache));
        self.input
            .draw_overlay(scene, transform, text, theme, &PresenceCtx::empty());
    }
}

fn region_base_zoom(sw: crate::model::LatLon, ne: crate::model::LatLon, viewport: Rect) -> u8 {
    let corners = [
        sw,
        ne,
        crate::model::LatLon::new(sw.lat, ne.lon),
        crate::model::LatLon::new(ne.lat, sw.lon),
    ];
    let zoom = geo::fit_bounds(
        &corners,
        viewport.width().max(400.0),
        viewport.height().max(300.0),
        40.0,
    )
    .map_or(12.0, |(_, zoom)| zoom);
    TileLayer::tile_zoom(zoom)
}

fn draw_trash(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    let center = rect.center();
    for (start, end) in [
        (
            Point::new(center.x - 5.0, center.y - 4.0),
            Point::new(center.x + 5.0, center.y - 4.0),
        ),
        (
            Point::new(center.x - 2.0, center.y - 6.5),
            Point::new(center.x + 2.0, center.y - 6.5),
        ),
        (
            Point::new(center.x - 3.6, center.y - 3.0),
            Point::new(center.x - 2.8, center.y + 5.0),
        ),
        (
            Point::new(center.x + 3.6, center.y - 3.0),
            Point::new(center.x + 2.8, center.y + 5.0),
        ),
        (
            Point::new(center.x - 2.8, center.y + 5.0),
            Point::new(center.x + 2.8, center.y + 5.0),
        ),
    ] {
        draw::line(scene, transform, start, end, 1.6, color);
    }
}

fn vmid_scaled(y: f64, height: f64, size: f32, text_scale: f64) -> f64 {
    y + (height - f64::from(size) * text_scale) / 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn offline_panel_owns_zoom_depth_selection_and_hit_testing() {
        // Phase `zoom_depth_is_owned_and_clamped_by_panel`.
        {
            let mut panel = OfflinePanel::new(0);
            panel.adjust_zoom(100);
            assert_eq!(panel.zoom_extra, MAX_EXTRA_ZOOM);
            panel.adjust_zoom(-100);
            assert_eq!(panel.zoom_extra, 0);
        }

        // Phase `reopening_drops_stale_search_selection`.
        {
            let mut panel = OfflinePanel::new(0);
            let hit = RegionHit {
                name: "Munich".to_string(),
                context: "Germany".to_string(),
                pos: crate::model::LatLon::new(48.5, 11.5),
                sw: crate::model::LatLon::new(48.0, 11.0),
                ne: crate::model::LatLon::new(49.0, 12.0),
            };
            panel.set_demo(42, "Mun", vec![hit]);
            assert!(panel.query_matches("Mun"));
            panel.close();
            panel.open(84);
            assert!(!panel.query_matches("Mun"));
            assert!(panel.query.is_empty());
            assert!(panel.pick.is_none());
        }

        // Phase `topmost_panel_target_wins_hit_testing`.
        {
            let mut panel = OfflinePanel::new(0);
            let rect = Rect::new(0.0, 0.0, 50.0, 50.0);
            panel.targets.push((rect, OfflineAction::Close));
            panel.targets.push((rect, OfflineAction::ClearCache));
            assert_eq!(
                panel.action_at(Point::new(10.0, 10.0)),
                Some(OfflineAction::ClearCache)
            );
        }
    }
}
