//! What an agent did for a reply, drawn under the reply's words. While the
//! agent works it is a card that says so and lists each operation as it is
//! dispatched; once it is done it folds to one line — "Did 3 things" — that
//! opens the list. The answer itself is the reply's text, never this line.

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::CardFace;
use vello::kurbo::{Point, Rect};

use crate::model::{AgentStepOutcome, AgentWork, AgentWorkState};
use crate::paint::text;

/// What a reply shows of its agent's work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentWorkPresentation {
    /// The state, while it says something the reply's text does not: the
    /// agent is still working, or the work did not finish.
    pub(super) summary: Option<String>,
    /// The folded list's line, once the work ended with operations done.
    pub(super) did: Option<String>,
    /// The operations shown: every one while the agent works, the folded
    /// list's only when it is open.
    pub(super) steps: Vec<String>,
}

impl AgentWorkPresentation {
    /// What `work` shows, with its list of operations `open` or folded;
    /// `None` when there is nothing to add to the reply's own words.
    pub(super) fn of(work: &AgentWork, open: bool) -> Option<Self> {
        use crate::i18n::{t, t_args};
        let state = work
            .stream
            .as_ref()
            .and_then(|stream| stream.finish)
            .map_or_else(
                || work.state.clone(),
                |outcome| AgentWorkState::ProviderFinished { outcome },
            );
        let summary = match &state {
            AgentWorkState::Taken | AgentWorkState::Doing { .. } => Some(t("chat-agent-working")),
            AgentWorkState::Waiting { app } => {
                Some(t_args("chat-agent-work-waiting", &[("app", app.into())]))
            }
            AgentWorkState::Done { .. } => None,
            AgentWorkState::Refused => Some(t("chat-agent-work-refused")),
            AgentWorkState::ProviderFinished { outcome } => {
                use crate::agent_stream::AgentFinish;
                match outcome {
                    AgentFinish::Complete => None,
                    AgentFinish::Truncated => Some(t("chat-agent-truncated")),
                    AgentFinish::Refused => Some(t("chat-agent-refused")),
                    AgentFinish::Cancelled => Some(t("chat-agent-cancelled")),
                    AgentFinish::Failed => Some(t("chat-agent-failed")),
                }
            }
        };
        let finished = state.finished();
        let did = (finished && !work.trace.is_empty())
            .then(|| t_args("chat-agent-work-did", &[("count", work.trace.len().into())]));
        let steps = if finished && !open {
            Vec::new()
        } else {
            work.trace.iter().map(step_line).collect()
        };
        (summary.is_some() || did.is_some() || !steps.is_empty()).then_some(Self {
            summary,
            did,
            steps,
        })
    }

    /// Whether it is drawn as a card: the work is still under way.
    fn card(&self) -> bool {
        self.did.is_none()
    }

    fn lines(&self) -> impl Iterator<Item = (&str, Line)> {
        self.summary
            .iter()
            .map(|summary| (summary.as_str(), Line::Summary))
            .chain(self.did.iter().map(|did| (did.as_str(), Line::Did)))
            .chain(self.steps.iter().map(|step| (step.as_str(), Line::Step)))
    }
}

/// One operation, in its app's words, with what came of it.
fn step_line(step: &crate::model::AgentStep) -> String {
    use crate::i18n::t_args;
    let target = || step.label.clone().into();
    match &step.outcome {
        AgentStepOutcome::Asked => {
            t_args("chat-agent-work-step-requested", &[("target", target())])
        }
        AgentStepOutcome::Applied => {
            t_args("chat-agent-work-step-applied", &[("target", target())])
        }
        AgentStepOutcome::NoChange => {
            t_args("chat-agent-work-step-no-change", &[("target", target())])
        }
        AgentStepOutcome::Refused { why } if why.trim().is_empty() => {
            t_args("chat-agent-work-step-refused", &[("target", target())])
        }
        AgentStepOutcome::Refused { why } => t_args(
            "chat-agent-work-step-refused-why",
            &[("target", target()), ("why", why.into())],
        ),
    }
}

#[derive(Clone, Copy)]
enum Line {
    Summary,
    Did,
    Step,
}

fn line_style(line: Line, theme: &Theme) -> (ui::TextStyle, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    match line {
        Line::Summary => (theme.text(TextRole::Body).strong(), colors.text_primary),
        Line::Did => (theme.text(TextRole::Caption).strong(), colors.text_dim),
        Line::Step => (theme.text(TextRole::Caption), colors.text_dim),
    }
}

/// Everything the work says, read aloud with the reply: the folded list too.
pub(crate) fn agent_work_accessible_description(work: &AgentWork) -> String {
    AgentWorkPresentation::of(work, true)
        .map(|shown| {
            shown
                .lines()
                .map(|(line, _)| line)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// The inset around the lines: a card's padding, none for the folded line.
fn inset(shown: &AgentWorkPresentation, theme: &Theme) -> f64 {
    if shown.card() {
        f64::from(theme.tokens.spacing.s3)
    } else {
        0.0
    }
}

/// The height `work` takes in a column `width` wide, gap below included;
/// zero when it shows nothing.
pub(super) fn measure_agent_work(
    tr: &mut TextRenderer,
    theme: &Theme,
    work: &AgentWork,
    open: bool,
    width: f32,
) -> f64 {
    let Some(shown) = AgentWorkPresentation::of(work, open) else {
        return 0.0;
    };
    let inset = inset(&shown, theme);
    let gap = f64::from(theme.tokens.spacing.s2);
    let text_width = (width - ui::num::f32_of_f64(2.0 * inset)).max(0.0);
    let mut height = 2.0 * inset;
    for (index, (line, kind)) in shown.lines().enumerate() {
        if index > 0 {
            height += gap;
        }
        let (style, _) = line_style(kind, theme);
        height += f64::from(tr.measure_style(line, Some(text_width), &style).1);
    }
    height + gap
}

/// Paint `work` into `area`, as [`measure_agent_work`] measured it. Returns
/// whether it folds, so the caller makes the area open and close it.
pub(super) fn draw_agent_work(
    paint: &mut PaintCtx<'_>,
    work: &AgentWork,
    open: bool,
    area: Rect,
) -> bool {
    let Some(shown) = AgentWorkPresentation::of(work, open) else {
        return false;
    };
    let theme = paint.theme;
    let gap = f64::from(theme.tokens.spacing.s2);
    let inset = inset(&shown, theme);
    if shown.card() {
        CardFace::new().paint(paint, area);
    }
    let width = ui::num::f32_of_f64((area.width() - 2.0 * inset).max(0.0));
    let mut y = area.y0 + inset;
    for (index, (line, kind)) in shown.lines().enumerate() {
        if index > 0 {
            y += gap;
        }
        let (style, color) = line_style(kind, paint.theme);
        y += f64::from(
            text(
                paint,
                line,
                &style,
                color,
                Point::new(area.x0 + inset, y),
                Some(width),
            )
            .1,
        );
    }
    shown.did.is_some()
}
