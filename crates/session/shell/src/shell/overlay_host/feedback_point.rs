//! A click that picked what a feedback report is about
//! ([`crate::shell::feedback_entry::FeedbackPoint`]): the app that draws the
//! window is asked over its shell-ui connection what control is at that
//! point, and the composer opens about that control, or about the window
//! when the app cannot say.

use std::time::Instant;

use ui::floating_surface::{FloatingSurfaceHostMessage, PointedControl};

use super::ShellOverlayHost;
use crate::shell::feedback_entry::PointStep;
use tilo_desktop::feedback::{Control, Subject};

impl ShellOverlayHost {
    /// The armed click landed on `point` (in that window's logical
    /// coordinates) of a window of `app_id`, as the launcher knows the app.
    pub(crate) fn feedback_picked(&mut self, app_id: &str, point: [f64; 2]) {
        let subject = Subject {
            app: app_id.to_owned(),
            name: self.app_label(app_id),
            control: None,
        };
        self.feedback_point.picked(subject, point);
        self.bump_chrome();
    }

    /// Advance a picked point: the messages to send to which shell-ui
    /// connections now. Opens the composer once the report's subject is
    /// settled without an answer.
    pub(crate) fn route_feedback_point(
        &mut self,
        now: Instant,
    ) -> Vec<(u64, FloatingSurfaceHostMessage)> {
        let requests = &self.selection_requests;
        match self
            .feedback_point
            .step(|app| requests.connections_of(app), now)
        {
            Some(PointStep::Ask {
                connections,
                request,
                point,
            }) => connections
                .into_iter()
                .map(|connection| {
                    (
                        connection,
                        FloatingSurfaceHostMessage::PointAt { request, point },
                    )
                })
                .collect(),
            Some(PointStep::Open(subject)) => {
                self.open_feedback_about(subject);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// `app_id` (as the launcher vouched for it) answered `request` with the
    /// control at the picked point. Whether the composer opened.
    pub(crate) fn receive_pointed(
        &mut self,
        app_id: &str,
        request: u64,
        control: Option<PointedControl>,
    ) -> bool {
        let control = control.map(|control| Control {
            id: control.id,
            role: control.role,
            label: control.label,
        });
        let Some(subject) = self.feedback_point.answered(app_id, request, control) else {
            return false;
        };
        self.open_feedback_about(subject);
        true
    }

    /// When a picked point next needs a tick.
    pub(crate) fn feedback_point_deadline(&self, now: Instant) -> Option<Instant> {
        self.feedback_point.deadline(now)
    }

    fn open_feedback_about(&self, subject: Subject) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if let Err(error) =
            crate::shell::feedback_entry::about(&mut chrome.borrow_mut(), Some(subject))
        {
            tracing::debug!(?error, "feedback composer did not open");
        }
        self.bump_chrome();
    }
}
