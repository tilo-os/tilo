//! An effect's calls on other resources: another resource's published
//! action, and a record of the workspace read through the record door.

use std::sync::mpsc::TryRecvError;

use tilo_actions::remote::ActionHostError;
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_resource::Resource;
use ui_core::Arrival;

use super::{EffectAnswer, EffectCx, Effected};

impl<R: Resource, H: 'static> EffectCx<R, H> {
    /// Call another resource's published action, as part of this effect,
    /// then `then` with what it answered (or why it did not go ahead) on the
    /// UI thread.
    ///
    /// The host decides the call as this effect's caller: when it delivered
    /// this invocation for someone (a person at its palette, an agent,
    /// another member), the call continues it and is theirs, with their
    /// grants, in the other resource's history and their undo there. The
    /// person at this device acting here asks as this app, which needs the
    /// workspace's grant to it (or their answer). What the effect then
    /// submits to its own resource continues it the same way, so the whole
    /// composition is the caller's, each part recorded once where it
    /// changed something.
    ///
    /// ```ignore
    /// let note = Call::on("so.tilo.notes", None, &notebook, "create-page")
    ///     .with(json!({ "title": title }));
    /// Ok(cx.call(&note, move |_, created, cx| {
    ///     let created = created?;
    ///     cx.submit(Tracker::LINK.with(..));
    ///     cx.when_landed(|_, _, _| Ok(Effected::done()))
    /// }))
    /// ```
    pub fn call(
        &mut self,
        call: &super::super::Call,
        then: impl FnOnce(&mut H, Result<ActionOutput, String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let action = call.action().to_owned();
        let answer = self.env.dispatch.call_published(self.inv.delivered, call);
        self.wait_answer(answer, move |called| called_outcome(called, &action), then)
    }

    /// Wait on `answer` until `settled` reads an outcome from it, then
    /// `then` with that outcome on the UI thread.
    fn wait_answer<A: 'static, T: Send + 'static>(
        &mut self,
        answer: Result<ui_core::Arriving<A, ActionHostError>, String>,
        settled: impl Fn(&Arrival<A, ActionHostError>) -> Option<Result<T, String>> + 'static,
        then: impl FnOnce(&mut H, Result<T, String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let mut answer = Some(answer);
        self.wait_on(
            move || match answer.take() {
                Some(Ok(arriving)) => {
                    if let Some(outcome) = arriving.with(&settled) {
                        return Ok(outcome);
                    }
                    answer = Some(Ok(arriving));
                    Err(TryRecvError::Empty)
                }
                Some(Err(refused)) => Ok(Err(refused)),
                None => Err(TryRecvError::Disconnected),
            },
            move |host, outcome, cx| {
                let outcome = outcome.unwrap_or_else(|| Err("it was dropped unanswered".into()));
                then(host, outcome, cx)
            },
        )
    }

    /// Read a record of the workspace, any app's, through the host's record
    /// door, then `then` with its readings (`{"revision", "records": [...]}`,
    /// each record `{"record", "value" | "absent" | "arriving"}`) or why it
    /// was not read, on the UI thread. The host decides it as this effect's
    /// caller, as it decides [`Self::call`]: continuing the invocation it
    /// delivered for someone, it is their read, by their grants.
    pub fn read_record(
        &mut self,
        read: tilo_actions::remote::RecordReadRequest,
        then: impl FnOnce(&mut H, Result<serde_json::Value, String>, &mut Self) -> EffectAnswer
        + 'static,
    ) -> Effected {
        let answer = self.env.dispatch.read_record(self.inv.delivered, read);
        self.wait_answer(
            answer,
            |reading| match reading {
                Arrival::Ready(readings) => Some(Ok(readings.clone())),
                Arrival::Failed(error) => Some(Err(error.to_string())),
                Arrival::Pending { .. } => None,
            },
            then,
        )
    }
}

/// What `called`, a call of another resource's `action`, came to once it
/// ended: its output, or why it did not go ahead. `None` while it runs.
fn called_outcome(
    arrival: &Arrival<InvocationUpdate, ActionHostError>,
    action: &str,
) -> Option<Result<ActionOutput, String>> {
    match arrival {
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(Ok(output.clone())),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
            Some(Err(format!("{action}: {}", error.message)))
        }
        Arrival::Ready(InvocationUpdate::Cancelled { .. }) => {
            Some(Err(format!("{action} was cancelled")))
        }
        Arrival::Failed(error) => Some(Err(format!("{action}: {error}"))),
        Arrival::Pending { .. }
        | Arrival::Ready(
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. },
        ) => None,
    }
}
