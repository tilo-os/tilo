//! Feedback about the window in front, or about one control in it: the
//! palette's `feedback.open` names the focused window's app as the report's
//! subject and brings up the one private composer; `feedback.point` lets the
//! person click what the report is about, and the app that draws it names
//! the control there ([`FeedbackPoint`]). Nothing about the window is read
//! until the person includes context in a review.

use std::time::Instant;

use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::ActionError;
use crate::shell::state::AppKind;
use tilo_desktop::feedback::{Control, Focused, Subject};

pub(crate) fn open(st: &mut ChromeState) -> Result<(), ActionError> {
    // Native tiles other than built-ins carry no app-chosen display name.
    let focused = tilo_desktop::feedback::focused(&st.tree().desktop, |_| None);
    match focused {
        Focused::Composer => bring_up(st),
        Focused::Nothing => about(st, None),
        Focused::App(subject) => about(st, Some(subject)),
    }
}

/// The next click picks what the report is about. Until then the desktop
/// says so; Escape lets it go.
pub(crate) fn point(st: &mut ChromeState) -> Result<(), ActionError> {
    in_process(st)?;
    st.arm_feedback_pointing();
    st.show_osd(crate::shell::osd::Osd::feedback_pointing());
    Ok(())
}

/// Open the composer about `subject`, general feedback for `None`.
pub(crate) fn about(st: &mut ChromeState, subject: Option<Subject>) -> Result<(), ActionError> {
    in_process(st)?.borrow_mut().feedback_about(subject);
    bring_up(st)
}

/// Whether reports have a destination on this desktop.
pub(crate) fn configured(st: &ChromeState) -> bool {
    st.hosted_inputs
        .in_process()
        .is_some_and(|apps| apps.borrow().feedback_configured())
}

/// Open the composer on a report about the parts the person picked.
pub(crate) fn report(
    st: &mut ChromeState,
    picked: tilo_app::feedback::PickedReport,
) -> Result<(), ActionError> {
    in_process(st)?
        .borrow()
        .begin_feedback_report(picked)
        .map_err(ActionError::Explained)?;
    bring_up(st)
}

fn in_process(
    st: &ChromeState,
) -> Result<crate::shell::in_process::SharedInProcessApps, ActionError> {
    st.hosted_inputs
        .in_process()
        .cloned()
        .ok_or_else(|| ActionError::Explained(crate::shell::i18n::t("feedback-action-unavailable")))
}

/// Bring up the one composer, keeping the subject it has.
fn bring_up(st: &mut ChromeState) -> Result<(), ActionError> {
    in_process(st)?;
    let open = tilo_desktop::feedback::composer_tab(&st.tree().desktop);
    if let Some((workspace, strip, group, tab)) = open {
        st.focus_tab(workspace, strip, group, tab);
        return Ok(());
    }
    crate::shell::window_actions::open_hosted(
        st,
        AppKind::Other(tilo_desktop::feedback::KIND.into()),
        tilo_desktop::feedback::definition().title(),
        crate::shell::window_actions::Destination::Beside,
    );
    Ok(())
}

/// Where a click picked a report's subject, as it waits for the app under
/// it to name the control there.
#[derive(Default)]
pub(crate) struct FeedbackPoint {
    pending: Option<Pending>,
    next_request: u64,
}

struct Pending {
    request: u64,
    subject: Subject,
    /// In the window's logical coordinates.
    point: [f64; 2],
    /// When the app was asked, and until when it may answer.
    asked: Option<Instant>,
}

/// What the desktop does next for a picked point.
#[derive(Debug, PartialEq)]
pub(crate) enum PointStep {
    /// Ask these shell-ui connections what is at `point` of the window.
    Ask {
        connections: Vec<u64>,
        request: u64,
        point: [f64; 2],
    },
    /// Open the composer about this subject.
    Open(Subject),
}

impl FeedbackPoint {
    /// The person clicked `point` of a window of `subject`'s app; a click
    /// before the last one was answered replaces it.
    pub(crate) fn picked(&mut self, subject: Subject, point: [f64; 2]) {
        self.next_request = self.next_request.wrapping_add(1);
        self.pending = Some(Pending {
            request: self.next_request,
            subject,
            point,
            asked: None,
        });
    }

    /// Advance a picked point: ask the app's `connections` once (an app
    /// that cannot answer, or none, leaves the report about the window),
    /// and give up on an answer at `deadline` after asking.
    pub(crate) fn step(
        &mut self,
        connections: impl FnOnce(&str) -> Vec<u64>,
        now: Instant,
    ) -> Option<PointStep> {
        let pending = self.pending.as_mut()?;
        match pending.asked {
            None => {
                let connections = connections(&pending.subject.app);
                if connections.is_empty() {
                    return self
                        .pending
                        .take()
                        .map(|pending| PointStep::Open(pending.subject));
                }
                pending.asked = Some(now);
                Some(PointStep::Ask {
                    connections,
                    request: pending.request,
                    point: pending.point,
                })
            }
            Some(asked) if now >= answer_by(asked) => self
                .pending
                .take()
                .map(|pending| PointStep::Open(pending.subject)),
            Some(_) => None,
        }
    }

    /// `app` answered `request` with the control at the point: the report
    /// is about it. Any other answer is ignored.
    pub(crate) fn answered(
        &mut self,
        app: &str,
        request: u64,
        control: Option<Control>,
    ) -> Option<Subject> {
        let matches = self.pending.as_ref().is_some_and(|pending| {
            pending.asked.is_some() && pending.request == request && pending.subject.app == app
        });
        if !matches {
            return None;
        }
        let mut subject = self.pending.take()?.subject;
        subject.control = control;
        Some(subject)
    }

    /// When a picked point next needs a step: at once before the app was
    /// asked, then at its deadline.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        let pending = self.pending.as_ref()?;
        Some(pending.asked.map_or(now, answer_by))
    }
}

/// Until when an app asked at `asked` may answer.
fn answer_by(asked: Instant) -> Instant {
    asked
        .checked_add(crate::shell::selection_agent::ANSWER_WAIT)
        .unwrap_or(asked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::selection_agent::ANSWER_WAIT;

    fn notes() -> Subject {
        Subject {
            app: "so.tilo.notes".into(),
            name: "Notes".into(),
            control: None,
        }
    }

    fn send() -> Control {
        Control {
            id: "root~k:send".into(),
            role: "Button".into(),
            label: Some("Send".into()),
        }
    }

    #[test]
    fn a_picked_point_asks_the_app_once_and_its_answer_names_the_control() {
        let mut point = FeedbackPoint::default();
        let now = Instant::now();
        assert_eq!(point.step(|_| vec![7], now), None, "nothing was picked");
        point.picked(notes(), [40.0, 12.0]);
        assert_eq!(point.deadline(now), Some(now));
        let Some(PointStep::Ask {
            connections,
            request,
            point: at,
        }) = point.step(
            |app| {
                if app == "so.tilo.notes" {
                    vec![7]
                } else {
                    vec![]
                }
            },
            now,
        )
        else {
            panic!("the app that draws the window is asked");
        };
        assert_eq!((connections, at), (vec![7], [40.0, 12.0]));
        assert_eq!(point.step(|_| vec![7], now), None, "asked once");
        assert_eq!(point.deadline(now), Some(now + ANSWER_WAIT));
        assert_eq!(
            point.answered("so.tilo.files", request, Some(send())),
            None,
            "another app's answer is not this report's"
        );
        assert_eq!(
            point.answered("so.tilo.notes", request + 1, Some(send())),
            None
        );
        let subject = point
            .answered("so.tilo.notes", request, Some(send()))
            .unwrap();
        assert_eq!(subject.control, Some(send()));
        assert_eq!(point.deadline(now), None, "done");
    }

    #[test]
    fn an_app_that_cannot_answer_or_does_not_in_time_leaves_the_report_about_its_window() {
        let mut point = FeedbackPoint::default();
        let now = Instant::now();
        point.picked(notes(), [1.0, 1.0]);
        assert_eq!(
            point.step(|_| Vec::new(), now),
            Some(PointStep::Open(notes()))
        );

        point.picked(notes(), [1.0, 1.0]);
        let Some(PointStep::Ask { request, .. }) = point.step(|_| vec![3], now) else {
            panic!("asked");
        };
        assert_eq!(point.step(|_| vec![3], now + ANSWER_WAIT / 2), None);
        assert_eq!(
            point.step(|_| vec![3], now + ANSWER_WAIT),
            Some(PointStep::Open(notes()))
        );
        assert_eq!(
            point.answered("so.tilo.notes", request, Some(send())),
            None,
            "a late answer opens nothing more"
        );
    }
}
