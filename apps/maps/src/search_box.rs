//! Top-level place search control.
//!
//! The control owns the shared text field, focus, live-result overlay, debounce
//! timer, and every hot region it paints. `MapsApp` remains responsible for
//! map-domain effects (selecting/recentring a place and starting network work).

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::canvas;
use crate::draw;
use crate::geo;
use crate::model::{LatLon, Place};

const DEBOUNCE: Duration = Duration::from_millis(280);
const MAX_RESULTS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SearchHit {
    Field,
    Clear,
    Result(usize),
    Consume,
}

#[derive(Debug)]
pub(crate) enum SearchEffect {
    None,
    Clear,
    Pick(Place),
}

pub(crate) enum SearchKeyEffect {
    None,
    Changed(String),
    Submit,
    Dismissed,
}

pub(crate) struct SearchOverlay<'a> {
    pub(crate) rect: Rect,
    pub(crate) query: &'a str,
    pub(crate) selected_id: Option<&'a str>,
    pub(crate) anchor: LatLon,
}

pub(crate) struct SearchBox {
    input: TextInput,
    focused: bool,
    results: Vec<Place>,
    debounce: Option<Instant>,
    targets: Vec<(Rect, SearchHit)>,
}

impl SearchBox {
    pub(crate) fn new() -> Self {
        let mut input = TextInput::local()
            .with_enter_submits(true)
            .with_font_size(15.0);
        input.set_plain_chrome(true);
        input.set_placeholder("Search places, addresses…");
        Self {
            input,
            focused: false,
            results: Vec::new(),
            debounce: None,
            targets: Vec::new(),
        }
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub(crate) fn has_pending_query(&self) -> bool {
        self.debounce.is_some()
    }

    #[cfg(test)]
    pub(crate) fn results(&self) -> &[Place] {
        &self.results
    }

    pub(crate) fn replace_results(&mut self, results: Vec<Place>) {
        self.results = results;
    }

    pub(crate) fn query_changed(&mut self, query: &str, instant_results: Vec<Place>, now: Instant) {
        if query.trim().is_empty() {
            self.results.clear();
            self.debounce = None;
        } else {
            self.results = instant_results;
            self.debounce = Some(now + DEBOUNCE);
        }
    }

    pub(crate) fn take_due_query(&mut self, now: Instant, query: &str) -> Option<String> {
        let deadline = self.debounce?;
        if now < deadline {
            return None;
        }
        self.debounce = None;
        let query = query.trim();
        (!query.is_empty()).then(|| query.to_string())
    }

    pub(crate) fn dismiss_results(&mut self) {
        self.results.clear();
        self.debounce = None;
    }

    pub(crate) fn overlay_active(&self, query: &str) -> bool {
        self.focused && !query.trim().is_empty() && !self.results.is_empty()
    }

    pub(crate) fn handle_hit(&mut self, hit: &SearchHit) -> SearchEffect {
        match hit {
            SearchHit::Field => {
                self.focused = true;
                SearchEffect::None
            }
            SearchHit::Clear => {
                self.dismiss_results();
                SearchEffect::Clear
            }
            SearchHit::Result(index) => {
                let Some(place) = self.results.get(*index).cloned() else {
                    return SearchEffect::None;
                };
                self.dismiss_results();
                self.focused = false;
                SearchEffect::Pick(place)
            }
            SearchHit::Consume => SearchEffect::None,
        }
    }

    pub(crate) fn handle_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) -> SearchKeyEffect {
        if !self.focused {
            return SearchKeyEffect::None;
        }
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
            self.focused = false;
            return SearchKeyEffect::Dismissed;
        }
        self.input.set_focus(true);
        drive_text_input(&mut self.input, event, scale, modifiers);
        if self.input.take_submit() {
            self.focused = false;
            SearchKeyEffect::Submit
        } else if let Some(text) = self.input.take_dirty() {
            SearchKeyEffect::Changed(text)
        } else {
            SearchKeyEffect::None
        }
    }

    pub(crate) fn forward_pointer(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        drive_text_input(&mut self.input, event, scale, modifiers);
    }

    pub(crate) fn forward_press(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
    ) {
        self.input.set_focus(true);
        drive_text_input(&mut self.input, event, scale, modifiers);
    }

    pub(crate) fn take_targets(&mut self) -> Vec<(Rect, SearchHit)> {
        std::mem::take(&mut self.targets)
    }

    pub(crate) fn draw_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        query: &str,
    ) {
        self.targets.clear();
        self.targets.push((rect, SearchHit::Field));
        let colors = &theme.tokens.colors;
        let border = if self.focused {
            theme.border_focus
        } else {
            colors.border_standard
        };
        draw::card(scene, transform, rect, 12.0, colors.surfaces.raised, border);
        let glyph = Point::new(rect.x0 + 22.0, rect.center().y);
        draw::stroke_circle(
            scene,
            transform,
            Point::new(glyph.x, glyph.y - 1.0),
            6.0,
            2.0,
            theme.text_dim,
        );
        draw::line(
            scene,
            transform,
            Point::new(glyph.x + 4.5, glyph.y + 3.5),
            Point::new(glyph.x + 9.0, glyph.y + 8.0),
            2.0,
            theme.text_dim,
        );

        let text_rect = Rect::new(rect.x0 + 36.0, rect.y0, rect.x1 - 34.0, rect.y1);
        self.input.set_text(query);
        self.input.set_focus(self.focused);
        self.input.apply_layout_external(text_rect);
        self.input.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::empty(),
        );

        if !query.is_empty() {
            let clear = Rect::new(
                rect.x1 - 32.0,
                rect.y0 + 10.0,
                rect.x1 - 10.0,
                rect.y1 - 10.0,
            );
            draw::fill_circle(scene, transform, clear.center(), 10.0, colors.surfaces.base);
            let center = clear.center();
            for (a, b) in [
                (
                    Point::new(center.x - 4.0, center.y - 4.0),
                    Point::new(center.x + 4.0, center.y + 4.0),
                ),
                (
                    Point::new(center.x + 4.0, center.y - 4.0),
                    Point::new(center.x - 4.0, center.y + 4.0),
                ),
            ] {
                draw::line(scene, transform, a, b, 1.8, theme.text_dim);
            }
            self.targets.push((clear, SearchHit::Clear));
        }
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "short geometry names stay local to one compact row-layout loop"
    )]
    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        overlay: SearchOverlay<'_>,
    ) {
        if !self.overlay_active(overlay.query) {
            return;
        }
        let c = &theme.tokens.colors;
        let x = overlay.rect.x0;
        let w = overlay.rect.width();
        let row_h = 46.0;
        let pad = 6.0;
        let n = self.results.len().min(MAX_RESULTS);
        let top = overlay.rect.y1 + 6.0;
        let card = Rect::new(x, top, x + w, top + pad * 2.0 + n as f64 * row_h);
        draw::card(
            scene,
            transform,
            card,
            12.0,
            c.surfaces.raised,
            c.border_standard,
        );
        self.targets.push((card, SearchHit::Consume));

        let mut y = top + pad;
        for (index, place) in self.results.iter().enumerate().take(n) {
            let r = Rect::new(x + pad, y, x + w - pad, y + row_h - 2.0);
            if overlay.selected_id == Some(place.id.as_str()) {
                draw::fill_rrect(
                    scene,
                    transform,
                    r,
                    8.0,
                    draw::alpha(theme.accent_primary, 0.12),
                );
            }
            let category = canvas::category_color(theme, place.category);
            draw::fill_circle(
                scene,
                transform,
                Point::new(r.x0 + 14.0, r.center().y),
                6.0,
                category,
            );
            draw::fill_circle(
                scene,
                transform,
                Point::new(r.x0 + 14.0, r.center().y),
                2.4,
                draw::mix(c.surfaces.base, Color::WHITE, 0.7),
            );
            let text_x = r.x0 + 30.0;
            let distance = geo::fmt_distance(geo::haversine_m(place.pos, overlay.anchor));
            let distance_w = draw::measure_w(text_renderer, &distance, 11.0) + 8.0;
            draw::text_clip(
                scene,
                text_renderer,
                transform,
                &place.name,
                text_x,
                r.y0 + 6.0,
                13.0,
                theme.text_primary,
                r.x1 - text_x - distance_w,
            );
            draw::text_clip(
                scene,
                text_renderer,
                transform,
                &place.address,
                text_x,
                r.y0 + 24.0,
                11.0,
                theme.text_dim,
                r.x1 - text_x - distance_w,
            );
            draw::text_right(
                scene,
                text_renderer,
                transform,
                &distance,
                r.x1 - 8.0,
                r.y0 + 8.0,
                11.0,
                theme.accent_primary,
            );
            self.targets.push((r, SearchHit::Result(index)));
            y += row_h;
        }
    }
}

/// Drive a hand-laid local text field through the standard widget event path.
/// Directions reuses this so Maps has one adapter for all three fields.
pub(crate) fn drive_text_input(
    input: &mut TextInput,
    event: &WindowEvent,
    scale: f64,
    modifiers: winit::event::Modifiers,
) {
    let mut doc = AutoCommit::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return;
    };
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    input.handle_event(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn place(id: &str) -> Place {
        Place::new(id, id, Category::Cafe, "", LatLon::new(48.0, 11.0))
    }

    #[test]
    fn debounce_replaces_results_and_fires_once() {
        let mut search = SearchBox::new();
        let now = Instant::now();
        search.query_changed("caf", vec![place("local")], now);
        assert_eq!(search.results()[0].id, "local");
        assert!(search.take_due_query(now, "caf").is_none());
        assert_eq!(
            search.take_due_query(now + DEBOUNCE, " caf "),
            Some("caf".into())
        );
        assert!(search.take_due_query(now + DEBOUNCE, "caf").is_none());
    }

    #[test]
    fn picking_is_an_owned_event_effect() {
        let mut search = SearchBox::new();
        search.set_focused(true);
        search.replace_results(vec![place("first")]);
        let SearchEffect::Pick(picked) = search.handle_hit(&SearchHit::Result(0)) else {
            panic!("result hit must produce a pick effect");
        };
        assert_eq!(picked.id, "first");
        assert!(!search.is_focused());
        assert!(search.results().is_empty());
    }
}
