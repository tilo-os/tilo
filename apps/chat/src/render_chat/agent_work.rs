//! What an agent did for a reply, drawn under the reply's words. While the
//! agent works it is a card of one line, which says the step the agent is at
//! and how many it has taken; once it is done it folds to one line, "Did 3
//! things". Either line opens the list of the operations, each one line, and
//! closes it again: no operation is listed unasked, however many there are.
//! The answer itself is the reply's text, never this line.

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
    /// step the agent is at and how many it has taken while it works, or
    /// that the work did not finish.
    pub(super) summary: Option<String>,
    /// The folded list's line, once the work ended with operations done.
    pub(super) did: Option<String>,
    /// The operations, each one line: only while their list is open.
    pub(super) steps: Vec<String>,
    /// Whether a press opens and closes the list: there are operations.
    pub(super) opens: bool,
}

impl AgentWorkPresentation {
    /// What `work` shows, with its list of operations `open` or folded;
    /// `None` when there is nothing to add to the reply's own words.
    ///
    /// `words` says whether the reply itself carries text. A refused run puts
    /// its failure into that text, so the card does not say the same failure
    /// again: the reply has one failure surface, not two.
    ///
    /// `stopped` says who stopped the reply's turn, when its person did. The
    /// conversation holds their stop from the moment they pressed it, so the
    /// reply says so at once, before whoever works the agent has ended its
    /// work. A reply that ended by itself keeps saying how it ended.
    pub(super) fn of_turn(
        work: &AgentWork,
        open: bool,
        words: bool,
        stopped: Option<&str>,
    ) -> Option<Self> {
        use crate::agent_stream::AgentFinish;
        use crate::i18n::{t, t_args};
        let state = work
            .stream
            .as_ref()
            .and_then(|stream| stream.finish)
            .map_or_else(
                || work.state.clone(),
                |outcome| AgentWorkState::ProviderFinished { outcome },
            );
        // Its person's stop ends the work for every reader, whatever its agent wrote last.
        let state = if stopped.is_some() && !state.finished() {
            AgentWorkState::ProviderFinished {
                outcome: AgentFinish::Cancelled,
            }
        } else {
            state
        };
        let summary = match &state {
            AgentWorkState::Taken => Some(at_work(None, work.trace.len())),
            AgentWorkState::Doing { did, about } => {
                Some(at_work(step_words(did, about), work.trace.len()))
            }
            AgentWorkState::Waiting { app } => {
                Some(t_args("chat-agent-work-waiting", &[("app", app.into())]))
            }
            AgentWorkState::Done { .. } => None,
            AgentWorkState::Refused => (!words).then(|| t("chat-agent-work-refused")),
            AgentWorkState::ProviderFinished { outcome } => match outcome {
                AgentFinish::Complete => None,
                AgentFinish::Truncated => Some(t("chat-agent-truncated")),
                AgentFinish::Refused => Some(t("chat-agent-refused")),
                AgentFinish::Cancelled => {
                    Some(stopped.map_or_else(|| t("chat-agent-cancelled"), str::to_owned))
                }
                AgentFinish::Failed => Some(t("chat-agent-failed")),
            },
        };
        let finished = state.finished();
        let did = (finished && !work.trace.is_empty())
            .then(|| t_args("chat-agent-work-did", &[("count", work.trace.len().into())]));
        let steps = if open {
            work.trace.iter().map(step_line).collect()
        } else {
            Vec::new()
        };
        (summary.is_some() || did.is_some() || !steps.is_empty()).then_some(Self {
            summary,
            did,
            steps,
            opens: !work.trace.is_empty(),
        })
    }

    /// [`Self::of_turn`] of a reply nobody stopped.
    #[cfg(test)]
    pub(super) fn of(work: &AgentWork, open: bool, words: bool) -> Option<Self> {
        Self::of_turn(work, open, words, None)
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

/// The one line of an agent at work: the step it is at, in the words its state
/// says it in (`None` where it says none), and how many steps it has taken.
fn at_work(step: Option<String>, taken: usize) -> String {
    use crate::i18n::{t, t_args};
    let what = step.unwrap_or_else(|| t("chat-agent-working"));
    if taken == 0 {
        return what;
    }
    t_args(
        "chat-agent-work-at-step",
        &[("what", what.into()), ("count", taken.into())],
    )
}

/// A step in words a person reads, with what it is about. A tool's or an
/// action's bare name (`tilo_ls`, `tasks.add`, `find`) is no such words: words
/// have a blank between them, or begin as a label does.
fn step_words(did: &str, about: &str) -> Option<String> {
    let (did, about) = (did.trim(), about.trim());
    let named =
        !did.contains(' ') && (did.contains(['_', '.']) || !did.starts_with(char::is_uppercase));
    if did.is_empty() || named {
        return None;
    }
    if about.is_empty() {
        return Some(did.to_owned());
    }
    Some(crate::i18n::t_args(
        "chat-agent-work-step-about",
        &[("step", did.into()), ("about", about.into())],
    ))
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
/// `words` says whether the reply itself carries text, as it does when drawn,
/// and `stopped` who stopped its turn, when its person did.
pub(crate) fn agent_work_accessible_description(
    work: &AgentWork,
    words: bool,
    stopped: Option<&str>,
) -> String {
    AgentWorkPresentation::of_turn(work, true, words, stopped)
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
    words: bool,
    stopped: Option<&str>,
) -> f64 {
    let Some(shown) = AgentWorkPresentation::of_turn(work, open, words, stopped) else {
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
/// whether it has a list to open, so the caller makes the area open and close it.
pub(super) fn draw_agent_work(
    paint: &mut PaintCtx<'_>,
    work: &AgentWork,
    open: bool,
    area: Rect,
    words: bool,
    stopped: Option<&str>,
) -> bool {
    let Some(shown) = AgentWorkPresentation::of_turn(work, open, words, stopped) else {
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
    shown.opens
}
