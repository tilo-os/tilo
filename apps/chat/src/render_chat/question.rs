//! The question a message asks, drawn under its words in Chat's window: its
//! card as [`crate::question_card`] words it, every answer the shared
//! button, every line read aloud with the message.

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonRole, CardFace, ControlSize};
use vello::kurbo::{Point, Rect};

use crate::app::{ChatApp, Hit};
use crate::model::Message;
use crate::paint::text;
use crate::question::Question;
use crate::question_card::{Line, QuestionShown, Reader, answer_label, shown};

impl ChatApp {
    /// Whether a choice `m` asks is still open: nothing its person wrote in
    /// the conversation came after it.
    fn choice_open(&self, m: &Message, question: &Question) -> bool {
        !self
            .messages
            .iter()
            .rev()
            .take_while(|later| later.id != m.id)
            .any(|later| later.channel == m.channel && question.asks(&later.author))
    }

    /// What `m`'s question shows to this reader; `None` when it asks nothing
    /// or has nothing to say to them.
    pub(crate) fn question_shown(&self, m: &Message) -> Option<QuestionShown> {
        let question = m.question.as_ref()?;
        let name = self.display_name(&question.person);
        shown(
            question,
            &Reader {
                mine: self.is_me(&question.person),
                name: Some(&name),
                open: self.choice_open(m, question),
            },
        )
    }

    /// What the button of `answer` under the message `message` says.
    pub(crate) fn question_answer_label(&self, message: &str, answer: &str) -> String {
        self.message(message)
            .and_then(|m| m.question.as_ref())
            .map_or_else(String::new, |question| answer_label(question, answer))
    }

    /// The height of `m`'s question in a column `width` wide, the gaps
    /// around it included; zero when it shows nothing.
    pub(crate) fn question_height(
        &self,
        m: &Message,
        width: f64,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        self.question_shown(m).map_or(0.0, |shown| {
            lay(&shown, Point::ZERO, width, tr, theme).bottom
        })
    }

    /// [`Self::question_height`] of the row of `m`, which shows no question
    /// while its message is being edited.
    pub(crate) fn row_question_height(
        &self,
        m: &Message,
        editing: bool,
        width: f32,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        if editing {
            0.0
        } else {
            self.question_height(m, f64::from(width), tr, theme)
        }
    }

    /// Paint `m`'s question from `*y` in a column at `x`, `width` wide;
    /// advances `*y` past it.
    pub(crate) fn draw_question(
        &self,
        mut paint: PaintCtx<'_>,
        m: &Message,
        (x, width): (f64, f64),
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(shown) = self.question_shown(m) else {
            return;
        };
        let theme = paint.theme;
        let laid = lay(&shown, Point::new(x, *y), width, paint.text, theme);
        if let Some(card) = laid.card {
            CardFace::new().paint(&mut paint, card);
        }
        for ((line, kind), (at, wrap)) in shown.lines.iter().zip(&laid.lines) {
            let (style, color) = line_style(*kind, theme);
            text(&mut paint, line, &style, color, *at, Some(*wrap));
        }
        for (offered, rect) in shown.answers.iter().zip(&laid.answers) {
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                theme,
                &ButtonFace::new(*rect, &offered.label)
                    .control_size(ControlSize::Small)
                    .role(if offered.primary {
                        ButtonRole::Primary
                    } else {
                        ButtonRole::Secondary
                    })
                    .hovered(self.hovered(*rect)),
            );
            ChatApp::push_hit(
                hits,
                *rect,
                Hit::Answer {
                    msg: m.id.clone(),
                    answer: offered.id.clone(),
                },
            );
        }
        *y = laid.bottom;
    }
}

fn line_style(line: Line, theme: &Theme) -> (ui::TextStyle, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    match line {
        Line::Title => (theme.text(TextRole::Body).strong(), colors.text_primary),
        Line::Field | Line::Status => (theme.text(TextRole::Caption), colors.text_dim),
    }
}

/// Where a question's parts stand.
struct Laid {
    card: Option<Rect>,
    /// Each line's origin and the width it wraps at.
    lines: Vec<(Point, f32)>,
    answers: Vec<Rect>,
    /// Where what follows the question starts.
    bottom: f64,
}

/// Lay `shown` out from `origin` in a column `width` wide: its lines one
/// under another, then its answers in rows that wrap, on a card when it
/// shows what would happen.
fn lay(
    shown: &QuestionShown,
    origin: Point,
    width: f64,
    tr: &mut TextRenderer,
    theme: &Theme,
) -> Laid {
    let sp = &theme.tokens.spacing;
    let gap = f64::from(sp.s2);
    let inset = if shown.card { f64::from(sp.s3) } else { 0.0 };
    let outer = width.clamp(0.0, crate::live_share::max_card_width(theme).max(1.0));
    let inner = (outer - 2.0 * inset).max(1.0);
    let wrap = ui::num::f32_of_f64(inner);
    let top = origin.y + gap;
    let x = origin.x + inset;
    let mut y = top + inset;
    let mut lines = Vec::with_capacity(shown.lines.len());
    for (index, (line, kind)) in shown.lines.iter().enumerate() {
        if index > 0 {
            y += f64::from(sp.s1);
        }
        let (style, _) = line_style(*kind, theme);
        lines.push((Point::new(x, y), wrap));
        y += f64::from(tr.measure_style(line, Some(wrap), &style).1);
    }
    let mut answers = Vec::with_capacity(shown.answers.len());
    if !shown.answers.is_empty() {
        if !shown.lines.is_empty() {
            y += gap;
        }
        let height = ButtonFace::height(ControlSize::Small, theme);
        let between = f64::from(sp.s1);
        let mut at = x;
        for offered in &shown.answers {
            let wide = ButtonFace::new(Rect::ZERO, &offered.label)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme)
                .min(inner);
            // An answer that does not fit beside the others starts a row.
            if at > x && at + wide > x + inner {
                at = x;
                y += height + between;
            }
            answers.push(Rect::new(at, y, at + wide, y + height));
            at += wide + between;
        }
        y += height;
    }
    y += inset;
    Laid {
        card: shown
            .card
            .then(|| Rect::new(origin.x, top, origin.x + outer, y)),
        lines,
        answers,
        bottom: y + gap,
    }
}
