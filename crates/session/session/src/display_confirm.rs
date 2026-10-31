//! "Keep this display change?" on its sheet, whatever opened the question.
//!
//! A display change can leave the person unable to see or reach their screen.
//! Two places stage one: a layout tool on the privileged display socket,
//! whose risky change waits inside the compositor ([`DisplayEvent::Confirm`]),
//! and Settings, whose change waits in its settings
//! ([`DisplayTransaction`](settings_store::hardware::DisplayTransaction)).
//! Both are one question here, on the sheet of
//! [`tilo_sheets::display_confirm`]: one seat, one question, one place that
//! answers it. The question stands on every display that is on, so it is
//! seen even when the change blanked another one, and the countdown is read
//! again on every settle pass: nothing counts down on a timer of its own.
//!
//! An answer goes back where the change waits: the compositor keeps or
//! reverts the tool's change, and the settings transaction is asked to
//! confirm or revert itself, the same path Settings' own Keep and Revert
//! take.

use std::time::{Duration, Instant};

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use tilo_compositor::{Compositor, DisplayEvent};
use tilo_desktop_surface::DesktopNotifications;
use tilo_desktop_surface::notifications::Notification;
use tilo_sheets::Seat as _;
use tilo_sheets::display_confirm::{self, ConfirmEffect, ConfirmSeat};

use crate::display_settings::{self, Displays};
use crate::i18n::{t, t_args};
use ui::i18n::ArgValue;

/// The change the question stands for.
enum Change {
    /// A tool's change, waiting inside the compositor.
    Tool {
        id: u64,
        /// The executable that made it, when the compositor read it.
        tool: Option<DisplayTool>,
        /// When the previous layout comes back without an answer.
        deadline: Instant,
    },
    /// A change Settings staged, waiting in its settings.
    Settings {
        id: u64,
        /// When the previous layout comes back without an answer, in
        /// milliseconds since the Unix epoch.
        deadline: Option<u64>,
    },
}

/// The question that stands now.
struct Asked {
    change: Change,
    /// An answer was taken and is being recorded: the buttons wait.
    answering: bool,
}

/// The one question about a display change, and the answers it takes.
#[derive(Default)]
pub(crate) struct Confirmation {
    asked: Option<Asked>,
}

impl Confirmation {
    /// Follow every display change waiting for the person, in this pass of
    /// the loop: hear what the compositor reports, record what was answered
    /// on the sheet, and keep the question current. One question stands,
    /// whichever of the two places asked.
    pub(crate) fn follow(
        &mut self,
        compositor: &mut Compositor,
        seat: &ConfirmSeat,
        displays: &Displays,
        notices: &DesktopNotifications,
    ) {
        for event in compositor.take_display_events() {
            self.hear(event, notices);
        }
        for effect in seat.take_asked() {
            self.answer(effect, compositor, displays);
        }
        self.follow_settings(displays);
        self.tell(seat);
    }

    /// What the compositor reported about display tools' changes.
    fn hear(&mut self, event: DisplayEvent, notices: &DesktopNotifications) {
        match event {
            DisplayEvent::Confirm { id, tool, within } => {
                let now = Instant::now();
                self.asked = Some(Asked {
                    change: Change::Tool {
                        id,
                        tool: tool.map(|path| DisplayTool::new(path.to_string_lossy())),
                        deadline: now.checked_add(within).unwrap_or(now),
                    },
                    answering: false,
                });
            }
            DisplayEvent::Reverted { id } => {
                if self.asks_about_tool(id) {
                    self.asked = None;
                }
            }
            DisplayEvent::Kept { .. } => {
                // A safe change is kept without asking, and a tool may make
                // one while another change's question stands: only a change
                // kept in answer to this question takes it down.
                if self.asked.as_ref().is_some_and(|asked| {
                    asked.answering && matches!(asked.change, Change::Tool { .. })
                }) {
                    self.asked = None;
                }
            }
            DisplayEvent::RefusedReapply { tool } => {
                let tool = tool.map(|path| DisplayTool::new(path.to_string_lossy()));
                report_refused(tool.as_ref(), notices);
            }
        }
    }

    /// Whether the question standing asks about the tool's change `id`.
    fn asks_about_tool(&self, id: u64) -> bool {
        self.asked.as_ref().is_some_and(
            |asked| matches!(asked.change, Change::Tool { id: open, .. } if open == id),
        )
    }

    /// Ask about the change Settings staged while nothing else is asked, and
    /// take the question down once that change stopped waiting: answered
    /// elsewhere, or its time ran out and the previous layout came back.
    fn follow_settings(&mut self, displays: &Displays) {
        let waiting = displays.awaiting_confirmation();
        let mut stopped = false;
        if let Some(asked) = self.asked.as_mut()
            && let Change::Settings { id, deadline } = &mut asked.change
        {
            match waiting {
                Some((open, at)) if open == *id => {
                    *deadline = at;
                    return;
                }
                _ => stopped = true,
            }
        }
        if stopped {
            self.asked = None;
            return;
        }
        if self.asked.is_some() {
            return;
        }
        if let Some((id, deadline)) = waiting {
            self.asked = Some(Asked {
                change: Change::Settings { id, deadline },
                answering: false,
            });
        }
    }

    /// Record the person's answer on the change they were asked about.
    fn answer(&mut self, effect: ConfirmEffect, compositor: &mut Compositor, displays: &Displays) {
        let keep = effect != ConfirmEffect::Revert;
        let recorded = {
            let Some(asked) = self.asked.as_mut() else {
                return;
            };
            if asked.answering {
                return;
            }
            asked.answering = true;
            match &asked.change {
                Change::Tool { id, tool, .. } => {
                    if effect == ConfirmEffect::KeepAndTrust {
                        trust(tool.as_ref(), compositor);
                    }
                    if keep {
                        if !compositor.keep_display_change(*id) {
                            tracing::info!(id = *id, "the display question was answered elsewhere");
                        }
                    } else if let Err(error) = compositor.revert_display_change(*id) {
                        tracing::warn!(%error, "the display change was not reverted");
                    }
                    true
                }
                Change::Settings { id, .. } => match displays.answer(*id, keep) {
                    Ok(recorded) => recorded,
                    Err(error) => {
                        // The answer did not stick: the question stays until
                        // the change's own time puts the previous layout back.
                        tracing::warn!(%error, "the display answer was not kept");
                        false
                    }
                },
            }
        };
        if recorded {
            self.asked = None;
        } else if let Some(asked) = self.asked.as_mut() {
            asked.answering = false;
        }
    }

    /// Say the question, or take it down. The countdown is read from the
    /// change's deadline here, on every pass: it is the loop's own wait that
    /// brings the next reading, never a clock the question runs.
    fn tell(&self, seat: &ConfirmSeat) {
        let presentation = seat.presentation();
        let Some(asked) = &self.asked else {
            if presentation.is_drawn() {
                presentation.close();
                presentation.gone();
            }
            return;
        };
        let seconds = match &asked.change {
            Change::Tool { deadline, .. } => {
                let left = deadline.saturating_duration_since(Instant::now());
                Some(seconds_left(left))
            }
            Change::Settings { deadline, .. } => deadline.map(|deadline| {
                seconds_left(Duration::from_millis(
                    deadline.saturating_sub(display_settings::unix_now_ms()),
                ))
            }),
        };
        let tool = match &asked.change {
            Change::Tool { tool, .. } => tool.as_ref().map(|tool| tool.name().to_owned()),
            Change::Settings { .. } => None,
        };
        seat.asking(
            tool,
            display_confirm::status(seconds, asked.answering),
            asked.answering,
        );
        if !presentation.is_drawn() {
            presentation.open(false);
        }
    }
}

/// Whole seconds in `left`, rounded up so the last second is shown as one
/// rather than as none.
fn seconds_left(left: Duration) -> u64 {
    left.as_millis()
        .div_ceil(1000)
        .try_into()
        .unwrap_or(u64::MAX)
}

/// The person keeps this tool's changes from now on: remembered in their
/// settings and given to the compositor before the change is kept, so the
/// next one is not asked about.
fn trust(tool: Option<&DisplayTool>, compositor: &mut Compositor) {
    let Some(tool) = tool else {
        return;
    };
    if let Err(error) = TrustedDisplayTools::trust(tool.clone()) {
        tracing::warn!(%error, tool = tool.executable, "the display tool is not trusted");
    }
    compositor.set_trusted_display_tools(display_settings::trusted_tools());
}

/// Tell the person `tool` put back a layout they had just reverted, and was
/// refused instead of being asked again.
fn report_refused(tool: Option<&DisplayTool>, notices: &DesktopNotifications) {
    let title = tool.map_or_else(
        || t("display-tool-reapply-refused-unknown"),
        |tool| {
            t_args(
                "display-tool-reapply-refused",
                &[("tool", ArgValue::Text(tool.name().to_owned()))],
            )
        },
    );
    notices.post(Notification::new(title, String::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked_about_a_tool(id: u64) -> Confirmation {
        let mut confirmation = Confirmation::default();
        confirmation.hear(
            DisplayEvent::Confirm {
                id,
                tool: None,
                within: Duration::from_secs(15),
            },
            &DesktopNotifications::default(),
        );
        confirmation
    }

    #[test]
    fn a_tools_change_is_asked_about() {
        let confirmation = asked_about_a_tool(7);
        assert!(confirmation.asks_about_tool(7));
    }

    #[test]
    fn the_question_closes_when_its_change_goes_back_but_not_another() {
        let mut confirmation = asked_about_a_tool(7);
        confirmation.hear(
            DisplayEvent::Reverted { id: 8 },
            &DesktopNotifications::default(),
        );
        assert!(confirmation.asks_about_tool(7), "another change went back");
        confirmation.hear(
            DisplayEvent::Reverted { id: 7 },
            &DesktopNotifications::default(),
        );
        assert!(confirmation.asked.is_none());
    }

    #[test]
    fn a_safe_change_kept_while_the_question_stands_leaves_it_alone() {
        let mut confirmation = asked_about_a_tool(7);
        confirmation.hear(
            DisplayEvent::Kept {
                changes: Vec::new(),
            },
            &DesktopNotifications::default(),
        );
        assert!(confirmation.asks_about_tool(7));
    }

    #[test]
    fn a_change_kept_in_answer_to_the_question_takes_it_down() {
        let mut confirmation = asked_about_a_tool(7);
        if let Some(asked) = confirmation.asked.as_mut() {
            asked.answering = true;
        }
        confirmation.hear(
            DisplayEvent::Kept {
                changes: Vec::new(),
            },
            &DesktopNotifications::default(),
        );
        assert!(confirmation.asked.is_none());
    }
}
