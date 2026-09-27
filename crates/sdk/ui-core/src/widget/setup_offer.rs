//! Shared "set this up?" offer: the configuration an app recommends setting
//! up, and every other one it supports behind "Show more".
//!
//! One model, [`SetupOffer`], with two presentations that cannot drift: a
//! retained composition ([`SetupOffer::widget`]) for apps built from
//! widgets, and an immediate-mode face ([`SetupOffer::layout`],
//! [`SetupOfferLayout::paint`]) for shell chrome that owns its own geometry
//! and hit-testing. Both show the same copy in the same order: the heading
//! with the recommendation's fit and speed, the question naming what would
//! be set up, its description and terms, then the confirm and "Show more"
//! buttons, and, when shown, each alternative with its own fit, speed, terms
//! and button. Nothing is set up by showing an offer; every button is the
//! caller's confirmed action.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::{Metrics, Spacing, TypeScale};

use super::{Badge, BadgeTone, Button, ButtonFace, Column, Container, Row, Spacer, Text, Widget};

/// A status pill on an offer: its words and tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferPill {
    pub label: String,
    pub tone: BadgeTone,
}

impl OfferPill {
    pub fn new(label: impl Into<String>, tone: BadgeTone) -> Self {
        Self {
            label: label.into(),
            tone,
        }
    }
}

/// One configuration on offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupChoice {
    /// What would be set up, e.g. "Qwen3 4B with llama.cpp on the processor".
    pub title: String,
    /// What it is confirmed against: size, license, memory, speed.
    pub terms: String,
    /// How much room it leaves, and how fast it runs, where known.
    pub pills: Vec<OfferPill>,
}

/// The copy of an offer. Every string is the caller's, already localized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupOffer {
    /// "Recommended for this device".
    pub heading: String,
    /// "Set this up? …", naming the recommendation.
    pub question: String,
    /// What the recommended model is for.
    pub description: String,
    pub recommended: SetupChoice,
    /// The confirm button, e.g. "Set it up…".
    pub set_up: String,
    /// "Show more (3)".
    pub show_more: String,
    /// "Show fewer".
    pub show_fewer: String,
    /// A dismiss button ("Not now"); `None` where the offer stays in place.
    pub dismiss: Option<String>,
    /// "Other ways to run it on this device".
    pub alternatives_title: String,
    /// "Set this up instead…".
    pub instead: String,
    pub alternatives: Vec<SetupChoice>,
}

/// What a press on an offer means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferTarget {
    /// Set up the recommendation.
    SetUp,
    /// Show or hide the alternatives.
    ToggleMore,
    /// Leave the offer.
    Dismiss,
    /// Set up the alternative at this index instead.
    Alternative(usize),
}

/// The retained offer's actions: the caller's own bindings.
pub struct OfferBindings {
    pub set_up: Option<crate::tilo_actions::ActionBinding<()>>,
    /// One per alternative, in order.
    pub alternatives: Vec<Option<crate::tilo_actions::ActionBinding<()>>>,
    /// Show or hide the alternatives: view state only.
    pub toggle: Box<dyn FnMut()>,
}

/// An offer's geometry, all read from the theme's tokens: its type sizes
/// from the type scale, its gaps and paddings from the spacing scale, and
/// its buttons at the pointer-target height plus a step.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    heading: f32,
    detail: f32,
    pill_text: f32,
    button_height: f64,
    button_pad: f64,
    pill_height: f64,
    pill_pad: f64,
    gap: f64,
    row_gap: f64,
}

impl Geometry {
    fn of(spacing: &Spacing, metrics: &Metrics, scale: &TypeScale) -> Self {
        Self {
            heading: scale.base,
            detail: scale.sm,
            pill_text: scale.xs,
            button_height: f64::from(metrics.min_pointer_target + spacing.s2),
            button_pad: f64::from(spacing.s4),
            pill_height: f64::from(spacing.s4 + spacing.s1),
            pill_pad: f64::from(spacing.s3),
            gap: f64::from(spacing.s2),
            row_gap: f64::from(spacing.s3),
        }
    }

    /// The reference geometry, for the retained composition, which is
    /// built before it sees a theme and scales with it afterwards.
    fn reference() -> Self {
        Self::of(&Spacing::BASE, &Metrics::BASE, &TypeScale::BASE)
    }

    fn themed(theme: &Theme) -> Self {
        Self::of(
            &theme.tokens.spacing,
            &theme.tokens.metrics,
            &theme.tokens.typography.scale,
        )
    }

    /// The same offer a step tighter, for a surface short of room: rows a
    /// small step apart and buttons at the bare pointer-target height.
    fn compact(self, theme: &Theme) -> Self {
        Self {
            row_gap: f64::from(theme.tokens.spacing.s2),
            button_height: f64::from(theme.tokens.metrics.min_pointer_target),
            ..self
        }
    }
}

fn f32_of(value: f64) -> f32 {
    crate::num::f32_of_f64(value)
}

impl SetupOffer {
    /// The offer as retained widgets, with the alternatives shown when
    /// `expanded`. `id` prefixes every control id.
    #[must_use]
    pub fn widget(&self, id: &str, expanded: bool, bindings: OfferBindings) -> Box<dyn Widget> {
        let OfferBindings {
            set_up,
            alternatives,
            mut toggle,
        } = bindings;
        let geometry = Geometry::reference();
        let mut rows: Vec<Box<dyn Widget>> = vec![
            Self::titled_row(&self.heading, geometry, &self.recommended.pills),
            Box::new(Text::new(&self.question).with_scaled_size(geometry.heading)),
            Box::new(
                Text::new(&self.description)
                    .with_scaled_size(geometry.detail)
                    .dimmed(),
            ),
            Box::new(
                Text::new(&self.recommended.terms)
                    .with_scaled_size(geometry.detail)
                    .dimmed(),
            ),
        ];
        let button =
            |id: String, label: &str, binding: Option<crate::tilo_actions::ActionBinding<()>>| {
                let button =
                    Button::new(&id, label, None).fit_content(f32_of(geometry.button_height));
                match binding {
                    Some(binding) => button.on_action(binding),
                    None => button.with_enabled(false),
                }
            };
        let mut buttons = Row::new().push(Box::new(
            button(format!("{id}-set-up"), &self.set_up, set_up).primary(),
        ));
        if !self.alternatives.is_empty() {
            buttons = buttons.push(gap(geometry)).push(Box::new(
                Button::new(
                    &format!("{id}-more"),
                    if expanded {
                        &self.show_fewer
                    } else {
                        &self.show_more
                    },
                    None,
                )
                .fit_content(f32_of(geometry.button_height))
                .ghost()
                .on_view(move || toggle()),
            ));
        }
        rows.push(Box::new(buttons.push(Box::new(Spacer::new()))));
        if expanded && !self.alternatives.is_empty() {
            rows.push(Box::new(
                Text::new(&self.alternatives_title).with_scaled_size(geometry.heading),
            ));
            let mut bindings = alternatives.into_iter();
            for (index, choice) in self.alternatives.iter().enumerate() {
                rows.push(Self::titled_row(&choice.title, geometry, &choice.pills));
                rows.push(Box::new(
                    Text::new(&choice.terms)
                        .with_scaled_size(geometry.detail)
                        .dimmed(),
                ));
                rows.push(Box::new(
                    Row::new()
                        .push(Box::new(button(
                            format!("{id}-instead-{index}"),
                            &self.instead,
                            bindings.next().flatten(),
                        )))
                        .push(Box::new(Spacer::new())),
                ));
            }
        }
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(f32_of(geometry.row_gap));
        for row in rows {
            column = column.push(row);
        }
        Box::new(column)
    }

    fn titled_row(title: &str, geometry: Geometry, pills: &[OfferPill]) -> Box<dyn Widget> {
        let mut row = Row::new()
            .push(Box::new(
                Text::new(title).with_scaled_size(geometry.heading),
            ))
            .push(Box::new(Spacer::new()));
        for (index, pill) in pills.iter().enumerate() {
            if index > 0 {
                row = row.push(gap(geometry));
            }
            row = row.push(Box::new(Badge::semantic_pill(
                pill.label.clone(),
                pill.tone,
            )));
        }
        Box::new(row)
    }

    /// Lay the offer out at `origin`, `width` wide, in `theme`'s geometry,
    /// for immediate-mode painting: every line wrapped to the width, every
    /// pill and button placed, and the press targets.
    #[must_use]
    pub fn layout(
        &self,
        origin: Point,
        width: f64,
        expanded: bool,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> SetupOfferLayout {
        self.layout_in(
            origin,
            width,
            expanded,
            Geometry::themed(theme),
            text_renderer,
        )
    }

    /// As [`Self::layout`], a step tighter when the offer would be taller
    /// than `max_height` in the theme's usual geometry.
    #[must_use]
    pub fn layout_fitting(
        &self,
        origin: Point,
        width: f64,
        max_height: f64,
        expanded: bool,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> SetupOfferLayout {
        let usual = Geometry::themed(theme);
        let layout = self.layout_in(origin, width, expanded, usual, text_renderer);
        if layout.rect.height() <= max_height {
            return layout;
        }
        self.layout_in(origin, width, expanded, usual.compact(theme), text_renderer)
    }

    fn layout_in(
        &self,
        origin: Point,
        width: f64,
        expanded: bool,
        geometry: Geometry,
        text_renderer: &mut TextRenderer,
    ) -> SetupOfferLayout {
        let mut placed = Placer {
            origin,
            width,
            y: origin.y,
            parts: Vec::new(),
            geometry,
            text_renderer,
        };
        placed.titled(&self.heading, &self.recommended.pills);
        placed.text(&self.question, geometry.heading, false);
        placed.text(&self.description, geometry.detail, true);
        placed.text(&self.recommended.terms, geometry.detail, true);
        let mut buttons = vec![(OfferTarget::SetUp, self.set_up.as_str(), Look::Primary)];
        if !self.alternatives.is_empty() {
            buttons.push((
                OfferTarget::ToggleMore,
                if expanded {
                    self.show_fewer.as_str()
                } else {
                    self.show_more.as_str()
                },
                Look::Ghost,
            ));
        }
        if let Some(dismiss) = &self.dismiss {
            buttons.push((OfferTarget::Dismiss, dismiss.as_str(), Look::Ghost));
        }
        placed.buttons(&buttons);
        if expanded && !self.alternatives.is_empty() {
            placed.text(&self.alternatives_title, geometry.heading, false);
            for (index, choice) in self.alternatives.iter().enumerate() {
                placed.titled(&choice.title, &choice.pills);
                placed.text(&choice.terms, geometry.detail, true);
                placed.buttons(&[(
                    OfferTarget::Alternative(index),
                    self.instead.as_str(),
                    Look::Plain,
                )]);
            }
        }
        let height = (placed.y - geometry.row_gap - origin.y).max(0.0);
        SetupOfferLayout {
            rect: Rect::new(origin.x, origin.y, origin.x + width, origin.y + height),
            parts: placed.parts,
        }
    }
}

fn gap(geometry: Geometry) -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_size(f32_of(geometry.gap), 1.0),
    )
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Text {
        at: Point,
        text: String,
        size: f32,
        dimmed: bool,
        max_width: f32,
    },
    Pill {
        rect: Rect,
        label: String,
        tone: BadgeTone,
    },
    Button {
        rect: Rect,
        label: String,
        target: OfferTarget,
        style: Look,
    },
}

/// How an offer button reads: the confirm button stands out, "Show more"
/// and "Not now" recede, each alternative's button is an ordinary one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Primary,
    Plain,
    Ghost,
}

struct Placer<'a> {
    origin: Point,
    width: f64,
    y: f64,
    parts: Vec<Part>,
    geometry: Geometry,
    text_renderer: &'a mut TextRenderer,
}

impl Placer<'_> {
    fn advance(&mut self, height: f64) {
        self.y += height + self.geometry.row_gap;
    }

    fn text(&mut self, text: &str, size: f32, dimmed: bool) {
        let max_width = f32_of(self.width);
        let (_, height) = self.text_renderer.measure(text, size, Some(max_width));
        self.parts.push(Part::Text {
            at: Point::new(self.origin.x, self.y),
            text: text.to_owned(),
            size,
            dimmed,
            max_width,
        });
        self.advance(f64::from(height));
    }

    /// A title with its pills at the right edge, the title wrapping in the
    /// room the pills leave.
    fn titled(&mut self, title: &str, pills: &[OfferPill]) {
        let geometry = self.geometry;
        let size = geometry.heading;
        let mut x = self.origin.x + self.width;
        let mut pill_parts = Vec::new();
        for pill in pills.iter().rev() {
            let (label_width, _) =
                self.text_renderer
                    .measure(&pill.label, geometry.pill_text, None);
            let pill_width = f64::from(label_width) + geometry.pill_pad * 2.0;
            let rect = Rect::new(x - pill_width, self.y, x, self.y + geometry.pill_height);
            x = rect.x0 - geometry.gap;
            pill_parts.push(Part::Pill {
                rect,
                label: pill.label.clone(),
                tone: pill.tone,
            });
        }
        let max_width = f32_of((x - self.origin.x).max(self.width / 3.0));
        let (_, height) = self.text_renderer.measure(title, size, Some(max_width));
        self.parts.push(Part::Text {
            at: Point::new(self.origin.x, self.y),
            text: title.to_owned(),
            size,
            dimmed: false,
            max_width,
        });
        self.parts.extend(pill_parts);
        self.advance(f64::from(height).max(geometry.pill_height));
    }

    fn buttons(&mut self, buttons: &[(OfferTarget, &str, Look)]) {
        let geometry = self.geometry;
        let mut x = self.origin.x;
        for (target, label, style) in buttons {
            let (label_width, _) = self.text_renderer.measure_weighted(
                label,
                geometry.heading,
                None,
                TextWeight::Medium,
            );
            let rect = Rect::new(
                x,
                self.y,
                x + f64::from(label_width) + geometry.button_pad * 2.0,
                self.y + geometry.button_height,
            );
            x = rect.x1 + geometry.gap;
            self.parts.push(Part::Button {
                rect,
                label: (*label).to_owned(),
                target: *target,
                style: *style,
            });
        }
        self.advance(geometry.button_height);
    }
}

/// An offer laid out for painting.
#[derive(Clone, Debug, PartialEq)]
pub struct SetupOfferLayout {
    /// The bounds of everything laid out.
    pub rect: Rect,
    parts: Vec<Part>,
}

impl SetupOfferLayout {
    /// Every press target, in paint order.
    #[must_use]
    pub fn targets(&self) -> Vec<(Rect, OfferTarget)> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Button { rect, target, .. } => Some((*rect, *target)),
                _ => None,
            })
            .collect()
    }

    /// The label of `target`'s button, for its accessible name.
    #[must_use]
    pub fn label(&self, target: OfferTarget) -> Option<&str> {
        self.parts.iter().find_map(|part| match part {
            Part::Button {
                target: found,
                label,
                ..
            } if *found == target => Some(label.as_str()),
            _ => None,
        })
    }

    /// Every line of copy, in order, for accessibility and tests.
    #[must_use]
    pub fn texts(&self) -> Vec<&str> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text, .. } | Part::Pill { label: text, .. } => Some(text.as_str()),
                Part::Button { .. } => None,
            })
            .collect()
    }

    /// Paint the offer with the toolkit's own text, pill and button faces.
    /// `hovered` and `focused` say which target is under the pointer and
    /// which holds keyboard focus.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        hovered: Option<OfferTarget>,
        focused: Option<OfferTarget>,
    ) {
        let colors = &theme.tokens.colors;
        for part in &self.parts {
            match part {
                Part::Text {
                    at,
                    text,
                    size,
                    dimmed,
                    max_width,
                } => text_renderer.draw_text(
                    scene,
                    text,
                    *size,
                    if *dimmed {
                        colors.text_dim
                    } else {
                        colors.text_primary
                    },
                    transform.pre_translate(Vec2::new(at.x, at.y)),
                    Some(*max_width),
                ),
                Part::Pill { rect, label, tone } => {
                    let tint = Badge::semantic_pill(label.clone(), *tone).background(theme);
                    Badge::paint_at(scene, transform, *rect, label, tint, text_renderer, theme);
                }
                Part::Button {
                    rect,
                    label,
                    target,
                    style,
                } => {
                    let face = ButtonFace::new(*rect, label)
                        .hovered(hovered == Some(*target))
                        .focused(focused == Some(*target));
                    let face = match style {
                        Look::Primary => face.primary(),
                        Look::Plain => face,
                        Look::Ghost => face.ghost(),
                    };
                    Button::paint(scene, transform, text_renderer, theme, &face);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> SetupOffer {
        let choice = |title: &str| SetupChoice {
            title: title.into(),
            terms: "2.3 GiB download · Apache-2.0 · Fast here".into(),
            pills: vec![
                OfferPill::new("Good fit", BadgeTone::Success),
                OfferPill::new("Fast here", BadgeTone::Success),
            ],
        };
        SetupOffer {
            heading: "Recommended for this device".into(),
            question: "Set this up? Qwen3 4B with vLLM on the card".into(),
            description: "Reviews recordings".into(),
            recommended: choice("Qwen3 4B with vLLM on the card"),
            set_up: "Set it up…".into(),
            show_more: "Show more (2)".into(),
            show_fewer: "Show fewer".into(),
            dismiss: Some("Not now".into()),
            alternatives_title: "Other ways".into(),
            instead: "Set this up instead…".into(),
            alternatives: vec![
                choice("Qwen3 4B with llama.cpp on the iGPU"),
                choice("Qwen3 4B with llama.cpp on the processor"),
            ],
        }
    }

    #[test]
    fn the_face_shows_the_recommendation_and_its_alternatives_only_when_asked() {
        let mut text = TextRenderer::new();
        let collapsed = offer().layout(
            Point::new(10.0, 20.0),
            600.0,
            false,
            &Theme::dark(),
            &mut text,
        );
        assert_eq!(
            collapsed
                .targets()
                .iter()
                .map(|(_, target)| *target)
                .collect::<Vec<_>>(),
            [
                OfferTarget::SetUp,
                OfferTarget::ToggleMore,
                OfferTarget::Dismiss
            ]
        );
        assert!(
            collapsed
                .texts()
                .contains(&"Set this up? Qwen3 4B with vLLM on the card")
        );
        assert!(collapsed.texts().contains(&"Good fit"));
        assert!(
            !collapsed
                .texts()
                .contains(&"Qwen3 4B with llama.cpp on the iGPU")
        );
        assert_eq!(
            collapsed.label(OfferTarget::ToggleMore),
            Some("Show more (2)")
        );

        let expanded = offer().layout(
            Point::new(10.0, 20.0),
            600.0,
            true,
            &Theme::dark(),
            &mut text,
        );
        assert!(expanded.rect.height() > collapsed.rect.height());
        // Short of room, the same offer tightens to the compact steps.
        let short = expanded.rect.height() - 20.0;
        let fitted = offer().layout_fitting(
            Point::new(10.0, 20.0),
            600.0,
            short,
            true,
            &Theme::dark(),
            &mut text,
        );
        assert!(fitted.rect.height() < expanded.rect.height());
        assert_eq!(fitted.targets().len(), expanded.targets().len());
        assert!(
            expanded
                .texts()
                .contains(&"Qwen3 4B with llama.cpp on the processor")
        );
        assert_eq!(expanded.label(OfferTarget::ToggleMore), Some("Show fewer"));
        let targets = expanded.targets();
        assert!(targets.contains(&(targets[3].0, OfferTarget::Alternative(0))));
        assert!(
            targets
                .iter()
                .all(|(rect, _)| expanded.rect.contains(rect.origin())),
            "every target lies inside the laid-out offer"
        );
        let mut scene = Scene::new();
        expanded.paint(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            Some(OfferTarget::SetUp),
            None,
        );
        assert!(!scene.encoding().draw_tags.is_empty());
    }
}
