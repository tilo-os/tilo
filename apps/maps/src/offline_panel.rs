//! Offline-map download and cache-management panel.
//!
//! This component owns the panel's transient UI state, widget event routing,
//! hit-testing, and drawing. The map app remains responsible only for effects:
//! starting/deleting tile downloads and recording collaborative region rows.

use ui::time::{Duration, Instant};

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
    ModalAction, ModalSpec, NumberInput, ProgressBar, Suggestion, Widget,
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

/// What a refused download tells the person: the map source does not let
/// areas be saved ahead (OpenStreetMap's tile server forbids fetching tiles
/// nobody is viewing), but what they have looked at stays.
pub(crate) fn areas_refused() -> String {
    crate::failure::t("maps-offline-refused")
}
const MAX_EXTRA_ZOOM: u8 = 6;

/// Effect requested by a pointer interaction inside the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OfflineAction {
    Close,
    ZoomDelta(i8),
    Download,
    /// Stop the download in progress; what arrived stays.
    StopDownload,
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
    /// Whether the tile server permits saving areas. When it does not, the
    /// sheet offers no download: it says what stays on the device and how
    /// to free it.
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
    /// How many zoom levels a download covers, from the area's own.
    levels: NumberInput,
    levels_rect: Option<Rect>,
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
                .with_placeholder(&crate::failure::t("maps-offline-search-placeholder"))
                .with_text_role(TextRole::Body),
            results: Vec::new(),
            query: String::new(),
            debounce: None,
            pick: None,
            field_rect: None,
            levels: NumberInput::new(3.0)
                .with_id("maps_download_levels")
                .with_range(1.0, f64::from(MAX_EXTRA_ZOOM) + 1.0)
                .with_step(1.0)
                .with_accessible_name(&crate::failure::t("maps-offline-zoom-levels"))
                .compact(),
            levels_rect: None,
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

    #[cfg(test)]
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

    /// Route an event to the sheet's toolkit controls: the zoom-level
    /// stepper under the pointer, else the shared autocomplete field.
    pub fn route_field_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        mouse: Point,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let on_levels = self.levels_rect.is_some_and(|rect| rect.contains(mouse));
        if on_levels || self.levels.is_editing() {
            let handled = crate::hand_laid::drive(&mut self.levels, event, scale, modifiers, doc);
            if let Some(levels) = self.levels.take_changed() {
                self.zoom_extra = (levels.round() as u8).saturating_sub(1).min(MAX_EXTRA_ZOOM);
            }
            if handled || on_levels {
                return handled;
            }
        }
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
        let handled = crate::hand_laid::drive(&mut self.input, event, scale, modifiers, doc);
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
                .with_name(&crate::failure::t("maps-offline-title"))
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
                crate::failure::t("maps-offline-area"),
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
                crate::failure::t("maps-offline-close"),
                "maps.offline.close".to_string(),
            ));
        }
        if let Some(levels) = self.levels_rect {
            for (action, label, target) in [
                (
                    -1,
                    &crate::failure::t("maps-offline-fewer-zoom-levels"),
                    "maps.offline.zoom-out",
                ),
                (
                    1,
                    &crate::failure::t("maps-offline-more-zoom-levels"),
                    "maps.offline.zoom-in",
                ),
            ] {
                targets.push((
                    levels,
                    OfflineAction::ZoomDelta(action),
                    label.to_string(),
                    target.to_string(),
                ));
            }
        }
        for (rect, action) in &self.targets {
            let (label, target) = match action {
                OfflineAction::Download => (
                    &crate::failure::t("maps-offline-download-area"),
                    "maps.offline.download".to_string(),
                ),
                OfflineAction::StopDownload => (
                    &crate::failure::t("maps-offline-stop"),
                    "maps.offline.stop-download".to_string(),
                ),
                OfflineAction::DeleteRegion(id) => (
                    &crate::failure::t("maps-offline-delete-area"),
                    format!("maps.offline.region:{id}:delete"),
                ),
                OfflineAction::ClearCache => (
                    &crate::failure::t("maps-offline-clear"),
                    "maps.offline.clear".to_string(),
                ),
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

    /// Paint the sheet in the window's top layer. Where the map source lets
    /// areas be saved: the area to download (a place search, or the current
    /// view), how many zoom levels, the estimate or the running download's
    /// progress, and Download as its one action. Where it does not: what
    /// stays on the device anyway. Then what this device holds, with the way
    /// to free it, and the areas already downloaded.
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
        let downloads = frame.downloads_allowed;
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
        let bar_h = if downloads && frame.progress.total > 0 {
            s2
        } else {
            0.0
        };

        let shown = frame.regions.len().min(MAX_ROWS);
        let overflow = frame.regions.len() > shown;
        let row_probe = ListRowFace::new("Area").secondary("12 MB");
        let row_h = row_probe.height(text, theme);
        let regions_h = if frame.regions.is_empty() {
            0.0
        } else {
            s4 + sub_h + s1 + shown as f64 * row_h + if overflow { s1 + caption_h } else { 0.0 }
        };
        // Without downloads the sheet says, wrapped to the body, what stays.
        let note_w = ui::num::f32_of_f64(modal_dialog::body_width(
            frame.window,
            modal_dialog::MODAL_MAX_WIDTH,
            theme,
        ));
        let note_h = f64::from(text.measure_style(&areas_refused(), Some(note_w), &body).1);
        // The body is exactly the stack the painter advances through below.
        let form_h = if downloads {
            sub_h
                + s2
                + control_h
                + s2
                + caption_h
                + s4
                + control_h
                + s2
                + caption_h
                + if bar_h > 0.0 { s2 + bar_h } else { 0.0 }
        } else {
            note_h
        };
        let body_height = form_h + s4 + small_h.max(body_h) + regions_h;

        let request =
            self.download_request(frame.viewport_bounds, frame.viewport_zoom, frame.viewport);
        let label = self.pick.as_ref().map_or_else(
            || crate::failure::t("maps-offline-download-view"),
            |pick| {
                crate::failure::t_args(
                    "maps-offline-download-named",
                    &[("name", pick.name.clone().into())],
                )
            },
        );
        // While a download runs, the one action the sheet offers stops it:
        // picking the wrong city must not mean waiting out hundreds of tiles
        // and then deleting the region.
        let running = frame.progress.is_active();
        let label = if running {
            crate::failure::t("maps-offline-stop")
        } else {
            label
        };
        let primary = [ModalAction::primary(&label)];
        let offered: &[ModalAction<'_>] = if downloads { &primary } else { &[] };
        let title = crate::failure::t("maps-offline-title");
        let spec = ModalSpec::new(&title)
            .actions(offered)
            .closable(true)
            .dismiss_on_scrim(true)
            .body_height(body_height);
        let layout = modal_dialog::layout(frame.window, &spec, text, theme);
        let hovered_action = layout
            .actions
            .first()
            .is_some_and(|r| r.contains(frame.pointer));
        let actions = [ModalAction::primary(&label).hovered(hovered_action)];
        let offered: &[ModalAction<'_>] = if downloads { &actions } else { &[] };
        let spec = spec
            .actions(offered)
            .close_hovered(layout.close.is_some_and(|c| c.contains(frame.pointer)));
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        self.targets.push((layout.scrim, OfflineAction::Close));
        self.targets.push((layout.card, OfflineAction::Consume));
        if let Some(close) = layout.close {
            self.targets.push((close, OfflineAction::Close));
        }
        if let Some(action) = layout.actions.first() {
            self.targets.push((
                *action,
                if running {
                    OfflineAction::StopDownload
                } else {
                    OfflineAction::Download
                },
            ));
        }

        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let area = layout.body;
        let left = area.x0;
        let right = area.x1;
        let mut y = area.y0;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &area);

        if downloads {
            y = self.draw_download_form(
                scene,
                transform,
                text,
                &ink,
                Rect::new(left, y, right, area.y1),
                &request,
                frame,
            );
        } else {
            self.field_rect = None;
            self.levels_rect = None;
            let mut ctx = PaintCtx::new(scene, transform, text, &ink);
            ctx.text(
                &areas_refused(),
                &body,
                ink.text_primary,
                Point::new(left, y),
                Some(note_w),
            );
            y += note_h;
        }
        y += s4;

        // What this device holds, and the way to free it.
        let clear_label = crate::failure::t("maps-offline-clear-short");
        let clear_face = ButtonFace::new(Rect::ZERO, &clear_label)
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
            &ButtonFace::new(clear, &clear_label)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .hovered(clear.contains(frame.pointer)),
        );
        self.targets.push((clear, OfflineAction::ClearCache));
        let mut ctx = PaintCtx::new(scene, transform, text, &ink);
        let stored = crate::failure::t_args(
            "maps-offline-stored",
            &[("size", ui::format::size(self.cache_bytes).into())],
        );
        let stored_top = draw::vmid(ctx.text, y, line_h, &numeric);
        draw::text_clip(
            &mut ctx,
            &stored,
            Point::new(left, stored_top),
            &numeric,
            ink.text_primary,
            clear.x0 - s2 - left,
        );
        y += line_h;

        // The areas already downloaded.
        if !frame.regions.is_empty() {
            y += s4;
            draw::text(
                &mut ctx,
                &crate::failure::t("maps-offline-downloaded-areas"),
                Point::new(left, y),
                &subheading,
                ink.text_primary,
            );
            y += sub_h + s1;
            let side = control_h;
            for region in frame.regions.iter().take(shown) {
                let trash = Rect::new(
                    right - side,
                    y + (row_h - side) / 2.0,
                    right,
                    y + (row_h + side) / 2.0,
                );
                let footprint = ui::format::size(u64::from(region.tiles) * TILE_BYTES);
                let rect = Rect::new(left - f64::from(sp.s3), y, trash.x0 - s1, y + row_h);
                let mut ctx = PaintCtx::new(scene, transform, text, &ink);
                ListRowFace::new(&region.name)
                    .secondary(&footprint)
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
        if downloads {
            self.input
                .draw_overlay(scene, transform, text, &ink, &PresenceCtx::empty());
        }
    }

    /// The download form: the area (a place search, or the current view),
    /// how many zoom levels, and the estimate or the running download's
    /// progress. Returns the y below it.
    fn draw_download_form(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        ink: &Theme,
        body_rect: Rect,
        request: &DownloadRequest,
        frame: OfflineFrame<'_>,
    ) -> f64 {
        let sp = &ink.tokens.spacing;
        let (s2, s4) = (f64::from(sp.s2), f64::from(sp.s4));
        let subheading = ink.text(TextRole::Subheading);
        let body = ink.text(TextRole::Body);
        let caption = ink.text(TextRole::Caption);
        let control_h = ButtonFace::height(ControlSize::Regular, ink);
        let (left, right) = (body_rect.x0, body_rect.x1);
        let mut y = body_rect.y0;

        // The area: a place search, or the current view.
        let mut ctx = PaintCtx::new(scene, transform, text, ink);
        draw::text(
            &mut ctx,
            &crate::failure::t("maps-offline-area"),
            Point::new(left, y),
            &subheading,
            ink.text_primary,
        );
        y += draw::line_h(ctx.text, &subheading) + s2;
        let field = Rect::new(left, y, right, y + control_h);
        self.field_rect = Some(field);
        self.input.apply_layout_external(field);
        self.input
            .draw(scene, transform, text, ink, &PresenceCtx::empty());
        y += control_h + s2;
        let mut ctx = PaintCtx::new(scene, transform, text, ink);
        let hint = match &self.pick {
            Some(pick) if !pick.context.is_empty() => pick.context.clone(),
            Some(pick) => pick.name.clone(),
            None => crate::failure::t("maps-offline-empty"),
        };
        draw::text_clip(
            &mut ctx,
            &hint,
            Point::new(left, y),
            &caption,
            ink.text_dim,
            right - left,
        );
        y += draw::line_h(ctx.text, &caption) + s4;

        // How many zoom levels, from the area's own: the toolkit's stepper.
        let top = draw::vmid(ctx.text, y, control_h, &body);
        draw::text(
            &mut ctx,
            &crate::failure::t("maps-offline-zoom-levels"),
            Point::new(left, top),
            &body,
            ink.text_primary,
        );
        self.levels.set_value(f64::from(self.zoom_extra) + 1.0);
        let levels_w = self.levels.preferred_width(ink, text);
        let levels = Rect::new(right - levels_w, y, right, y + control_h);
        self.levels_rect = Some(levels);
        self.levels.apply_layout_external(levels);
        self.levels
            .draw(scene, transform, text, ink, &PresenceCtx::empty());
        y += control_h + s2;

        // The estimate, or the running download.
        let estimate = TileLayer::estimate_download(request.bounds, request.z0, request.z1);
        let count = |value: u32| ui::i18n::ArgValue::from(i64::from(value));
        let readout = if frame.progress.is_active() {
            crate::failure::t_args(
                "maps-offline-progress",
                &[
                    ("done", count(frame.progress.done)),
                    ("total", count(frame.progress.total)),
                ],
            )
        } else if frame.progress.stopped {
            crate::failure::t_args(
                "maps-offline-stopped-count",
                &[("total", count(frame.progress.total))],
            )
        } else if frame.progress.total > 0 && frame.progress.done >= frame.progress.total {
            if frame.progress.failed > 0 {
                crate::failure::t_args(
                    "maps-offline-done-failed",
                    &[
                        ("total", count(frame.progress.total)),
                        ("failed", count(frame.progress.failed)),
                    ],
                )
            } else {
                crate::failure::t_args(
                    "maps-offline-done",
                    &[("total", count(frame.progress.total))],
                )
            }
        } else {
            crate::failure::t_args(
                "maps-offline-estimate",
                &[(
                    "size",
                    ui::format::size(u64::from(estimate) * TILE_BYTES).into(),
                )],
            )
        };
        let mut ctx = PaintCtx::new(scene, transform, text, ink);
        draw::text_clip(
            &mut ctx,
            &readout,
            Point::new(left, y),
            &caption.clone().numeric(),
            ink.text_dim,
            right - left,
        );
        y += draw::line_h(ctx.text, &caption);
        if frame.progress.total > 0 {
            y += s2;
            self.progress_bar
                .set_progress(ui::num::f32_of_f64(frame.progress.fraction()));
            self.progress_bar
                .apply_layout_external(Rect::new(left, y, right, y + s2));
            self.progress_bar
                .draw(scene, transform, text, ink, &PresenceCtx::empty());
            y += s2;
        }
        y
    }
}

/// About how much one stored map tile takes on disk.
const TILE_BYTES: u64 = 18_000;

fn region_base_zoom(sw: crate::model::LatLon, ne: crate::model::LatLon, viewport: Rect) -> u8 {
    let corners = [
        sw,
        ne,
        crate::model::LatLon::new(sw.lat, ne.lon),
        crate::model::LatLon::new(ne.lat, sw.lon),
    ];
    let zoom = geo::fit_bounds(
        &corners,
        viewport.width().max(1.0),
        viewport.height().max(1.0),
        40.0,
    )
    .map_or(12.0, |(_, zoom)| zoom);
    TileLayer::tile_zoom(zoom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_panel_owns_zoom_depth_selection_and_hit_testing() {
        {
            let mut panel = OfflinePanel::new(0);
            panel.adjust_zoom(100);
            assert_eq!(panel.zoom_extra, MAX_EXTRA_ZOOM);
            panel.adjust_zoom(-100);
            assert_eq!(panel.zoom_extra, 0);
        }

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
