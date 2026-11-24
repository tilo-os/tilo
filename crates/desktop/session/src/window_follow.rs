//! What the session tells the apps of a workspace about the person's windows: whose view the
//! person follows, and the save one window's app is asked for before its window moves. The apps
//! are connected to the device's daemon, so the session asks the daemon to tell them
//! ([`tilo_actions::remote::window_follow`]), as the person, on its link.

use std::sync::Arc;

use collab_types::handoff::{HandoffReport, HandoffRequest};
use tilo_actions::remote::window_follow as wire;

/// The person's window steps, told to their apps through the device's daemon. Cheap to clone.
#[derive(Clone)]
pub(crate) struct WindowFollow(Arc<runtime_collab::ShellDispatch>);

impl WindowFollow {
    pub(crate) fn new(dispatch: Arc<runtime_collab::ShellDispatch>) -> Self {
        Self(dispatch)
    }

    /// Tell `workspace`'s apps the person follows `peer`'s view now, or nobody's.
    pub(crate) fn set(&self, workspace: &str, peer: Option<tilo_actions::MemberId>) {
        self.tell(
            workspace,
            wire::request(wire::FOLLOW, &wire::Follow { peer }),
        );
    }

    /// Ask the window of `app`'s instance `instance` in `workspace` to make everything it shows
    /// durable, as request `token`. `answered` hears the app's answer, on the thread it arrives
    /// on, or `None` when it gave none: its connection closed first, or no app shows that window.
    pub(crate) fn save(
        &self,
        workspace: &str,
        app: &str,
        instance: u64,
        token: u64,
        answered: impl FnOnce(Option<HandoffReport>) + Send + 'static,
    ) {
        let request = wire::request(
            wire::HANDOFF,
            &wire::Handoff {
                app: app.to_owned(),
                instance,
                request: HandoffRequest::Checkpoint { token },
            },
        );
        let workspace = workspace.to_owned();
        self.0
            .follow(Some(workspace.clone()), request)
            .then(move |result| {
                answered(match result {
                    Some(Ok(output)) => {
                        match serde_json::from_value(output.value.unwrap_or_default()) {
                            Ok(report) => Some(report),
                            Err(error) => {
                                tracing::warn!(%workspace, %error, "a window's answer to a save does not read");
                                None
                            }
                        }
                    }
                    Some(Err(why)) => {
                        tracing::warn!(%workspace, %why, "a window's app did not answer a save");
                        None
                    }
                    None => None,
                });
            });
    }

    /// Ask the daemon to carry out `request` in `workspace`, without waiting for it: the session's
    /// pass goes on.
    fn tell(&self, workspace: &str, request: tilo_actions::remote::InvokeRequest) {
        let action = request.target.address.action.clone();
        let workspace = workspace.to_owned();
        self.0
            .follow(Some(workspace.clone()), request)
            .then(move |result| match result {
                Some(Ok(_)) => {}
                Some(Err(why)) => {
                    tracing::warn!(%workspace, %action, %why, "the device did not tell the apps of a window step");
                }
                None => {
                    tracing::warn!(%workspace, %action, "the device dropped a window step");
                }
            });
    }
}
