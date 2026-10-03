//! Offline-map download and cache-management panel.
//!
//! This component owns the panel's transient UI state, widget event routing,
//! hit-testing, and drawing. The map app remains responsible only for effects:
//! starting/deleting tile downloads and recording collaborative region rows.

use ui::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::modal_dialog;
use ui::widget::{
    Autocomplete, AutocompleteOutcome, Button, ButtonFace, ButtonRole, ControlSize, ListRowFace,
    ModalAction, ModalSpec, ProgressBar, Suggestion, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
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
    /// The whole window the sheet and its scrim cover.
    pub window: Rect,
    /// The map viewport a "download this view" request covers.
    pub viewport: Rect,
    pub viewport_bounds: GeoBounds,
    pub viewport_zoom: u8,
    pub regions: &'a [DownloadedRegion],
    pub progress: DownloadProgress,
    /// Whether the tile server permits saving areas; the Download button is
    /// inert and says why when it does not.
    pub downloads_allowed: bool,
    /// The pointer, for the hover state of the sheet's buttons.
    pub pointer: Point,
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
    progress_bar: ProgressBar,
}

impl OfflinePanel {
    pub fn new(cache_bytes: u64) -> Self {
        Self {
            open: false,
            zoom_extra: 2,
            cache_bytes,
            input: Autocomplete::new()
                .with_id("maps_download_search")
                .with_placeholder("Search for a city or a place")
                .with_text_role(TextRole::Body),
            results: Vec::new(),
            query: String::new(),
            debounce: None,
            pick: None,
            field_rect: None,
            targets: Vec::new(),
            progress_bar: ProgressBar::new(0.0),
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

    /// Pick result `index` as the area to download, as accepting it in the
    /// field would.
    pub fn select_result(&mut self, index: usize) -> Option<RegionHit> {
        let hit = self.results.get(index)?.clone();
        self.input.set_text(&hit.name);
        hit.name.clone_into(&mut self.query);
        self.pick = Some(hit.clone());
        self.input.close();
        self.debounce = None;
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

    /// Paint the sheet in the window's top layer: the area to download (a
    /// place search, or the current view), how much detail, the estimate or
    /// the running download's progress, what is stored on this device, and
    /// the areas already downloaded. Its one action downloads.
    #[allow(
        clippy::too_many_lines,
        reason = "the sheet is one top-to-bottom immediate-mode pass; its state, hit regions and \
                  visuals remain together in this component"
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
        let sp = &theme.tokens.spacing;
        let (s1, s2, s4) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s4));
        let subheading = theme.text(TextRole::Subheading);
        let body = theme.text(TextRole::Body);
        let numeric = body.clone().numeric();
        let caption = theme.text(TextRole::Caption);
        let sub_h = draw::line_h(text, &subheading);
        let body_h = draw::line_h(text, &body);
        let caption_h = draw::line_h(text, &caption);
        let control_h = ButtonFace::height(ControlSize::Regular, theme);
        let small_h = ButtonFace::height(ControlSize::Small, theme);
        // The progress bar shows only once a download has started.
        let bar_h = if frame.progress.total > 0 { s2 } else { 0.0 };

        let shown = frame.regions.len().min(MAX_ROWS);
        let overflow = frame.regions.len() > shown;
        let row_probe = ListRowFace::new("Area").secondary("Zoom");
        let row_h = row_probe.height(text, theme);
        let list_h = if frame.regions.is_empty() {
            caption_h
        } else {
            shown as f64 * row_h + if overflow { s1 + caption_h } else { 0.0 }
        };
        // The body is exactly the stack the painter advances through below.
        let body_height = sub_h
            + s2
            + control_h
            + s2
            + caption_h
            + s4
            + control_h
            + s2
            + caption_h
            + if bar_h > 0.0 { s2 + bar_h } else { 0.0 }
            + s4
            + small_h.max(body_h)
            + s4
            + sub_h
            + s1
            + list_h;

        let request =
            self.download_request(frame.viewport_bounds, frame.viewport_zoom, frame.viewport);
        let label = self.pick.as_ref().map_or_else(
            || "Download this view".to_string(),
            |pick| format!("Download {}", pick.name),
        );
        let actions = [ModalAction::primary(&label).enabled(frame.downloads_allowed)];
        let spec = ModalSpec::new("Offline maps")
            .actions(&actions)
            .closable(true)
            .dismiss_on_scrim(true)
            .max_width(f64::from(theme.tokens.metrics.panel_width_sheet) * 0.8)
            .body_height(body_height);
        let layout = modal_dialog::layout(frame.window, &spec, text, theme);
        let hovered_action = layout
            .actions
            .first()
            .is_some_and(|r| r.contains(frame.pointer));
        let actions = [ModalAction::primary(&label)
            .enabled(frame.downloads_allowed)
            .hovered(hovered_action)];
        let spec = spec
            .actions(&actions)
            .close_hovered(layout.close.is_some_and(|c| c.contains(frame.pointer)));
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        self.targets.push((layout.scrim, OfflineAction::Close));
        self.targets.push((layout.card, OfflineAction::Consume));
        if let Some(close) = layout.close {
            self.targets.push((close, OfflineAction::Close));
        }
        if let Some(download) = layout.actions.first() {
            self.targets.push((
                *download,
                if frame.downloads_allowed {
                    OfflineAction::Download
                } else {
                    OfflineAction::Consume
                },
            ));
        }

        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let area = layout.body;
        let left = area.x0;
        let right = area.x1;
        let mut y = area.y0;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &area);

        // The area: a place search, or the current view.
        let mut ctx = PaintCtx::new(scene, transform, text, &ink);
        draw::text(
            &mut ctx,
            "Area",
            Point::new(left, y),
            &subheading,
            ink.text_primary,
        );
        y += sub_h + s2;
        let field = Rect::new(left, y, right, y + control_h);
        self.field_rect = Some(field);
        self.input.apply_layout_external(field);
        self.input
            .draw(scene, transform, text, &ink, &PresenceCtx::empty());
        y += control_h + s2;
        let mut ctx = PaintCtx::new(scene, transform, text, &ink);
        let hint = match &self.pick {
            Some(pick) if !pick.context.is_empty() => pick.context.clone(),
            Some(pick) => pick.name.clone(),
            None => "Search for a place, or download what the map shows now.".to_string(),
        };
        draw::text_clip(
            &mut ctx,
            &hint,
            Point::new(left, y),
            &caption,
            ink.text_dim,
            right - left,
        );
        y += caption_h + s4;

        // Detail: how many zoom levels below the area's own.
        let row = Rect::new(left, y, right, y + control_h);
        let top = draw::vmid(ctx.text, row.y0, control_h, &body);
        draw::text(
            &mut ctx,
            "Detail",
            Point::new(left, top),
            &body,
            ink.text_primary,
        );
        let side = control_h;
        let range = format!("Zoom {}–{}", request.z0, request.z1);
        let range_w = draw::measure_w(ctx.text, "Zoom 19–19", &numeric);
        let plus = Rect::new(right - side, row.y0, right, row.y1);
        let minus = Rect::new(
            plus.x0 - range_w - 2.0 * s2 - side,
            row.y0,
            plus.x0 - range_w - 2.0 * s2,
            row.y1,
        );
        draw::text_center(
            &mut ctx,
            &range,
            Point::new((minus.x1 + plus.x0) / 2.0, top),
            &numeric,
            ink.text_primary,
        );
        for (button, icon, name, delta) in [
            (minus, Icon::Minus, "Less detail", -1),
            (plus, Icon::Plus, "More detail", 1),
        ] {
            Button::paint(
                scene,
                transform,
                text,
                &ink,
                &ButtonFace::icon_only(button, icon, name).hovered(button.contains(frame.pointer)),
            );
            self.targets.push((button, OfflineAction::ZoomDelta(delta)));
        }
        y += control_h + s2;

        // The estimate, or the running download.
        let estimate = TileLayer::estimate_download(request.bounds, request.z0, request.z1);
        let readout = if !frame.downloads_allowed {
            OFFLINE_AREAS_REFUSED.to_string()
        } else if frame.progress.is_active() {
            format!(
                "Downloading {} of {} tiles…",
                frame.progress.done, frame.progress.total
            )
        } else if frame.progress.total > 0 && frame.progress.done >= frame.progress.total {
            if frame.progress.failed > 0 {
                format!(
                    "Downloaded {} tiles, {} failed",
                    frame.progress.total, frame.progress.failed
                )
            } else {
                format!("Downloaded {} tiles", frame.progress.total)
            }
        } else {
            format!(
                "About {estimate} tiles, {}",
                ui::format::bytes_si(u64::from(estimate) * 18_000)
            )
        };
        let mut ctx = PaintCtx::new(scene, transform, text, &ink);
        draw::text_clip(
            &mut ctx,
            &readout,
            Point::new(left, y),
            &caption.clone().numeric(),
            ink.text_dim,
            right - left,
        );
        y += caption_h;
        if bar_h > 0.0 {
            y += s2;
            self.progress_bar
                .set_progress(ui::num::f32_of_f64(frame.progress.fraction()));
            self.progress_bar
                .apply_layout_external(Rect::new(left, y, right, y + bar_h));
            self.progress_bar
                .draw(scene, transform, text, &ink, &PresenceCtx::empty());
            y += bar_h;
        }
        y += s4;

        // What this device holds, and the way to free it.
        let clear_face = ButtonFace::new(Rect::ZERO, "Clear offline maps")
            .role(ButtonRole::Destructive)
            .control_size(ControlSize::Small);
        let clear_w = clear_face.intrinsic_width(text, theme);
        let line_h = small_h.max(body_h);
        let clear = Rect::new(
            right - clear_w,
            y + (line_h - small_h) / 2.0,
            right,
            y + (line_h + small_h) / 2.0,
        );
        Button::paint(
            scene,
            transform,
            text,
            &ink,
            &ButtonFace::new(clear, "Clear offline maps")
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .hovered(clear.contains(frame.pointer)),
        );
        self.targets.push((clear, OfflineAction::ClearCache));
        let mut ctx = PaintCtx::new(scene, transform, text, &ink);
        let stored = format!("{} on this device", ui::format::bytes_si(self.cache_bytes));
        let stored_top = draw::vmid(ctx.text, y, line_h, &numeric);
        draw::text_clip(
            &mut ctx,
            &stored,
            Point::new(left, stored_top),
            &numeric,
            ink.text_primary,
            clear.x0 - s2 - left,
        );
        y += line_h + s4;

        // The areas already downloaded.
        draw::text(
            &mut ctx,
            "Downloaded areas",
            Point::new(left, y),
            &subheading,
            ink.text_primary,
        );
        y += sub_h + s1;
        if frame.regions.is_empty() {
            draw::text_clip(
                &mut ctx,
                "No downloaded areas yet.",
                Point::new(left, y),
                &caption,
                ink.text_dim,
                right - left,
            );
        } else {
            for region in frame.regions.iter().take(shown) {
                let trash = Rect::new(
                    right - side,
                    y + (row_h - side) / 2.0,
                    right,
                    y + (row_h + side) / 2.0,
                );
                let detail = format!("Zoom {}–{}, {} tiles", region.z0, region.z1, region.tiles);
                let rect = Rect::new(left - f64::from(sp.s3), y, trash.x0 - s1, y + row_h);
                let mut ctx = PaintCtx::new(scene, transform, text, &ink);
                ListRowFace::new(&region.name)
                    .secondary(&detail)
                    .paint(&mut ctx, rect);
                Button::paint(
                    scene,
                    transform,
                    text,
                    &ink,
                    &ButtonFace::icon_only(trash, Icon::Trash2, "Remove downloaded area")
                        .hovered(trash.contains(frame.pointer)),
                );
                self.targets
                    .push((trash, OfflineAction::DeleteRegion(region.id.clone())));
                y += row_h;
            }
            if overflow {
                let mut ctx = PaintCtx::new(scene, transform, text, &ink);
                y += s1;
                draw::text(
                    &mut ctx,
                    &format!("{} more", frame.regions.len() - shown),
                    Point::new(left, y),
                    &caption,
                    ink.text_dim,
                );
            }
        }
        scene.pop_layer();
        modal_dialog::paint_actions(scene, transform, text, theme, &layout, &spec);
        self.input
            .draw_overlay(scene, transform, text, &ink, &PresenceCtx::empty());
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
