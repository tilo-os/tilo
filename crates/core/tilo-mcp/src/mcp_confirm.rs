//! Starting an invocation: its admission, asked again under the same
//! invocation while an answer is lost on its way. A call whose action waits for
//! the person is kept as its asked invocation record by the dispatch that admits
//! it, and the agent follows it as it follows any other call.
use super::{
    ActionProvider, BridgeError, InvokeRequest, McpServer, ReceiptStatus, RemoteActionTarget,
    Value, json,
};

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Start one invocation, reporting the host's refusal verbatim.
    ///
    /// The host answers once the admission is durable, which on a loaded
    /// disk takes seconds. It is waited for as long as the call waits for
    /// its outcome, and at least [`HOST_ANSWER_WAIT`]; an admission still
    /// unanswered then goes on without this call, which says so.
    pub(crate) async fn start(
        &self,
        request: InvokeRequest,
        wait: crate::Hold,
        record: &mut Option<(tilo_actions::InvocationId, &RemoteActionTarget)>,
    ) -> Result<Admission, BridgeError> {
        // The workspace's own actions are admitted and started synchronously
        // (a launch among them); an app's action is resolved on the broker's
        // set and answers later.
        let blocking = tilo_actions::remote::provider_workspace(&request.target.provider).is_some();
        // What the answer will be read for is settled as the action starts,
        // on the revision it starts at: whether it is a view or an effect is
        // what decides if a picture it names is shown or left as an address.
        // Asked here because the answer's own target carries the id a bound
        // resource was issued under, which the host cannot resolve later. An
        // action starting at no named revision is not asked: it must cost no
        // read of the app's directory first, and an answer that says what ran
        // (`agent.kind`) settles its class.
        let asked = record
            .take()
            .filter(|_| !request.revision.as_deref().unwrap_or_default().is_empty());
        let revision = request.revision.clone().unwrap_or_default();
        // A call held until its action has ended waits for its admission far longer than one
        // that named a wait, which is answered as pending once that has passed. Even it has a
        // bound: a provider that never answers leaves the call waiting forever, and the action
        // may be running all the same, so the caller is answered as pending under the
        // invocation id it can follow.
        let (until, patience) = match wait.at_least(HOST_ANSWER_WAIT) {
            crate::Hold::UntilEnded => (ADMISSION_ANSWER_WAIT, ADMISSION_ANSWER_WAIT),
            crate::Hold::For(patience) => (patience, patience),
        };
        let until = tokio::time::Instant::now()
            .checked_add(until)
            .unwrap_or_else(tokio::time::Instant::now);
        // An answer that was lost on its way (the provider did not answer in time, or the
        // session carrying it gave it up) says nothing about whether the provider admitted the
        // invocation: it may be done already. The same invocation is asked again, which the
        // provider answers with what became of it, and admits once if it never arrived. Only
        // the patience ending answers this call without it, as pending under its id.
        let answer = loop {
            let again = request.clone();
            let admitting = self.at_host(blocking, move |host, caller| host.invoke(caller, again));
            let Ok(answer) = tokio::time::timeout_at(until, admitting).await else {
                return Ok(Admission::Unanswered(patience));
            };
            // A workspace a server has not started serving yet refuses the ask and takes it
            // once it does: the session is the signal that it moved, and the call waits for
            // it rather than telling its caller to come back.
            if matches!(
                &answer,
                Some(Err(tilo_actions::remote::ActionCallError::NotReady))
            ) {
                let until = tokio::time::Instant::now()
                    .checked_add(crate::NOT_SERVED_WAIT)
                    .unwrap_or_else(tokio::time::Instant::now);
                if self.wait_served(until).await {
                    continue;
                }
                break answer;
            }
            if !lost(answer.as_ref()) {
                break answer;
            }
            let pause = tokio::time::Instant::now()
                .checked_add(ASK_AGAIN_PAUSE)
                .unwrap_or(until);
            if pause >= until {
                return Ok(Admission::Unanswered(patience));
            }
            tokio::time::sleep(ASK_AGAIN_PAUSE).await;
        };
        if let Some((invocation, target)) = asked {
            let effect = self.asked_effect(target, &revision).await;
            self.remember_effect(invocation, effect);
        }
        let answer = answer.ok_or_else(|| BridgeError::from("Unavailable"))?;
        // A receipt that admitted nothing is the host's refusal: there is no
        // invocation to follow, and answering one as unknown would lose why.
        if let Ok(receipt) = &answer
            && let ReceiptStatus::Rejected { error } = &receipt.status
        {
            return Err(BridgeError::Host(error.clone()));
        }
        answer
            .map(|receipt| match receipt.status {
                ReceiptStatus::Completed { output } => Admission::Answered(Some(output)),
                ReceiptStatus::Failed { error } => Admission::Failed(error),
                _ => Admission::Answered(None),
            })
            .map_err(BridgeError::Host)
    }
}

/// Whether `failure`, a refusal a call got, is the person being asked: the
/// host's own answer ([`tilo_actions::remote::ActionCallError::ConfirmationRequired`]),
/// which this bridge also answers while the person decides past its patience.
/// A typed caller is answered `pending` for it instead of waiting.
pub(super) const fn needs_the_person(failure: &super::Failure) -> bool {
    matches!(
        failure.host,
        Some(tilo_actions::remote::ActionCallError::ConfirmationRequired { .. })
    )
}

/// Whether the host answered an invocation's admission while the call
/// waited for it.
pub(super) enum Admission {
    /// Admitted: the invocation runs, and its outcome can be awaited. A read the host
    /// answered as it admitted it holds its output: it is remembered as no invocation
    /// to follow.
    Answered(Option<tilo_actions::ActionOutput>),
    /// Admitted and failed as it was admitted: an action that keeps no record of a call
    /// answers its failure here alone.
    Failed(tilo_actions::ActionFailure),
    /// Not answered within this long; the admission goes on regardless.
    Unanswered(std::time::Duration),
}
/// The answer to an invocation whose admission was not answered within
/// `patience`. The same invocation id never runs twice, so invoking it again
/// is safe and tells how the admission went.
pub(super) fn unanswered_admission(
    id: tilo_actions::InvocationId,
    patience: std::time::Duration,
) -> Value {
    json!({
        "invocation_id": id.0.to_string(),
        "pending": format!(
            "admission not answered within {} ms; tilo_status with this invocation_id \
             answers how it went (the same invocation_id never runs twice)",
            patience.as_millis()
        ),
    })
}
/// How long the host may take at least to admit an invocation: one not
/// admitted by then is answered as pending with its invocation id, to be
/// invoked again.
const HOST_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a call held until its action has ended waits for its admission.
/// Longer than any admission the host bounds itself (a launch is thirty
/// seconds, an app's directory a minute), so the host's own reason is heard
/// first, and short enough that a provider that never answers is heard of
/// rather than waited on forever.
const ADMISSION_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(90);
/// How long an admission whose answer was lost waits before it is asked again.
const ASK_AGAIN_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);
/// Whether `answer`, the provider's answer to an invocation, was lost on its way rather than
/// given: none came (the provider abandoned it), or it says the provider did not answer in
/// time. Neither says whether the invocation was done.
fn lost(
    answer: Option<
        &Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    >,
) -> bool {
    use tilo_actions::remote::ActionCallError::Unanswered;
    match answer {
        None | Some(Err(Unanswered)) => true,
        Some(Ok(receipt)) => matches!(
            &receipt.status,
            ReceiptStatus::Rejected { error: Unanswered }
        ),
        Some(Err(_)) => false,
    }
}
