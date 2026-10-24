//! The window search's options: a floating Panel under the open field with
//! the find's options (Match case, Whole word, and Regular expression or
//! Text / Hex where the document offers them) and, for a document that can
//! replace, the replacement with Replace and Replace all.
//!
//! The search owns what the panel's choices set and does what they ask; the
//! panel lays itself out by hand under the field, paints in the window's top
//! layer and says which choice a point is on. Every row is a whole-row
//! target at the panel's row height, so a finger hits it as surely as a
//! pointer does.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use crate::motion::Appearing;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::find_bar::{FindAbilities, FindMode, FindOptions};
use crate::widget::floating_surface;
use crate::widget::{
    Button, ButtonFace, Checkbox, CheckboxFace, ControlSize, SearchField, SegmentedFace,
    ToggleGroup, Widget,
};

use super::tr;

/// One choice in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Choice {
    MatchCase,
    WholeWord,
    Regex,
    Text,
    Hex,
    Replace,
    ReplaceAll,
}

impl Choice {
    /// Every choice, in the panel's order.
    pub(super) const ALL: [Self; 7] = [
        Self::MatchCase,
        Self::WholeWord,
        Self::Regex,
        Self::Text,
        Self::Hex,
        Self::Replace,
        Self::ReplaceAll,
    ];

    /// The choice's action target, for assistive technology and agents.
    pub(super) fn target(self) -> &'static str {
        match self {
            Self::MatchCase => "window-search-match-case",
            Self::WholeWord => "window-search-whole-word",
            Self::Regex => "window-search-regex",
            Self::Text => "window-search-text",
            Self::Hex => "window-search-hex",
            Self::Replace => "window-search-replace",
            Self::ReplaceAll => "window-search-replace-all",
        }
    }

    pub(super) fn label(self) -> String {
        tr(match self {
            Self::MatchCase => "find-bar-match-case",
            Self::WholeWord => "find-bar-whole-word",
            Self::Regex => "find-bar-regex",
            Self::Text => "find-bar-text",
            Self::Hex => "find-bar-hex",
            Self::Replace => "find-bar-replace",
            Self::ReplaceAll => "find-bar-replace-all",
        })
    }

    /// Whether a document offering `abilities` shows this choice.
    pub(super) fn offered(self, abilities: FindAbilities) -> bool {
        match self {
            Self::MatchCase | Self::WholeWord => true,
            Self::Regex => abilities.regex,
            Self::Text | Self::Hex => abilities.hex,
            Self::Replace | Self::ReplaceAll => abilities.replace,
        }
    }

    /// Whether the choice reads as on under `options`; `None` for the
    /// replace buttons, which are no state.
    pub(super) fn on(self, options: FindOptions) -> Option<bool> {
        match self {
            Self::MatchCase => Some(options.match_case),
            Self::WholeWord => Some(options.whole_word),
            Self::Regex => Some(options.mode == FindMode::Regex),
            Self::Text => Some(options.mode != FindMode::Hex),
            Self::Hex => Some(options.mode == FindMode::Hex),
            Self::Replace | Self::ReplaceAll => None,
        }
    }

    /// `options` with this choice applied; unchanged for the replace
    /// buttons.
    pub(super) fn apply(self, mut options: FindOptions) -> FindOptions {
        match self {
            Self::MatchCase => options.match_case = !options.match_case,
            Self::WholeWord => options.whole_word = !options.whole_word,
            Self::Regex => {
                options.mode = if options.mode == FindMode::Regex {
                    FindMode::Text
                } else {
                    FindMode::Regex
                };
            }
            Self::Text => options.mode = FindMode::Text,
            Self::Hex => options.mode = FindMode::Hex,
            Self::Replace | Self::ReplaceAll => {}
        }
        options
    }
}

/// The panel under the open field.
pub(super) struct OptionsPanel {
    open: bool,
    appearing: Appearing,
    /// The options control the panel comes from.
    anchor: Rect,
    rect: Rect,
    choices: Vec<(Choice, Rect)>,
    /// The replacement, when the document can replace.
    pub(super) replace_field: SearchField,
    replace_rect: Rect,
    hover: Option<Choice>,
    /// The option boxes, filling as a click checks them.
    faces: crate::widget::FaceMotions,
}

impl OptionsPanel {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            appearing: Appearing::closed(),
            anchor: Rect::ZERO,
            rect: Rect::ZERO,
            choices: Vec::new(),
            // The search has one close control: the field clears with the
            // keyboard, not with an X of its own.
            replace_field: SearchField::new()
                .with_icon(crate::icons::Icon::Pencil)
                .with_placeholder(&tr("find-bar-replace-placeholder"))
                .without_clear()
                .with_id("window-search-replacement"),
            replace_rect: Rect::ZERO,
            hover: None,
            faces: crate::widget::FaceMotions::default(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close it; a closed panel takes no input.
    pub(super) fn set_open(&mut self, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        self.hover = None;
        self.appearing.show(open);
        if !open {
            self.replace_field.set_focused(false);
        }
    }

    /// The panel as laid out, while open.
    pub(super) fn rect(&self) -> Option<Rect> {
        self.open.then_some(self.rect)
    }

    /// Whether `point` is on the open panel.
    pub(super) fn contains(&self, point: Point) -> bool {
        self.open && self.rect.contains(point)
    }

    /// The choice at `point` on the open panel.
    pub(super) fn choice_at(&self, point: Point) -> Option<Choice> {
        if !self.open {
            return None;
        }
        self.choices
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(choice, _)| *choice)
    }

    /// Whether `point` is on the replacement field.
    pub(super) fn on_replace_field(&self, point: Point) -> bool {
        self.open && self.replace_rect.contains(point)
    }

    pub(super) fn set_hover(&mut self, point: Option<Point>) {
        self.hover = point.and_then(|point| self.choice_at(point));
    }

    /// Where `choice` stands, while the panel is open and shows it.
    pub(super) fn choice_rect(&self, choice: Choice) -> Option<Rect> {
        self.choices
            .iter()
            .find(|(shown, _)| *shown == choice)
            .map(|(_, rect)| *rect)
            .filter(|_| self.open)
    }

    /// The replacement field's rect, while the panel shows it.
    pub(super) fn replace_field_rect(&self) -> Option<Rect> {
        (self.open && self.replace_rect.area() > 0.0).then_some(self.replace_rect)
    }

    /// Lay the panel out under `field`, coming from the options control
    /// `anchor`, within `span` (the bar's reach, left to right), for a
    /// document offering `abilities`.
    pub(super) fn layout(
        &mut self,
        field: Rect,
        anchor: Rect,
        span: (f64, f64),
        abilities: FindAbilities,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.anchor = anchor;
        self.choices.clear();
        self.replace_rect = Rect::ZERO;
        let pad = floating_surface::menu_inset(theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let row = f64::from(theme.tokens.metrics.panel_row_height);
        let control = ControlSize::Regular.height(theme);
        let label_style = theme.text(TextRole::Body);
        let label_x =
            pad + f64::from(theme.tokens.spacing.s3) + crate::widget::checkbox::BOX_EDGE + gap;
        // The narrowest that shows every label whole.
        let toggles: Vec<Choice> = [Choice::MatchCase, Choice::WholeWord, Choice::Regex]
            .into_iter()
            .filter(|choice| choice.offered(abilities))
            .collect();
        let mut width = field.width();
        for choice in &toggles {
            let (w, _) = text.measure_style(&choice.label(), None, &label_style);
            width = width.max(label_x + f64::from(w) + f64::from(theme.tokens.spacing.s4));
        }
        let mode_labels = [Choice::Text.label(), Choice::Hex.label()];
        let mode_labels = [mode_labels[0].as_str(), mode_labels[1].as_str()];
        if abilities.hex {
            let w = SegmentedFace::intrinsic_width(&mode_labels, ControlSize::Regular, text, theme);
            width = width.max(w + 2.0 * pad);
        }
        let button_w = |choice: Choice, text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, &choice.label())
                .control_size(ControlSize::Regular)
                .intrinsic_width(text, theme)
        };
        let (replace_w, replace_all_w) = if abilities.replace {
            (
                button_w(Choice::Replace, text),
                button_w(Choice::ReplaceAll, text),
            )
        } else {
            (0.0, 0.0)
        };
        if abilities.replace {
            width = width.max(replace_w + gap + replace_all_w + 2.0 * pad);
        }
        let (lo, hi) = span;
        let width = width.min((hi - lo).max(field.width()));
        // Under the field, from its leading edge, kept inside the bar's reach.
        let x0 = field.x0.min(hi - width).max(lo);
        let x1 = x0 + width;
        let mut y = field.y1 + f64::from(theme.tokens.spacing.s1) + pad;
        let y0 = y - pad;
        for choice in toggles {
            self.choices
                .push((choice, Rect::new(x0 + pad, y, x1 - pad, y + row)));
            y += row;
        }
        if abilities.hex {
            y += gap;
            let strip = Rect::new(x0 + pad, y, x1 - pad, y + control);
            let rects =
                SegmentedFace::partition(strip, &mode_labels, ControlSize::Regular, text, theme);
            for (choice, rect) in [Choice::Text, Choice::Hex].into_iter().zip(rects) {
                self.choices.push((choice, rect));
            }
            y += control;
        }
        if abilities.replace {
            y += gap;
            self.replace_rect = Rect::new(x0 + pad, y, x1 - pad, y + control);
            self.replace_field.apply_layout_external(self.replace_rect);
            y += control + gap;
            let all = Rect::new(x1 - pad - replace_all_w, y, x1 - pad, y + control);
            let one = Rect::new(all.x0 - gap - replace_w, y, all.x0 - gap, y + control);
            self.choices.push((Choice::Replace, one));
            self.choices.push((Choice::ReplaceAll, all));
            y += control;
        }
        self.rect = Rect::new(x0, y0, x1, y + pad);
    }

    /// Whether it is moving (arriving or leaving).
    /// Its painted switches ask the motion clock for their own frames.
    pub(super) fn is_moving(&self) -> bool {
        self.appearing.is_moving()
    }

    /// Whether it must be painted again: moving, or a switch not yet
    /// painted at rest.
    pub(super) fn needs_redraw(&self) -> bool {
        self.is_moving() || self.faces.needs_redraw(crate::motion::frame_now())
    }

    /// Whether anything of it is drawn.
    pub(super) fn is_drawn(&self) -> bool {
        self.appearing.is_drawn()
    }

    /// Paint the panel in the window's top layer, as `shown` says.
    pub(super) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        shown: Shown,
    ) {
        self.appearing.follow(self.open, theme.motion());
        if !self.appearing.is_drawn() {
            return;
        }
        let pose = self.appearing.pose(self.rect, self.anchor.center());
        let bounds = floating_surface::painted_bounds(self.rect, theme);
        let rect = self.rect;
        let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
        let choices = ChoicesFace {
            choices: &self.choices,
            hover: self.hover,
            shown,
            faces: &self.faces,
        };
        let replace_rect = self.replace_rect;
        let field = &mut self.replace_field;
        pose.paint(scene, transform, bounds, |scene, transform| {
            floating_surface::paint(scene, transform, rect, theme);
            paint_choices(scene, transform, text, &on_panel, &choices);
            if replace_rect.area() > 0.0 {
                field.draw(scene, transform, text, &on_panel, presence);
            }
        });
    }
}

/// What the panel shows of its owner's state: the options as set, and how
/// many matches there are to replace (the replace buttons are disabled
/// without one).
#[derive(Clone, Copy, Debug)]
pub(super) struct Shown {
    pub(super) options: FindOptions,
    pub(super) matches: usize,
}

/// The choices as painted: where each stands, which the pointer is over,
/// and what they show.
struct ChoicesFace<'a> {
    choices: &'a [(Choice, Rect)],
    hover: Option<Choice>,
    shown: Shown,
    faces: &'a crate::widget::FaceMotions,
}

/// Paint the panel's choices: the toggles as checkbox rows, Text / Hex as
/// one segmented strip, the replace buttons.
fn paint_choices(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    face: &ChoicesFace<'_>,
) {
    let ChoicesFace {
        choices,
        hover,
        shown: Shown { options, matches },
        faces,
    } = *face;
    let modes: Vec<(Choice, Rect)> = choices
        .iter()
        .copied()
        .filter(|(choice, _)| matches!(choice, Choice::Text | Choice::Hex))
        .collect();
    if !modes.is_empty() {
        let rects: Vec<Rect> = modes.iter().map(|(_, rect)| *rect).collect();
        let labels: Vec<String> = modes.iter().map(|(choice, _)| choice.label()).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = modes
            .iter()
            .position(|(choice, _)| choice.on(options) == Some(true))
            .unwrap_or(0);
        let hovered = modes.iter().position(|(choice, _)| hover == Some(*choice));
        let face = SegmentedFace::new(&rects, &labels, selected)
            .control_size(ControlSize::Regular)
            .hovered(hovered);
        ToggleGroup::paint(scene, transform, text, theme, &face);
    }
    let style = theme.text(TextRole::Body);
    for &(choice, rect) in choices {
        let hovered = hover == Some(choice);
        match choice {
            Choice::Text | Choice::Hex => {}
            Choice::MatchCase | Choice::WholeWord | Choice::Regex => {
                let ink = if hovered {
                    floating_surface::paint_armed_row(
                        scene,
                        transform,
                        rect,
                        floating_surface::menu_inset(theme),
                        theme,
                    )
                } else {
                    theme.tokens.colors.text_primary
                };
                let box_x = rect.x0 + f64::from(theme.tokens.spacing.s3);
                let check = Rect::new(
                    box_x,
                    rect.center().y - crate::widget::checkbox::BOX_EDGE / 2.0,
                    box_x + crate::widget::checkbox::BOX_EDGE,
                    rect.center().y + crate::widget::checkbox::BOX_EDGE / 2.0,
                );
                let on = choice.on(options) == Some(true);
                let face = CheckboxFace::new(check, on).pose(faces.check(
                    choice,
                    on,
                    crate::widget::CheckGlyph::Check,
                    theme.motion(),
                ));
                Checkbox::paint(scene, transform, theme, &face);
                let label = choice.label();
                let (_, lh) = text.measure_style(&label, None, &style);
                let ty =
                    crate::text_metrics::centered_text_top(rect.y0, rect.height(), f64::from(lh));
                let tx = check.x1 + f64::from(theme.tokens.spacing.s2);
                text.draw_style(
                    scene,
                    &label,
                    ink,
                    transform.pre_translate(Vec2::new(tx, ty)),
                    None,
                    &style,
                );
            }
            Choice::Replace | Choice::ReplaceAll => {
                let label = choice.label();
                let face = ButtonFace::new(rect, &label)
                    .control_size(ControlSize::Regular)
                    .hovered(hovered)
                    .disabled(matches == 0);
                Button::paint(scene, transform, text, theme, &face);
            }
        }
    }
}
