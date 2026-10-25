//! The message box beside the peek: the last few lines of the direct
//! conversation with the person, faded, over a field that sends the next.
//!
//! It is a frosted floating surface, so the work behind it still reads
//! through. It holds the keyboard only once it is pressed; Enter hands the
//! text to the host, which sends it through Chat like any other direct
//! message, and Escape lets go of it and the peek.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::keyboard::{Key, ModifiersState};

use ui::TextRole;
use ui::icons::Icon;
use ui::motion::Appear;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::{ButtonFace, ButtonRole, ControlSize, FieldFace, TextInput, Widget as _};

use super::MessageLine;
use crate::i18n::t_args;
use crate::text_entry::{self, Edit, Mods};

/// Lines of the conversation shown over the field.
pub(super) const SHOWN_LINES: usize = 3;
/// The longest message the box takes, in characters.
const MAX_CHARS: usize = 2000;

/// What a key did to the message box.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Keyed {
    Ignored,
    Consumed,
    /// Escape: let go of the box and the peek.
    Cancel,
    /// Enter on a message: send it.
    Send(String),
}

/// What one frame of the message box shows.
pub(super) struct ComposerModel<'a> {
    pub host: Rect,
    /// The peek's card, which the box stands under (or over, without room).
    pub peek: Rect,
    /// How open the peek is: the box arrives with it, a little behind.
    pub opening: f64,
    pub name: &'a str,
    pub lines: &'a [MessageLine],
    pub unavailable: Option<&'a str>,
    pub cursor: Option<Point>,
}

#[derive(Default)]
pub(super) struct Composer {
    /// Whose conversation the box is for.
    person: Option<String>,
    text: String,
    selection: Selection,
    focused: bool,
    input: Option<TextInput>,
    card: Rect,
    pub(super) field: Rect,
    send: Rect,
}

/// The box's height in `theme`: its lines and its field.
pub(super) fn height(theme: &Theme) -> f64 {
    let pad = f64::from(theme.tokens.spacing.s3);
    let line = f64::from(theme.text(TextRole::Caption).line_height_px());
    pad + line * ui::num::f64_of_usize(SHOWN_LINES)
        + f64::from(theme.tokens.spacing.s2)
        + FieldFace::height(ControlSize::Regular, theme)
        + pad
}

impl Composer {
    /// The box is for `person` from now on: another person's draft is not
    /// carried over.
    pub(super) fn reset_for(&mut self, person: &str) {
        if self.person.as_deref() != Some(person) {
            self.person = Some(person.to_owned());
            self.clear();
            self.focused = false;
        }
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.selection = Selection::collapsed(0);
    }

    pub(super) fn is_focused(&self) -> bool {
        self.focused
    }

    /// Whether the box keeps the peek open whatever the pointer does: it
    /// holds the keyboard, or a draft.
    pub(super) fn holds_peek(&self) -> bool {
        self.focused || !self.text.trim().is_empty()
    }

    pub(super) fn blur(&mut self) {
        self.focused = false;
        if let Some(input) = self.input.as_mut() {
            input.set_focus(false);
        }
    }

    pub(super) fn covers(&self, point: Point) -> bool {
        self.person.is_some() && self.card.contains(point)
    }

    pub(super) fn field_contains(&self, point: Point) -> bool {
        self.person.is_some() && self.field.contains(point) && !self.send.contains(point)
    }

    /// A press on the field: it takes the keyboard, the caret at the end.
    pub(super) fn focus(&mut self) {
        self.focused = true;
        self.selection = Selection::collapsed(self.text.len());
    }

    /// Where Send is, while there is something to send.
    pub(super) fn send_rect(&self) -> Option<Rect> {
        (self.send.area() > 0.0 && !self.text.trim().is_empty()).then_some(self.send)
    }

    /// The message typed, to send; `None` with nothing typed. The box keeps
    /// it until the host says it went ([`Self::clear`]).
    pub(super) fn text_to_send(&self) -> Option<String> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        Some(text)
    }

    pub(super) fn next_wake_deadline(&self) -> Option<Instant> {
        self.input
            .as_ref()
            .filter(|_| self.focused)
            .and_then(ui::widget::Widget::next_wake_deadline)
    }

    pub(super) fn commit_text(&mut self, text: &str) -> bool {
        if !self.focused {
            return false;
        }
        text_entry::commit(text, &mut self.text, &mut self.selection);
        self.bound();
        true
    }

    fn bound(&mut self) {
        if self.text.chars().count() > MAX_CHARS {
            self.text = self.text.chars().take(MAX_CHARS).collect();
            self.selection = Selection::collapsed(self.text.len());
        }
    }

    pub(super) fn key(&mut self, key: &Key, mods: ModifiersState) -> Keyed {
        if !self.focused {
            return Keyed::Ignored;
        }
        match text_entry::apply(
            key,
            Mods::from_state(mods),
            &mut self.text,
            &mut self.selection,
        ) {
            Edit::Submit => self.text_to_send().map_or(Keyed::Consumed, Keyed::Send),
            Edit::Cancel => {
                self.blur();
                Keyed::Cancel
            }
            Edit::Changed => {
                self.bound();
                Keyed::Consumed
            }
            // The box holds the keyboard: a key it does not use still must
            // not reach the window behind.
            Edit::ConsumedQuiet | Edit::Ignored => Keyed::Consumed,
        }
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &ComposerModel<'_>,
    ) {
        if model.peek.area() <= 0.0 {
            self.card = Rect::ZERO;
            self.field = Rect::ZERO;
            self.send = Rect::ZERO;
            return;
        }
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s3);
        let gap = super::peek::gap(theme);
        let h = height(theme);
        let below = model.peek.y1 + gap + h <= model.host.y1 - pad;
        let card = if below {
            Rect::new(
                model.peek.x0,
                model.peek.y1 + gap,
                model.peek.x1,
                model.peek.y1 + gap + h,
            )
        } else {
            Rect::new(
                model.peek.x0,
                model.peek.y0 - gap - h,
                model.peek.x1,
                model.peek.y0 - gap,
            )
        };
        self.card = card;
        // It follows the peek in, a beat behind.
        let progress = ui::motion::stage(model.opening, 0.25, 1.0);
        let origin = Point::new(card.center().x, if below { card.y0 } else { card.y1 });
        let pose = Appear::pose(progress, origin, theme.motion());
        let placed = pose.applied_to(transform);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            placed,
            &ui::widget::floating_surface::painted_bounds(card, theme),
        );
        // Frosted and a touch see-through: the work behind still reads.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            0.92,
            placed,
            &card.inflate(40.0, 40.0),
        );
        ui::widget::floating_surface::paint(scene, placed, card, theme);
        scene.pop_layer();
        let inner = card.inset(-pad);
        Self::paint_lines(scene, placed, text, theme, inner, model.lines, model.name);
        let field_h = FieldFace::height(ControlSize::Regular, theme);
        let field = Rect::new(inner.x0, inner.y1 - field_h, inner.x1, inner.y1);
        match model.unavailable {
            Some(reason) => {
                self.field = Rect::ZERO;
                self.send = Rect::ZERO;
                self.focused = false;
                let style = theme.text(TextRole::Caption);
                let max = ui::num::f32_of_f64(field.width());
                let shown = text.elide_to_width_style(reason, max, &style);
                let (_, h) = text.measure_style(&shown, None, &style);
                text.draw_style(
                    scene,
                    &shown,
                    theme.tokens.colors.text_dim,
                    placed.pre_translate((field.x0, field.center().y - f64::from(h) / 2.0).into()),
                    None,
                    &style,
                );
            }
            None => self.paint_field(scene, placed, text, theme, field, model),
        }
        scene.pop_layer();
    }

    fn paint_lines(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        lines: &[MessageLine],
        name: &str,
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Caption);
        let line_h = f64::from(style.line_height_px());
        let shown = lines.len().min(SHOWN_LINES);
        let start = lines.len().saturating_sub(shown);
        // The newest line sits on the field; older ones fade upward.
        for (k, line) in lines.iter().skip(start).enumerate() {
            let slot = SHOWN_LINES.saturating_sub(shown).saturating_add(k);
            let y = inner.y0 + line_h * ui::num::f64_of_usize(slot);
            let age = shown.saturating_sub(k).saturating_sub(1);
            let alpha = match age {
                0 => 0.9,
                1 => 0.62,
                _ => 0.38,
            };
            let who = if line.mine {
                crate::i18n::t("people-overlay-you")
            } else {
                name.to_owned()
            };
            let said = format!("{who}: {}", line.text.replace('\n', " "));
            let shown_text =
                text.elide_to_width_style(&said, ui::num::f32_of_f64(inner.width()), &style);
            text.draw_style(
                scene,
                &shown_text,
                colors.text_primary.multiply_alpha(alpha),
                transform.pre_translate((inner.x0, y).into()),
                None,
                &style,
            );
        }
        if lines.is_empty() {
            let empty = t_args("people-overlay-no-messages", &[("name", name.into())]);
            let y = inner.y0 + line_h * ui::num::f64_of_usize(SHOWN_LINES - 1);
            let shown_text =
                text.elide_to_width_style(&empty, ui::num::f32_of_f64(inner.width()), &style);
            text.draw_style(
                scene,
                &shown_text,
                colors.text_tertiary,
                transform.pre_translate((inner.x0, y).into()),
                None,
                &style,
            );
        }
    }

    fn paint_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        field: Rect,
        model: &ComposerModel<'_>,
    ) {
        self.field = field;
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let inset = (field.height() - control) / 2.0;
        self.send = Rect::new(
            field.x1 - inset - control,
            field.y0 + inset,
            field.x1 - inset,
            field.y1 - inset,
        );
        let placeholder = t_args(
            "people-overlay-message-placeholder",
            &[("name", model.name.into())],
        );
        let hovered = model.cursor.is_some_and(|p| field.contains(p));
        let face = FieldFace::new(field, &self.text)
            .placeholder(&placeholder)
            .hovered(hovered)
            .focused(self.focused);
        if self.focused && !self.text.is_empty() {
            face.paint_chrome(scene, transform, theme);
        } else {
            face.paint(scene, transform, text, theme);
        }
        if self.focused {
            let mut text_rect = face.text_rect(theme);
            text_rect.x1 = text_rect.x1.min(self.send.x0 - 4.0);
            let input = self.input.get_or_insert_with(|| {
                TextInput::new(Vec::new())
                    .with_plain_chrome()
                    .with_text_role(TextRole::Body)
            });
            input.set_text(&self.text);
            input.set_selection(self.selection.anchor, self.selection.cursor);
            input.set_focus(true);
            input.apply_layout_external(text_rect);
            let input_theme = theme
                .clone()
                .with_text_scale(theme.text_scale * theme.ui_scale);
            input.draw(
                scene,
                transform,
                text,
                &input_theme,
                &PresenceCtx::default(),
            );
        }
        let send_label = crate::i18n::t("people-overlay-send");
        let ready = !self.text.trim().is_empty();
        ui::widget::Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(self.send, Icon::Send, &send_label)
                .role(if ready {
                    ButtonRole::Primary
                } else {
                    ButtonRole::Plain
                })
                .hovered(model.cursor.is_some_and(|p| self.send.contains(p))),
        );
    }
}
