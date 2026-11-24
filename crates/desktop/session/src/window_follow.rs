//! What the session tells the apps of a workspace about the person's windows: whose view the
//! person follows, and the step of a move one window's app is asked for. The apps are connected to
//! the device's daemon, so the session asks the daemon to tell them
//! ([`tilo_actions::remote::window_follow`]), as the person, on its link.

use std::sync::Arc;

use collab_types::handoff::HandoffRequest;
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

    /// Ask the one instance `instance` of `app` in `workspace` for the next step of moving its
    /// window. The app's answer comes back through the host's continuation services.
    pub(crate) fn request_handoff(
        &self,
        workspace: &str,
        app: &str,
        instance: u64,
        request: HandoffRequest,
    ) {
        self.tell(
            workspace,
            wire::request(
                wire::HANDOFF,
                &wire::Handoff {
                    app: app.to_owned(),
                    instance,
                    request,
                },
            ),
        );
    }

    /// Ask the daemon to carry out `request` in `workspace`, without waiting for it: the session's
    /// pass goes on, and a move that waits for an app's answer gives up on its own when none comes.
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
