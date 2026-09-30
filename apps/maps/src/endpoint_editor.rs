//! Self-contained From/To editor for the Directions panel.

use ui::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TextInput, TextInputKind};

use crate::canvas;
use crate::draw;
use crate::geo;
use crate::model::{LatLon, Place};
use crate::search_box::drive_text_input;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(280);
const OVERLAY_MAX: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Endpoint {
    Origin,
    Destination,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum EndpointEffect {
    Changed { endpoint: Endpoint, text: String },
    Search { endpoint: Endpoint, query: String },
    Pick { endpoint: Endpoint, place: Place },
    Swap,
    Handled,
}

#[derive(Clone, Copy)]
enum EndpointHit {
    Field(Endpoint),
    Pick(Endpoint, usize),
    Swap,
    Consume,
}

pub(super) struct EndpointFrame<'a> {
    pub content: Rect,
    pub top: f64,
    pub origin: Option<&'a str>,
    pub destination: Option<&'a str>,
}

pub(super) struct EndpointEditor {
    origin_input: TextInput,
    destination_input: TextInput,
    focused: Option<Endpoint>,
    origin_query: String,
    destination_query: String,
    origin_results: Vec<Place>,
    destination_results: Vec<Place>,
    deadline: Option<Instant>,
    origin_rect: Option<Rect>,
    destination_rect: Option<Rect>,
    hits: Vec<(Rect, EndpointHit)>,
}

impl EndpointEditor {
    pub(super) fn new() -> Self {
        Self {
            origin_input: endpoint_input("Choose starting point"),
            destination_input: endpoint_input("Choose destination"),
            focused: None,
            origin_query: String::new(),
            destination_query: String::new(),
            origin_results: Vec::new(),
            destination_results: Vec::new(),
            deadline: None,
            origin_rect: None,
            destination_rect: None,
            hits: Vec::new(),
        }
    }

    /// Adopt persisted/external query state. The host mirrors every Changed
    /// effect immediately, so this is idempotent during ordinary local edits
    /// and authoritative after a document reload.
    pub(super) fn sync_queries(&mut self, origin: &str, destination: &str) {
        if self.origin_query != origin {
            origin.clone_into(&mut self.origin_query);
            if self.focused == Some(Endpoint::Origin) {
                self.origin_input.set_text(origin);
            }
        }
        if self.destination_query != destination {
            destination.clone_into(&mut self.destination_query);
            if self.focused == Some(Endpoint::Destination) {
                self.destination_input.set_text(destination);
            }
        }
    }

    pub(super) fn set_results(&mut self, endpoint: Endpoint, results: Vec<Place>) {
        *self.results_mut(endpoint) = results;
    }

    fn results(&self, endpoint: Endpoint) -> &[Place] {
        match endpoint {
            Endpoint::Origin => &self.origin_results,
            Endpoint::Destination => &self.destination_results,
        }
    }

    pub(super) fn dismiss(&mut self) {
        self.focused = None;
        self.deadline = None;
        self.origin_input.set_focus(false);
        self.destination_input.set_focus(false);
    }

    pub(super) fn begin_edit(&mut self, endpoint: Endpoint) {
        self.focus(endpoint);
    }

    pub(super) fn is_focused(&self) -> bool {
        self.focused.is_some()
    }

    #[cfg(test)]
    pub(super) fn has_pending_query(&self) -> bool {
        self.deadline.is_some()
    }

    /// When the focused endpoint's typed query settles into a live geocode.
    pub(super) const fn query_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    #[cfg(test)]
    pub(super) fn overlay_active(&self) -> bool {
        let Some(endpoint) = self.focused else {
            return false;
        };
        let rect = match endpoint {
            Endpoint::Origin => self.origin_rect,
            Endpoint::Destination => self.destination_rect,
        };
        rect.is_some() && !self.results(endpoint).is_empty()
    }

    fn query(&self, endpoint: Endpoint) -> &str {
        match endpoint {
            Endpoint::Origin => &self.origin_query,
            Endpoint::Destination => &self.destination_query,
        }
    }

    pub(super) fn take_due_search(&mut self, now: Instant) -> Option<EndpointEffect> {
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        self.deadline = None;
        let endpoint = self.focused?;
        let query = self.query(endpoint).trim().to_string();
        (!query.is_empty()).then_some(EndpointEffect::Search { endpoint, query })
    }

    pub(super) fn handle_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) -> Option<EndpointEffect> {
        let endpoint = self.focused?;
        if matches!(
            event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    ..
                },
                ..
            }
        ) {
            self.dismiss();
            return Some(EndpointEffect::Handled);
        }
        let input = self.input_mut(endpoint);
        input.set_focus(true);
        drive_text_input(input, event, scale, modifiers);
        if input.take_submit() {
            self.deadline = None;
            let query = self.query(endpoint).trim().to_string();
            return Some(EndpointEffect::Search { endpoint, query });
        }
        let text = input.take_dirty()?;
        text.clone_into(self.query_mut(endpoint));
        self.deadline = if text.trim().is_empty() {
            None
        } else {
            Some(ui::time::now() + SEARCH_DEBOUNCE)
        };
        Some(EndpointEffect::Changed { endpoint, text })
    }

    pub(super) fn forward_pointer(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        drive_text_input(&mut self.origin_input, event, scale, modifiers);
        drive_text_input(&mut self.destination_input, event, scale, modifiers);
    }

    pub(super) fn pointer_press(
        &mut self,
        point: Point,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) -> Option<EndpointEffect> {
        let hit = self.hit_at(point)?;
        match hit {
            EndpointHit::Field(endpoint) => {
                self.focus(endpoint);
                let input = self.input_mut(endpoint);
                input.set_focus(true);
                drive_text_input(input, event, scale, modifiers);
                Some(EndpointEffect::Changed {
                    endpoint,
                    text: String::new(),
                })
            }
            EndpointHit::Pick(endpoint, index) => {
                let place = self.results(endpoint).get(index).cloned()?;
                self.results_mut(endpoint).clear();
                self.dismiss();
                Some(EndpointEffect::Pick { endpoint, place })
            }
            EndpointHit::Swap => {
                self.dismiss();
                Some(EndpointEffect::Swap)
            }
            EndpointHit::Consume => Some(EndpointEffect::Handled),
        }
    }

    fn hit_at(&self, point: Point) -> Option<EndpointHit> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, hit)| *hit)
    }

    #[cfg(test)]
    pub(super) fn test_field_rect(&self, endpoint: Endpoint) -> Option<Rect> {
        match endpoint {
            Endpoint::Origin => self.origin_rect,
            Endpoint::Destination => self.destination_rect,
        }
    }

    #[cfg(test)]
    pub(super) fn test_pick_rect(&self, endpoint: Endpoint) -> Option<Rect> {
        self.hits.iter().find_map(|(rect, hit)| {
            matches!(hit, EndpointHit::Pick(candidate, _) if *candidate == endpoint)
                .then_some(*rect)
        })
    }

    #[cfg(test)]
    pub(super) fn test_swap_rect(&self) -> Option<Rect> {
        self.hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, EndpointHit::Swap).then_some(*rect))
    }

    #[cfg(test)]
    pub(super) fn test_top_hit_is_pick(&self, point: Point, endpoint: Endpoint) -> bool {
        matches!(self.hit_at(point), Some(EndpointHit::Pick(candidate, _)) if candidate == endpoint)
    }

    #[cfg(test)]
    pub(super) fn focused_endpoint(&self) -> Option<Endpoint> {
        self.focused
    }

    #[cfg(test)]
    pub(super) fn test_results(&self, endpoint: Endpoint) -> &[Place] {
        self.results(endpoint)
    }

    #[cfg(test)]
    pub(super) fn test_query(&self, endpoint: Endpoint) -> &str {
        self.query(endpoint)
    }

    fn focus(&mut self, endpoint: Endpoint) {
        self.focused = Some(endpoint);
        self.deadline = None;
        self.query_mut(endpoint).clear();
        self.results_mut(endpoint).clear();
        self.origin_input.set_focus(endpoint == Endpoint::Origin);
        self.destination_input
            .set_focus(endpoint == Endpoint::Destination);
        self.input_mut(endpoint).set_text("");
    }

    pub(super) fn draw_fields(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &EndpointFrame<'_>,
    ) -> f64 {
        self.hits.clear();
        let colors = &theme.tokens.colors;
        let content = frame.content;
        let y = frame.top;
        let field_h = 40.0;
        let swap_w = 34.0;
        let fields_right = content.x1 - swap_w - 8.0;
        let origin_rect = Rect::new(content.x0, y, fields_right, y + field_h);
        self.origin_rect = Some(origin_rect);
        let origin_inner = self.draw_field(
            scene,
            transform,
            theme,
            origin_rect,
            theme.accent_primary,
            Endpoint::Origin,
        );
        let origin_text = if self.focused == Some(Endpoint::Origin) {
            self.origin_query.as_str()
        } else {
            frame.origin.unwrap_or("")
        };
        self.origin_input.set_text(origin_text);
        self.origin_input
            .set_focus(self.focused == Some(Endpoint::Origin));
        self.origin_input.apply_layout_external(origin_inner);
        self.origin_input
            .draw(scene, transform, text, theme, &PresenceCtx::empty());

        let destination_y = y + field_h + f64::from(theme.tokens.spacing.s2);
        let destination_rect = Rect::new(
            content.x0,
            destination_y,
            fields_right,
            destination_y + field_h,
        );
        self.destination_rect = Some(destination_rect);
        let destination_inner = self.draw_field(
            scene,
            transform,
            theme,
            destination_rect,
            colors.semantic.danger,
            Endpoint::Destination,
        );
        let destination_text = if self.focused == Some(Endpoint::Destination) {
            self.destination_query.as_str()
        } else {
            frame.destination.unwrap_or("")
        };
        self.destination_input.set_text(destination_text);
        self.destination_input
            .set_focus(self.focused == Some(Endpoint::Destination));
        self.destination_input
            .apply_layout_external(destination_inner);
        self.destination_input
            .draw(scene, transform, text, theme, &PresenceCtx::empty());

        let swap = Rect::new(
            content.x1 - swap_w,
            y + field_h / 2.0,
            content.x1,
            destination_rect.y1 - field_h / 2.0,
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(swap, "⇅")
                .text_size(16.0)
                .accessible_name("Swap origin and destination"),
        );
        self.hits.push((swap, EndpointHit::Swap));
        destination_rect.y1 + f64::from(theme.tokens.spacing.s3)
    }

    fn draw_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        dot: vello::peniko::Color,
        endpoint: Endpoint,
    ) -> Rect {
        let colors = &theme.tokens.colors;
        // The field is the base tier seated in the panel with its derived
        // edge; focus adds the toolkit's 2 px ring.
        draw::edged(scene, transform, rect, 9.0, colors.surfaces.base);
        if self.focused == Some(endpoint) {
            draw::stroke_rrect(scene, transform, rect, 9.0, 2.0, theme.border_focus);
        }
        draw::fill_circle(
            scene,
            transform,
            Point::new(rect.x0 + 16.0, rect.center().y),
            4.0,
            dot,
        );
        self.hits.push((rect, EndpointHit::Field(endpoint)));
        Rect::new(rect.x0 + 26.0, rect.y0, rect.x1 - 12.0, rect.y1)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one z-ordered dropdown paint pass; splitting its card and rows would require \
                  duplicating the render context without creating an independently stateful part"
    )]
    pub(super) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        distance_anchor: LatLon,
    ) {
        let Some(endpoint) = self.focused else { return };
        let Some(field) = (match endpoint {
            Endpoint::Origin => self.origin_rect,
            Endpoint::Destination => self.destination_rect,
        }) else {
            return;
        };
        let results = match endpoint {
            Endpoint::Origin => &self.origin_results,
            Endpoint::Destination => &self.destination_results,
        };
        let sp = &theme.tokens.spacing;
        let top = field.y1 + f64::from(sp.s2);
        let colors = &theme.tokens.colors;
        if results.is_empty() {
            if self.query(endpoint).trim().is_empty() {
                return;
            }
            let card = Rect::new(field.x0, top, field.x1, top + 34.0);
            draw::card(scene, transform, card, 12.0, colors.surfaces.raised);
            self.hits.push((card, EndpointHit::Consume));
            draw::text(
                scene,
                text,
                transform,
                "Searching…",
                card.x0 + 12.0,
                vmid(card.y0, card.height(), 12.0),
                12.0,
                theme.text_dim,
            );
            return;
        }
        // Same dropdown rhythm as the search overlay: 46 px row pitch, `s1`
        // between hit rects, `s2` pad and `s2` under the field.
        let row_pitch = 46.0;
        let row_gap = f64::from(sp.s1);
        let pad = f64::from(sp.s2);
        let count = results.len().min(OVERLAY_MAX);
        let card = Rect::new(
            field.x0,
            top,
            field.x1,
            top + pad * 2.0 + count as f64 * row_pitch - row_gap,
        );
        draw::card(scene, transform, card, 12.0, colors.surfaces.raised);
        self.hits.push((card, EndpointHit::Consume));
        let scale = text.text_scale();
        let name_h = ui::text_metrics::line_height_px(13.0, scale);
        let sub_h = ui::text_metrics::line_height_px(11.0, scale);
        let mut y = top + pad;
        for (index, place) in results.iter().take(count).enumerate() {
            let rect = Rect::new(field.x0 + pad, y, field.x1 - pad, y + row_pitch - row_gap);
            let name_y = rect.y0 + (rect.height() - name_h - sub_h) / 2.0;
            draw::fill_circle(
                scene,
                transform,
                Point::new(rect.x0 + 14.0, rect.center().y),
                6.0,
                canvas::category_color(theme, place.category),
            );
            let text_x = rect.x0 + 30.0;
            let distance = geo::fmt_distance(geo::haversine_m(place.pos, distance_anchor));
            let distance_w = draw::measure_w(text, &distance, 11.0) + 8.0;
            draw::text_clip(
                &mut PaintCtx::new(scene, transform, text, theme),
                &place.name,
                Point::new(text_x, name_y),
                13.0,
                theme.text_primary,
                rect.x1 - text_x - distance_w,
            );
            draw::text_clip(
                &mut PaintCtx::new(scene, transform, text, theme),
                &place.address,
                Point::new(text_x, name_y + name_h),
                11.0,
                theme.text_dim,
                rect.x1 - text_x - distance_w,
            );
            draw::text_right(
                scene,
                text,
                transform,
                &distance,
                rect.x1 - pad,
                name_y + (name_h - sub_h) / 2.0,
                11.0,
                theme.accent_primary,
            );
            self.hits.push((rect, EndpointHit::Pick(endpoint, index)));
            y += row_pitch;
        }
    }

    fn input_mut(&mut self, endpoint: Endpoint) -> &mut TextInput {
        match endpoint {
            Endpoint::Origin => &mut self.origin_input,
            Endpoint::Destination => &mut self.destination_input,
        }
    }

    fn query_mut(&mut self, endpoint: Endpoint) -> &mut String {
        match endpoint {
            Endpoint::Origin => &mut self.origin_query,
            Endpoint::Destination => &mut self.destination_query,
        }
    }

    fn results_mut(&mut self, endpoint: Endpoint) -> &mut Vec<Place> {
        match endpoint {
            Endpoint::Origin => &mut self.origin_results,
            Endpoint::Destination => &mut self.destination_results,
        }
    }
}

fn endpoint_input(placeholder: &str) -> TextInput {
    let mut input = TextInput::local()
        // Route endpoints are the same structured place/address vocabulary as
        // the top-level map search, never prose to spellcheck or autocorrect.
        .with_kind(TextInputKind::Search)
        .with_enter_submits(true)
        .with_font_size(13.0);
    input.set_plain_chrome(true);
    input.set_placeholder(placeholder);
    input
}

fn vmid(top: f64, height: f64, font_size: f64) -> f64 {
    top + (height - font_size) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn endpoint_editor_owns_its_draft_debounce_dismiss_and_spellcheck() {
        // Phase `external_queries_replace_the_component_draft_without_drift`.
        {
            let mut editor = EndpointEditor::new();
            editor.sync_queries("Old origin", "Old destination");
            editor.sync_queries("Peer origin", "Peer destination");
            assert_eq!(editor.query(Endpoint::Origin), "Peer origin");
            assert_eq!(editor.query(Endpoint::Destination), "Peer destination");
        }

        // Phase `debounce_emits_one_typed_search_for_the_focused_endpoint`.
        {
            let mut editor = EndpointEditor::new();
            editor.focus(Endpoint::Origin);
            editor.origin_query = "station".to_string();
            let now = ui::time::now();
            editor.deadline = Some(now);
            assert_eq!(
                editor.take_due_search(now),
                Some(EndpointEffect::Search {
                    endpoint: Endpoint::Origin,
                    query: "station".to_string(),
                })
            );
            assert!(editor.take_due_search(now).is_none());
        }

        // Phase `dismiss_clears_focus_and_pending_search`.
        {
            let mut editor = EndpointEditor::new();
            editor.focus(Endpoint::Destination);
            editor.deadline = Some(ui::time::now());
            editor.dismiss();
            assert!(!editor.is_focused());
            assert!(!editor.has_pending_query());
        }

        // Phase `route_endpoints_are_not_spellchecked_as_prose`.
        {
            let editor = EndpointEditor::new();
            assert!(!editor.origin_input.spellcheck_enabled());
            assert!(!editor.destination_input.spellcheck_enabled());
        }
    }
}
