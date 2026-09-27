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

const HEADING_SIZE: f32 = 14.0;
const TITLE_SIZE: f32 = 13.0;
const DETAIL_SIZE: f32 = 12.0;
const BUTTON_HEIGHT: f64 = 40.0;
const BUTTON_PAD: f64 = 16.0;
const PILL_HEIGHT: f64 = 20.0;
const PILL_PAD: f64 = 10.0;
const GAP: f64 = 8.0;
/// The rhythm between an offer's rows.
const ROW_GAP: f32 = 10.0;

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
        let mut rows: Vec<Box<dyn Widget>> = vec![
            Self::titled_row(&self.heading, HEADING_SIZE, &self.recommended.pills),
            Box::new(Text::new(&self.question).with_scaled_size(TITLE_SIZE)),
            Box::new(
                Text::new(&self.description)
                    .with_scaled_size(DETAIL_SIZE)
                    .dimmed(),
            ),
            Box::new(
                Text::new(&self.recommended.terms)
                    .with_scaled_size(DETAIL_SIZE)
                    .dimmed(),
            ),
        ];
        let button =
            |id: String, label: &str, binding: Option<crate::tilo_actions::ActionBinding<()>>| {
                let button = Button::new(&id, label, None).fit_content(f32_of(BUTTON_HEIGHT));
                match binding {
                    Some(binding) => button.on_action(binding),
                    None => button.with_enabled(false),
                }
            };
        let mut buttons = Row::new().push(Box::new(
            button(format!("{id}-set-up"), &self.set_up, set_up).primary(),
        ));
        if !self.alternatives.is_empty() {
            buttons = buttons.push(gap()).push(Box::new(
                Button::new(
                    &format!("{id}-more"),
                    if expanded {
                        &self.show_fewer
                    } else {
                        &self.show_more
                    },
                    None,
                )
                .fit_content(f32_of(BUTTON_HEIGHT))
                .ghost()
                .on_view(move || toggle()),
            ));
        }
        rows.push(Box::new(buttons.push(Box::new(Spacer::new()))));
        if expanded && !self.alternatives.is_empty() {
            rows.push(Box::new(
                Text::new(&self.alternatives_title).with_scaled_size(TITLE_SIZE),
            ));
            let mut bindings = alternatives.into_iter();
            for (index, choice) in self.alternatives.iter().enumerate() {
                rows.push(Self::titled_row(&choice.title, TITLE_SIZE, &choice.pills));
                rows.push(Box::new(
                    Text::new(&choice.terms)
                        .with_scaled_size(DETAIL_SIZE)
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
            .with_gap(ROW_GAP);
        for row in rows {
            column = column.push(row);
        }
        Box::new(column)
    }

    fn titled_row(title: &str, size: f32, pills: &[OfferPill]) -> Box<dyn Widget> {
        let mut row = Row::new()
            .push(Box::new(Text::new(title).with_scaled_size(size)))
            .push(Box::new(Spacer::new()));
        for (index, pill) in pills.iter().enumerate() {
            if index > 0 {
                row = row.push(gap());
            }
            row = row.push(Box::new(Badge::semantic_pill(
                pill.label.clone(),
                pill.tone,
            )));
        }
        Box::new(row)
    }

    /// Lay the offer out at `origin`, `width` wide, for immediate-mode
    /// painting: every line wrapped to the width, every pill and button
    /// placed, and the press targets.
    #[must_use]
    pub fn layout(
        &self,
        origin: Point,
        width: f64,
        expanded: bool,
        text_renderer: &mut TextRenderer,
    ) -> SetupOfferLayout {
        let mut placed = Placer {
            origin,
            width,
            y: origin.y,
            parts: Vec::new(),
            text_renderer,
        };
        placed.titled(&self.heading, HEADING_SIZE, &self.recommended.pills);
        placed.text(&self.question, TITLE_SIZE, false);
        placed.text(&self.description, DETAIL_SIZE, true);
        placed.text(&self.recommended.terms, DETAIL_SIZE, true);
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
            placed.text(&self.alternatives_title, TITLE_SIZE, false);
            for (index, choice) in self.alternatives.iter().enumerate() {
                placed.titled(&choice.title, TITLE_SIZE, &choice.pills);
                placed.text(&choice.terms, DETAIL_SIZE, true);
                placed.buttons(&[(
                    OfferTarget::Alternative(index),
                    self.instead.as_str(),
                    Look::Plain,
                )]);
            }
        }
        let height = (placed.y - f64::from(ROW_GAP) - origin.y).max(0.0);
        SetupOfferLayout {
            rect: Rect::new(origin.x, origin.y, origin.x + width, origin.y + height),
            parts: placed.parts,
        }
    }
}

fn gap() -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_size(f32_of(GAP), 1.0),
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
    text_renderer: &'a mut TextRenderer,
}

impl Placer<'_> {
    fn advance(&mut self, height: f64) {
        self.y += height + f64::from(ROW_GAP);
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
    fn titled(&mut self, title: &str, size: f32, pills: &[OfferPill]) {
        let mut x = self.origin.x + self.width;
        let mut pill_parts = Vec::new();
        for pill in pills.iter().rev() {
            let (label_width, _) = self.text_renderer.measure(&pill.label, 11.0, None);
            let pill_width = f64::from(label_width) + PILL_PAD * 2.0;
            let rect = Rect::new(x - pill_width, self.y, x, self.y + PILL_HEIGHT);
            x = rect.x0 - GAP;
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
        self.advance(f64::from(height).max(PILL_HEIGHT));
    }

    fn buttons(&mut self, buttons: &[(OfferTarget, &str, Look)]) {
        let mut x = self.origin.x;
        for (target, label, style) in buttons {
            let (label_width, _) =
                self.text_renderer
                    .measure_weighted(label, TITLE_SIZE, None, TextWeight::Medium);
            let rect = Rect::new(
                x,
                self.y,
                x + f64::from(label_width) + BUTTON_PAD * 2.0,
                self.y + BUTTON_HEIGHT,
            );
            x = rect.x1 + GAP;
            self.parts.push(Part::Button {
                rect,
                label: (*label).to_owned(),
                target: *target,
                style: *style,
            });
        }
        self.advance(BUTTON_HEIGHT);
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
        let collapsed = offer().layout(Point::new(10.0, 20.0), 600.0, false, &mut text);
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

        let expanded = offer().layout(Point::new(10.0, 20.0), 600.0, true, &mut text);
        assert!(expanded.rect.height() > collapsed.rect.height());
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
