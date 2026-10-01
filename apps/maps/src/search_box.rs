//! Top-level place search control.
//!
//! The control owns the shared text field, focus, live-result overlay, debounce
//! timer, and every hot region it paints. `MapsApp` remains responsible for
//! map-domain effects (selecting/recentring a place and starting network work).

use ui::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{TextInput, TextInputKind, Widget};

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
            // Place names, addresses, and category terms are structured search
            // tokens, not prose. This semantic intent disables spelling-error
            // decoration and autocorrection without masking normal text input.
            .with_kind(TextInputKind::Search)
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

    /// When the typed query settles into a live geocode.
    pub(crate) const fn query_deadline(&self) -> Option<Instant> {
        self.debounce
    }

    /// Stable identity for a visible suggestion. The canvas uses this to give
    /// its semantic result node a durable address rather than a frame-local
    /// result index.
    pub(crate) fn result_id(&self, index: usize) -> Option<&str> {
        self.results.get(index).map(|place| place.id.as_str())
    }

    pub(crate) fn result_name(&self, index: usize) -> Option<&str> {
        self.results.get(index).map(|place| place.name.as_str())
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

    /// Close transient type-ahead suggestions without discarding the query.
    /// Escape or a click away returns attention to the map; the query remains
    /// available to edit or submit.
    pub(crate) fn dismiss(&mut self) {
        self.focused = false;
        self.dismiss_results();
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
            self.dismiss();
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
        // Rest edge derived from the field's own surface; focus is the
        // toolkit's 2 px `border_focus` ring, like every text field.
        if self.focused {
            draw::card_ring(
                scene,
                transform,
                theme,
                rect,
                12.0,
                colors.surfaces.raised,
                theme.border_focus,
                2.0,
            );
        } else {
            draw::card(scene, transform, theme, rect, 12.0, colors.surfaces.raised);
        }
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
        overlay: &SearchOverlay<'_>,
    ) {
        if !self.overlay_active(overlay.query) {
            return;
        }
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let x = overlay.rect.x0;
        let w = overlay.rect.width();
        // Rows pitch at the authored 46 px with an `s1` between neighbouring
        // hit rects; the card sits an `s2` under the field with an `s2` pad.
        let row_pitch = 46.0;
        let row_gap = f64::from(sp.s1);
        let pad = f64::from(sp.s2);
        let n = self.results.len().min(MAX_RESULTS);
        let top = overlay.rect.y1 + f64::from(sp.s2);
        let card = Rect::new(
            x,
            top,
            x + w,
            top + pad * 2.0 + n as f64 * row_pitch - row_gap,
        );
        draw::card(scene, transform, theme, card, 12.0, c.surfaces.raised);
        self.targets.push((card, SearchHit::Consume));

        // Name and address stack on their measured lines, the block centred
        // on the row like the marker beside it; the distance sits on the
        // name line's centre.
        let scale = text_renderer.text_scale();
        let name_h = ui::text_metrics::line_height_px(13.0, scale);
        let sub_h = ui::text_metrics::line_height_px(11.0, scale);
        let mut y = top + pad;
        for (index, place) in self.results.iter().enumerate().take(n) {
            let r = Rect::new(x + pad, y, x + w - pad, y + row_pitch - row_gap);
            let name_y = r.y0 + (r.height() - name_h - sub_h) / 2.0;
            if overlay.selected_id == Some(place.id.as_str()) {
                draw::selected_row(scene, transform, r, 8.0, theme);
            }
            // Category remains explicit in each row's text. A different
            // saturated hue for every result makes a short ordered list read
            // like a legend instead of a set of choices.
            let marker = draw::mix(theme.accent_primary, c.surfaces.base, 0.78);
            draw::fill_circle(
                scene,
                transform,
                Point::new(r.x0 + 14.0, r.center().y),
                6.0,
                marker,
            );
            draw::fill_circle(
                scene,
                transform,
                Point::new(r.x0 + 14.0, r.center().y),
                2.4,
                theme.accent_primary,
            );
            let text_x = r.x0 + 30.0;
            let distance = geo::fmt_distance(geo::haversine_m(place.pos, overlay.anchor));
            let distance_w = draw::measure_w(text_renderer, &distance, 11.0) + 8.0;
            draw::text_clip(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                &place.name,
                Point::new(text_x, name_y),
                13.0,
                theme.text_primary,
                r.x1 - text_x - distance_w,
            );
            draw::text_clip(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                &place.address,
                Point::new(text_x, name_y + name_h),
                11.0,
                theme.text_dim,
                r.x1 - text_x - distance_w,
            );
            draw::text_right(
                scene,
                text_renderer,
                transform,
                &distance,
                r.x1 - pad,
                name_y + (name_h - sub_h) / 2.0,
                11.0,
                theme.accent_primary,
            );
            self.targets.push((r, SearchHit::Result(index)));
            y += row_pitch;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn place(id: &str) -> Place {
        Place::new(id, id, Category::Cafe, "", LatLon::new(48.0, 11.0))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_box_debounces_picks_dismisses_and_skips_spellcheck() {
        // Phase `debounce_replaces_results_and_fires_once`.
        {
            let mut search = SearchBox::new();
            let now = ui::time::now();
            search.query_changed("caf", vec![place("local")], now);
            assert_eq!(search.results()[0].id, "local");
            assert!(search.take_due_query(now, "caf").is_none());
            assert_eq!(
                search.take_due_query(now + DEBOUNCE, " caf "),
                Some("caf".into())
            );
            assert!(search.take_due_query(now + DEBOUNCE, "caf").is_none());
        }

        // Phase `picking_is_an_owned_event_effect`.
        {
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

        // Phase `dismiss_keeps_the_query_but_drops_transient_suggestions`.
        {
            let mut search = SearchBox::new();
            let now = ui::time::now();
            search.set_focused(true);
            search.query_changed("museum", vec![place("local")], now);

            search.dismiss();

            assert!(!search.is_focused());
            assert!(search.results().is_empty());
            assert!(search.take_due_query(now + DEBOUNCE, "museum").is_none());
        }

        // Phase `place_search_is_not_spellchecked_as_prose`.
        {
            assert!(
                !SearchBox::new().input.spellcheck_enabled(),
                "place and address terms must not gain spelling-error decoration"
            );
        }
    }
}
