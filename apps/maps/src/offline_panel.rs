//! Offline-map download and cache-management panel.
//!
//! This component owns the panel's transient UI state, widget event routing,
//! hit-testing, and drawing. The map app remains responsible only for effects:
//! starting/deleting tile downloads and recording collaborative region rows.

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Autocomplete, AutocompleteOutcome, Suggestion, Widget};
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
pub(crate) struct OfflineFrame<'a> {
    pub viewport: Rect,
    pub viewport_bounds: GeoBounds,
    pub viewport_zoom: u8,
    pub regions: &'a [DownloadedRegion],
    pub progress: DownloadProgress,
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
            scale,
            &modifiers,
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
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
                self.debounce = Some(Instant::now() + SEARCH_DEBOUNCE);
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
            22.0
        } else {
            shown as f64 * 42.0 + if overflow { 18.0 } else { 0.0 }
        };
        let height = (350.0 + list_h).min(frame.viewport.height() - 24.0);
        let width = 440.0_f64.min(frame.viewport.width() - 40.0);
        let center = frame.viewport.center();
        let card = Rect::new(
            center.x - width / 2.0,
            center.y - height / 2.0,
            center.x + width / 2.0,
            center.y + height / 2.0,
        );
        draw::card(
            scene,
            transform,
            card,
            16.0,
            colors.surfaces.raised,
            colors.border_standard,
        );
        self.targets.push((card, OfflineAction::Consume));

        let left = card.x0 + 22.0;
        let right = card.x1 - 22.0;
        let mut y = card.y0 + 20.0;
        draw::text_w(
            scene,
            text,
            transform,
            "Offline maps",
            left,
            y,
            18.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        let close = Rect::new(right - 26.0, card.y0 + 16.0, right, card.y0 + 42.0);
        draw::text_center(
            scene,
            text,
            transform,
            "✕",
            close.x0,
            close.x1,
            vmid(close.y0, 26.0, 14.0),
            14.0,
            theme.text_dim,
        );
        self.targets.push((close, OfflineAction::Close));
        y += 30.0;

        draw::text_w(
            scene,
            text,
            transform,
            "Area to download",
            left,
            y,
            13.0,
            theme.text_primary,
            TextWeight::Semibold,
        );
        y += 22.0;
        let field = Rect::new(left, y, right, y + 36.0);
        self.field_rect = Some(field);
        self.input.apply_layout_external(field);
        self.input
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
        y += 44.0;

        let request =
            self.download_request(frame.viewport_bounds, frame.viewport_zoom, frame.viewport);
        if let Some(pick) = &self.pick {
            draw::text_w(
                scene,
                text,
                transform,
                &pick.name,
                left,
                y,
                13.0,
                theme.accent_primary,
                TextWeight::Semibold,
            );
            if !pick.context.is_empty() {
                let name_width = draw::measure_w(text, &pick.name, 13.0) + 10.0;
                draw::text_clip(
                    scene,
                    text,
                    transform,
                    &pick.context,
                    left + name_width,
                    y,
                    11.5,
                    theme.text_dim,
                    (right - left - name_width).max(20.0),
                );
            }
        } else {
            draw::text(
                scene,
                text,
                transform,
                "Search a city or place above, or download the current view.",
                left,
                y,
                11.5,
                theme.text_dim,
            );
        }
        y += 24.0;

        draw::text_w(
            scene,
            text,
            transform,
            "Detail (zoom levels)",
            left,
            y,
            13.0,
            theme.text_primary,
            TextWeight::Semibold,
        );
        let minus = Rect::new(right - 84.0, y - 4.0, right - 58.0, y + 22.0);
        let plus = Rect::new(right - 26.0, y - 4.0, right, y + 22.0);
        for (button, label) in [(minus, "–"), (plus, "+")] {
            draw::fill_rrect(scene, transform, button, 7.0, colors.surfaces.base);
            draw::text_center(
                scene,
                text,
                transform,
                label,
                button.x0,
                button.x1,
                vmid(button.y0, 26.0, 18.0),
                18.0,
                theme.text_primary,
            );
        }
        draw::text_center(
            scene,
            text,
            transform,
            &format!("z{}–z{}", request.z0, request.z1),
            minus.x1,
            plus.x0,
            vmid(y - 4.0, 26.0, 13.0),
            13.0,
            theme.accent_primary,
        );
        self.targets.push((minus, OfflineAction::ZoomDelta(-1)));
        self.targets.push((plus, OfflineAction::ZoomDelta(1)));
        y += 34.0;

        let estimate = TileLayer::estimate_download(request.bounds, request.z0, request.z1);
        let readout = if frame.progress.is_active() {
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
                fmt_bytes(u64::from(estimate) * 18_000)
            )
        };
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
        y += 22.0;
        let bar = Rect::new(left, y, right, y + 8.0);
        draw::fill_rrect(scene, transform, bar, 4.0, colors.surfaces.base);
        let fraction = frame.progress.fraction();
        if fraction > 0.0 {
            draw::fill_rrect(
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
        y += 24.0;
        draw::text(
            scene,
            text,
            transform,
            &format!("Cached on disk: {}", fmt_bytes(self.cache_bytes)),
            left,
            y,
            12.0,
            theme.text_primary,
        );
        y += 30.0;

        draw::text_w(
            scene,
            text,
            transform,
            "Downloaded areas",
            left,
            y,
            13.0,
            theme.text_primary,
            TextWeight::Semibold,
        );
        y += 24.0;
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
            y += 22.0;
        } else {
            for region in frame.regions.iter().take(shown) {
                let row = Rect::new(left, y, right, y + 34.0);
                draw::fill_rrect(scene, transform, row, 8.0, colors.surfaces.base);
                let trash = Rect::new(
                    row.x1 - 34.0,
                    row.center().y - 13.0,
                    row.x1 - 8.0,
                    row.center().y + 13.0,
                );
                let available = (trash.x0 - row.x0 - 20.0).max(40.0);
                draw::text_clip(
                    scene,
                    text,
                    transform,
                    &region.name,
                    row.x0 + 12.0,
                    row.y0 + 5.0,
                    13.0,
                    theme.text_primary,
                    available,
                );
                draw::text_clip(
                    scene,
                    text,
                    transform,
                    &format!("z{}–z{} · {} tiles", region.z0, region.z1, region.tiles),
                    row.x0 + 12.0,
                    row.y0 + 20.0,
                    11.0,
                    theme.text_dim,
                    available,
                );
                draw::fill_rrect(scene, transform, trash, 7.0, colors.surfaces.raised);
                draw::stroke_rrect(scene, transform, trash, 7.0, 1.0, colors.border_standard);
                draw_trash(scene, transform, trash, colors.semantic.danger);
                self.targets
                    .push((trash, OfflineAction::DeleteRegion(region.id.clone())));
                y += 42.0;
            }
            if overflow {
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
                y += 18.0;
            }
        }
        y += 12.0;

        let button_height = 38.0;
        let button_width = (right - left - 10.0) / 2.0;
        let download = Rect::new(left, y, left + button_width, y + button_height);
        let clear = Rect::new(left + button_width + 10.0, y, right, y + button_height);
        draw::fill_rrect(scene, transform, download, 9.0, theme.accent_primary);
        let label = self.pick.as_ref().map_or_else(
            || "Download current view".to_string(),
            |pick| format!("Download {}", pick.name),
        );
        let label = draw::ellipsize(text, &label, 13.0, button_width - 20.0);
        draw::text_center(
            scene,
            text,
            transform,
            &label,
            download.x0,
            download.x1,
            vmid(download.y0, button_height, 13.0),
            13.0,
            Color::WHITE,
        );
        self.targets.push((download, OfflineAction::Download));
        draw::fill_rrect(scene, transform, clear, 9.0, colors.surfaces.base);
        draw::stroke_rrect(scene, transform, clear, 9.0, 1.0, colors.border_standard);
        draw::text_center(
            scene,
            text,
            transform,
            "Clear all",
            clear.x0,
            clear.x1,
            vmid(clear.y0, button_height, 13.0),
            13.0,
            colors.semantic.danger,
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

fn vmid(y: f64, height: f64, size: f32) -> f64 {
    y + (height - f64::from(size)) / 2.0 - 1.0
}

fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_depth_is_owned_and_clamped_by_panel() {
        let mut panel = OfflinePanel::new(0);
        panel.adjust_zoom(100);
        assert_eq!(panel.zoom_extra, MAX_EXTRA_ZOOM);
        panel.adjust_zoom(-100);
        assert_eq!(panel.zoom_extra, 0);
    }

    #[test]
    fn reopening_drops_stale_search_selection() {
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

    #[test]
    fn topmost_panel_target_wins_hit_testing() {
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
