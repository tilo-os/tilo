//! An installed app's action, applied from the model its package carries: the same module every
//! holder of the workspace replays it with, whichever device built the app.

use super::{
    ActionCallError, ActionFailure, ActionOutput, ActorPlace, Answer, Caller, DocumentRoute,
    Outcome, PackagedCall, TreePath, Worker, WorkspaceId, direct, unknown_action,
};

impl Worker {
    /// The `.tapp` `workspace` keeps for `app`, when its governance installs
    /// a version and this device has the package file.
    pub(super) fn installed_package(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
    ) -> Option<Vec<u8>> {
        self.shared
            .link
            .get()
            .and_then(|link| link.installed_package(workspace_id, app))
    }

    /// Apply `request` to the resource at `route` from the model the app
    /// version's package carries, as `caller`: what the workspace's installed
    /// app offers, applied by the holder that keeps the package. The device
    /// executes the module, admits and publishes what it wrote under the
    /// caller's own actor, and answers the invocation a peer replays.
    ///
    /// An action a grant leaves to the person, or refuses them, is refused
    /// here: nothing is asked on the server's behalf, and the app's own code
    /// never checks a permission.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn perform_packaged(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        package: &[u8],
        (target, arguments, withheld): (
            &tilo_actions::remote::RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
            &direct::Kept,
        ),
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let Some(link) = self.shared.link.get().cloned() else {
            return Err(unknown_action());
        };
        let (resource_kind, verb) = link
            .packaged_action(workspace_id, &route.app_id, &target.address.action)
            .ok_or_else(unknown_action)?;
        withheld.refuses(&target.address.resource)?;
        let kind = verb.kind;
        if !self.serves_as_owner(caller) {
            let who = self.bounded(caller).ok_or_else(unknown_action)?;
            if !self.admits_bounded(workspace_id, &who) {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
            // Only what their access answers allow is applied: an action that
            // would ask the person first waits for a device of theirs, never
            // goes unasked on a server.
            let allowed =
                self.permissions_at(workspace_id, &who, route)
                    .is_some_and(|permissions| {
                        permissions.answer(&TreePath::root(), kind) == Answer::Allow
                    });
            if !allowed {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        }
        let now_ms = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX);
        let answer = link
            .apply_packaged_action(
                workspace_id,
                caller,
                PackagedCall {
                    app: &route.app_id,
                    kind: &resource_kind,
                    document: &route.document,
                    package,
                    action: &target.address.action,
                    arguments,
                    now_ms,
                },
            )
            .map_err(|reason| {
                Outcome::Failed(ActionFailure {
                    code: "packaged_action".into(),
                    message: reason,
                    details: None,
                    retryable: false,
                })
            })?;
        // The invocation is the change's, which a peer replays; the action
        // answers nothing more, as a verb that returns nothing does.
        answer
            .map(|_envelope| (ActionOutput::default(), None))
            .ok_or_else(unknown_action)
    }
}
