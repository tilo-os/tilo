//! An agent's work, drawn as one line in the conversation: what it is doing,
//! how far along, and what a screen reader hears for it. The line is a fact
//! about the thread, not a message, so it is measured and painted here rather
//! than borrowed from the message column.

use ui::TextRole;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::model::{AgentRoute, AgentStepOutcome, AgentWork, AgentWorkState};
use crate::paint::text;
use ui::paint::PaintCtx;
use vello::kurbo::Point;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentWorkLineStyle {
    Summary,
    Rationale,
    Step,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentWorkLine {
    pub(super) text: String,
    pub(super) style: AgentWorkLineStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentWorkPresentation {
    pub(super) summary: String,
    pub(super) details: Vec<AgentWorkLine>,
}

/// The same recorded evidence read aloud in the conversation tree.
pub(crate) fn agent_work_accessible_description(work: &AgentWork) -> String {
    let presentation = AgentWorkPresentation::from(work);
    std::iter::once(presentation.summary)
        .chain(presentation.details.into_iter().map(|line| line.text))
        .collect::<Vec<_>>()
        .join("; ")
}

impl From<&AgentWork> for AgentWorkPresentation {
    fn from(work: &AgentWork) -> Self {
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
            AgentWorkState::ProviderFinished { outcome } => {
                use crate::agent_stream::AgentFinish;
                t(match outcome {
                    AgentFinish::Complete => "chat-agent-complete",
                    AgentFinish::Truncated => "chat-agent-truncated",
                    AgentFinish::Refused => "chat-agent-refused",
                    AgentFinish::Cancelled => "chat-agent-cancelled",
                    AgentFinish::Failed => "chat-agent-failed",
                })
            }
            AgentWorkState::Taken => t("chat-agent-work-taken"),
            AgentWorkState::Waiting { app } => {
                t_args("chat-agent-work-waiting", &[("app", app.into())])
            }
            AgentWorkState::Doing { did, about } if about.trim().is_empty() => {
                t_args("chat-agent-work-intent", &[("action", did.into())])
            }
            AgentWorkState::Doing { did, about } => t_args(
                "chat-agent-work-intent-about",
                &[("action", did.into()), ("about", about.into())],
            ),
            AgentWorkState::Done { did } => {
                t_args("chat-agent-work-done", &[("action", did.into())])
            }
            AgentWorkState::Refused => t("chat-agent-work-refused"),
        };
        let mut details = Vec::new();
        if let Some(reasoning) = &work.reasoning {
            let route = match reasoning.route {
                AgentRoute::Model => t("chat-agent-work-model-rationale"),
                AgentRoute::Floor => t("chat-agent-work-rule-match"),
            };
            let text = if reasoning.why.trim().is_empty() {
                match reasoning.route {
                    AgentRoute::Model => t("chat-agent-work-model-choice"),
                    AgentRoute::Floor => t("chat-agent-work-rule-choice"),
                }
            } else {
                t_args(
                    "chat-agent-work-rationale",
                    &[("route", route.into()), ("why", (&reasoning.why).into())],
                )
            };
            details.push(AgentWorkLine {
                text,
                style: AgentWorkLineStyle::Rationale,
            });
        }
        for step in &work.trace {
            // The dispatch record retains the exact resource address for the
            // action reader. The conversation uses the app's readable label.
            let target = step.label.clone();
            let text = match &step.outcome {
                AgentStepOutcome::Asked => t_args(
                    "chat-agent-work-step-requested",
                    &[("target", target.into())],
                ),
                AgentStepOutcome::Applied => {
                    t_args("chat-agent-work-step-applied", &[("target", target.into())])
                }
                AgentStepOutcome::NoChange => t_args(
                    "chat-agent-work-step-no-change",
                    &[("target", target.into())],
                ),
                AgentStepOutcome::Refused { why } if why.trim().is_empty() => {
                    t_args("chat-agent-work-step-refused", &[("target", target.into())])
                }
                AgentStepOutcome::Refused { why } => t_args(
                    "chat-agent-work-step-refused-why",
                    &[("target", target.into()), ("why", why.into())],
                ),
            };
            details.push(AgentWorkLine {
                text,
                style: AgentWorkLineStyle::Step,
            });
        }
        Self { summary, details }
    }
}

pub(super) fn agent_work_text_style(style: AgentWorkLineStyle, theme: &Theme) -> ui::TextStyle {
    match style {
        AgentWorkLineStyle::Summary => theme.text(TextRole::Body).strong(),
        AgentWorkLineStyle::Rationale | AgentWorkLineStyle::Step => theme.text(TextRole::Caption),
    }
}

pub(super) fn measure_agent_work(
    tr: &mut TextRenderer,
    theme: &Theme,
    work: &AgentWork,
    width: f32,
) -> f64 {
    let presentation = AgentWorkPresentation::from(work);
    let inset = f64::from(theme.tokens.spacing.s3);
    let gap = f64::from(theme.tokens.spacing.s2);
    let text_width = (width - ui::num::f32_of_f64(2.0 * inset)).max(0.0);
    let mut height = 2.0 * inset;
    height += f64::from(
        tr.measure_style(
            &presentation.summary,
            Some(text_width),
            &agent_work_text_style(AgentWorkLineStyle::Summary, theme),
        )
        .1,
    );
    for line in presentation.details {
        height += gap;
        height += f64::from(
            tr.measure_style(
                &line.text,
                Some(text_width),
                &agent_work_text_style(line.style, theme),
            )
            .1,
        );
    }
    height + gap
}

pub(super) fn draw_agent_work_line(
    paint: &mut PaintCtx<'_>,
    value: &str,
    style: AgentWorkLineStyle,
    at: Point,
    width: f32,
) -> f64 {
    let text_style = agent_work_text_style(style, paint.theme);
    let color = match style {
        AgentWorkLineStyle::Summary => paint.theme.tokens.colors.text_primary,
        AgentWorkLineStyle::Rationale | AgentWorkLineStyle::Step => {
            paint.theme.tokens.colors.text_dim
        }
    };
    f64::from(text(paint, value, &text_style, color, at, Some(width)).1)
}
