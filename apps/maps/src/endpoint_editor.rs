//! Self-contained From/To editor for the Directions panel.
//!
//! Each endpoint is the toolkit's [`Autocomplete`]: its own field, its own
//! suggestion list, keyboard navigation (arrows, Enter, Escape) and pointer
//! picking. This component owns which field is being edited, the typed
//! drafts, the debounce before a live geocode, and the places each field
//! suggests; the app owns the route and the geocoder.

use ui::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::Widget;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Autocomplete, AutocompleteOutcome, Button, ButtonFace, ButtonRole, ControlSize, Suggestion,
};

use crate::canvas::{self, MapPalette};
use crate::model::Place;

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

pub(super) struct EndpointFrame<'a> {
    pub content: Rect,
    pub top: f64,
    pub origin: Option<&'a str>,
    pub destination: Option<&'a str>,
}

/// One endpoint: its field, its draft and the places it suggests.
struct Field {
    input: Autocomplete,
    query: String,
    results: Vec<Place>,
    rect: Option<Rect>,
    /// Whether the geocoder has answered the current draft.
    searched: bool,
}

impl Field {
    fn new(id: &str, placeholder: &str) -> Self {
        Self {
            // A route endpoint is a structured place/address vocabulary: the
            // autocomplete's field is a search field, never prose to
            // spellcheck or autocorrect.
            input: Autocomplete::new()
                .with_id(id)
                .with_placeholder(placeholder)
                .with_text_role(TextRole::Body)
                .with_max_visible(OVERLAY_MAX),
            query: String::new(),
            results: Vec::new(),
            rect: None,
            searched: false,
        }
    }

    /// Show the places for the current draft, or the state of the search
    /// for them.
    fn show_results(&mut self, editing: bool) {
        let suggestions = self
            .results
            .iter()
            .take(OVERLAY_MAX)
            .map(|place| {
                let detail = if place.address.is_empty() {
                    place.category.label().to_string()
                } else {
                    place.address.clone()
                };
                Suggestion::new(place.name.clone()).with_detail(detail)
            })
            .collect();
        self.input.set_suggestions_open(suggestions);
        if editing && self.results.is_empty() && !self.query.trim().is_empty() {
            let status = if self.searched {
                "No places found"
            } else {
                "Searching…"
            };
            self.input.set_status(Some(status.to_string()));
            self.input.open();
        }
    }
}

pub(super) struct EndpointEditor {
    origin: Field,
    destination: Field,
    focused: Option<Endpoint>,
    deadline: Option<Instant>,
    swap_rect: Option<Rect>,
    /// The pointer, for the hover state of the swap button.
    pointer: Point,
}

impl EndpointEditor {
    pub(super) fn new() -> Self {
        Self {
            origin: Field::new("maps_route_origin", "Choose starting point"),
            destination: Field::new("maps_route_destination", "Choose destination"),
            focused: None,
            deadline: None,
            swap_rect: None,
            pointer: Point::ZERO,
        }
    }

    fn field(&self, endpoint: Endpoint) -> &Field {
        match endpoint {
            Endpoint::Origin => &self.origin,
            Endpoint::Destination => &self.destination,
        }
    }

    fn field_mut(&mut self, endpoint: Endpoint) -> &mut Field {
        match endpoint {
            Endpoint::Origin => &mut self.origin,
            Endpoint::Destination => &mut self.destination,
        }
    }

    /// Adopt persisted/external query state. The host mirrors every Changed
    /// effect immediately, so this is idempotent during ordinary local edits
    /// and authoritative after a document reload.
    pub(super) fn sync_queries(&mut self, origin: &str, destination: &str) {
        for (endpoint, query) in [
            (Endpoint::Origin, origin),
            (Endpoint::Destination, destination),
        ] {
            let focused = self.focused == Some(endpoint);
            let field = self.field_mut(endpoint);
            if field.query != query {
                query.clone_into(&mut field.query);
                if focused {
                    field.input.set_text(query);
                }
            }
        }
    }

    /// The places the field suggests for its draft: local matches at once,
    /// then the geocoder's.
    pub(super) fn set_results(&mut self, endpoint: Endpoint, results: Vec<Place>) {
        let editing = self.focused == Some(endpoint);
        let field = self.field_mut(endpoint);
        field.results = results;
        field.show_results(editing);
    }

    /// The geocoder's answer for the field's draft (empty when it found
    /// nothing or failed).
    pub(super) fn finish_search(&mut self, endpoint: Endpoint, results: Vec<Place>) {
        self.field_mut(endpoint).searched = true;
        self.set_results(endpoint, results);
    }

    pub(super) fn dismiss(&mut self) {
        self.focused = None;
        self.deadline = None;
        for field in [&mut self.origin, &mut self.destination] {
            field.input.close();
            field.input.set_focus(false);
        }
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
        self.focused
            .is_some_and(|endpoint| self.field(endpoint).input.is_open())
    }

    pub(super) fn take_due_search(&mut self, now: Instant) -> Option<EndpointEffect> {
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        self.deadline = None;
        let endpoint = self.focused?;
        let query = self.field(endpoint).query.trim().to_string();
        (!query.is_empty()).then_some(EndpointEffect::Search { endpoint, query })
    }

    /// Route a key to the field being edited.
    pub(super) fn handle_key(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> Option<EndpointEffect> {
        let endpoint = self.focused?;
        drive(
            &mut self.field_mut(endpoint).input,
            event,
            scale,
            modifiers,
            doc,
        );
        self.reconcile(endpoint).or(Some(EndpointEffect::Handled))
    }

    /// Keep both fields' hover state current.
    pub(super) fn forward_pointer(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        drive(&mut self.origin.input, event, scale, modifiers, doc);
        drive(&mut self.destination.input, event, scale, modifiers, doc);
    }

    /// A press: the swap button, a field taking focus, or the field being
    /// edited picking a suggestion (or letting go when the press is
    /// elsewhere, which the caller then handles).
    pub(super) fn pointer_press(
        &mut self,
        point: Point,
        event: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> Option<EndpointEffect> {
        if self.swap_rect.is_some_and(|rect| rect.contains(point)) {
            self.dismiss();
            return Some(EndpointEffect::Swap);
        }
        if let Some(endpoint) = self.focused {
            let picked = {
                let field = self.field_mut(endpoint);
                drive(&mut field.input, event, scale, modifiers, doc);
                field.input.take_outcome()
            };
            match picked {
                AutocompleteOutcome::Accepted(value) => return self.accept(endpoint, &value),
                AutocompleteOutcome::Cancelled => self.dismiss(),
                AutocompleteOutcome::None => {
                    if self
                        .field(endpoint)
                        .rect
                        .is_some_and(|rect| rect.contains(point))
                    {
                        return Some(EndpointEffect::Handled);
                    }
                }
            }
        }
        for endpoint in [Endpoint::Origin, Endpoint::Destination] {
            if self
                .field(endpoint)
                .rect
                .is_some_and(|rect| rect.contains(point))
            {
                self.focus(endpoint);
                drive(
                    &mut self.field_mut(endpoint).input,
                    event,
                    scale,
                    modifiers,
                    doc,
                );
                return Some(EndpointEffect::Changed {
                    endpoint,
                    text: String::new(),
                });
            }
        }
        None
    }

    /// Fold the field's state into an effect: a changed draft, an accepted
    /// suggestion (or a typed query to search), or a cancelled edit.
    fn reconcile(&mut self, endpoint: Endpoint) -> Option<EndpointEffect> {
        let field = self.field_mut(endpoint);
        let text = field.input.text().to_string();
        let outcome = field.input.take_outcome();
        match outcome {
            AutocompleteOutcome::Accepted(value) => return self.accept(endpoint, &value),
            AutocompleteOutcome::Cancelled => {
                self.dismiss();
                return Some(EndpointEffect::Handled);
            }
            AutocompleteOutcome::None => {}
        }
        if text == field.query {
            return None;
        }
        text.clone_into(&mut field.query);
        field.searched = false;
        self.deadline = if text.trim().is_empty() {
            None
        } else {
            Some(ui::time::now() + SEARCH_DEBOUNCE)
        };
        Some(EndpointEffect::Changed { endpoint, text })
    }

    /// The field accepted `value`: a suggested place picks it; anything else
    /// is a query to search for now.
    fn accept(&mut self, endpoint: Endpoint, value: &str) -> Option<EndpointEffect> {
        let field = self.field_mut(endpoint);
        if let Some(place) = field.results.iter().find(|p| p.name == value).cloned() {
            field.results.clear();
            self.dismiss();
            return Some(EndpointEffect::Pick { endpoint, place });
        }
        self.deadline = None;
        let query = value.trim().to_string();
        (!query.is_empty()).then_some(EndpointEffect::Search { endpoint, query })
    }

    #[cfg(test)]
    pub(super) fn test_field_rect(&self, endpoint: Endpoint) -> Option<Rect> {
        self.field(endpoint).rect
    }

    #[cfg(test)]
    pub(super) fn test_swap_rect(&self) -> Option<Rect> {
        self.swap_rect
    }

    #[cfg(test)]
    pub(super) fn focused_endpoint(&self) -> Option<Endpoint> {
        self.focused
    }

    #[cfg(test)]
    pub(super) fn test_results(&self, endpoint: Endpoint) -> &[Place] {
        &self.field(endpoint).results
    }

    #[cfg(test)]
    pub(super) fn test_query(&self, endpoint: Endpoint) -> &str {
        &self.field(endpoint).query
    }

    fn focus(&mut self, endpoint: Endpoint) {
        self.focused = Some(endpoint);
        self.deadline = None;
        let other = match endpoint {
            Endpoint::Origin => Endpoint::Destination,
            Endpoint::Destination => Endpoint::Origin,
        };
        let other = self.field_mut(other);
        other.input.close();
        other.input.set_focus(false);
        let field = self.field_mut(endpoint);
        field.query.clear();
        field.results.clear();
        field.searched = false;
        field.input.set_text("");
        field.input.begin_edit();
    }

    /// How far the fields stand in from the content's leading edge: past
    /// the endpoint markers, so what refines a field lines up with it.
    pub(super) fn field_inset(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_md) + f64::from(theme.tokens.spacing.s2)
    }

    /// Paint the From and To fields — the toolkit's own fields, each after
    /// the marker it has on the map — and the swap button beside them.
    /// Returns the y below them.
    pub(super) fn draw_fields(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &EndpointFrame<'_>,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let content = frame.content;
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        let marker_w = f64::from(theme.tokens.metrics.icon_md);
        let side = field_h;
        let x0 = content.x0 + Self::field_inset(theme);
        let x1 = content.x1 - side - s2;
        let pal = MapPalette::from_theme(theme);

        let origin_rect = Rect::new(x0, frame.top, x1, frame.top + field_h);
        let destination_y = origin_rect.y1 + s2;
        let destination_rect = Rect::new(x0, destination_y, x1, destination_y + field_h);
        for (endpoint, rect, label, destination) in [
            (Endpoint::Origin, origin_rect, frame.origin, false),
            (
                Endpoint::Destination,
                destination_rect,
                frame.destination,
                true,
            ),
        ] {
            let centre = Point::new(content.x0 + marker_w / 2.0, rect.center().y);
            canvas::draw_endpoint_marker(scene, transform, centre, &pal, destination);
            let editing = self.focused == Some(endpoint);
            let field = self.field_mut(endpoint);
            field.rect = Some(rect);
            // A field shows its endpoint's name until it is being edited.
            if !editing && field.input.text() != label.unwrap_or("") {
                field.input.set_text(label.unwrap_or(""));
            }
            // This field is placed outside the retained layout tree. Give it
            // the resolved theme role so its type follows whole-UI scaling,
            // just like the measured field band and the suggestions do.
            field
                .input
                .input_mut()
                .set_content_size(Some(theme.text(ui::tokens::TextRole::Body).size));
            field.input.apply_layout_external(rect);
            field
                .input
                .draw(scene, transform, text, theme, &PresenceCtx::empty());
        }

        let swap = Rect::new(
            content.x1 - side,
            destination_rect.y0,
            content.x1,
            destination_rect.y1,
        );
        self.swap_rect = Some(swap);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(swap, Icon::ArrowUpDown, "Swap start and destination")
                .role(ButtonRole::Plain)
                .hovered(swap.contains(self.pointer)),
        );
        destination_rect.y1 + s3
    }

    /// The suggestions of the field being edited, in the toolkit's own list
    /// under it.
    pub(super) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(endpoint) = self.focused else { return };
        self.field_mut(endpoint).input.draw_overlay(
            scene,
            transform,
            text,
            theme,
            &PresenceCtx::empty(),
        );
    }
}

/// Drive one event through a hand-placed field: a press inside a field that
/// is not being edited starts the edit first, so the caret lands where the
/// press was.
fn drive(
    input: &mut Autocomplete,
    event: &WindowEvent,
    scale: f64,
    modifiers: winit::event::Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) {
    if let WindowEvent::MouseInput {
        state: ElementState::Pressed,
        button: MouseButton::Left,
        ..
    } = event
        && !input.is_editing()
    {
        return;
    }
    crate::hand_laid::drive(input, event, scale, modifiers, doc);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, LatLon};

    #[test]
    fn endpoint_editor_owns_its_draft_debounce_dismiss_and_suggestions() {
        // Phase `external_queries_replace_the_component_draft_without_drift`.
        {
            let mut editor = EndpointEditor::new();
            editor.sync_queries("Old origin", "Old destination");
            editor.sync_queries("Peer origin", "Peer destination");
            assert_eq!(editor.test_query(Endpoint::Origin), "Peer origin");
            assert_eq!(editor.test_query(Endpoint::Destination), "Peer destination");
        }

        // Phase `debounce_emits_one_typed_search_for_the_focused_endpoint`.
        {
            let mut editor = EndpointEditor::new();
            editor.focus(Endpoint::Origin);
            editor.origin.query = "station".to_string();
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

        // Phase `accepting_a_suggestion_picks_its_place`.
        {
            let mut editor = EndpointEditor::new();
            editor.focus(Endpoint::Destination);
            let place = Place::new(
                "hbf",
                "Hauptbahnhof",
                Category::Transit,
                "Bahnhofplatz 2",
                LatLon::new(48.14, 11.56),
            );
            editor.set_results(Endpoint::Destination, vec![place.clone()]);
            assert!(editor.overlay_active(), "the suggestions open");
            assert_eq!(
                editor.accept(Endpoint::Destination, "Hauptbahnhof"),
                Some(EndpointEffect::Pick {
                    endpoint: Endpoint::Destination,
                    place,
                })
            );
            assert!(!editor.is_focused(), "picking ends the edit");
            // A typed value nothing suggested is a query to search for.
            editor.focus(Endpoint::Origin);
            assert_eq!(
                editor.accept(Endpoint::Origin, "Ostbahnhof"),
                Some(EndpointEffect::Search {
                    endpoint: Endpoint::Origin,
                    query: "Ostbahnhof".to_string(),
                })
            );
        }
    }
}
