//! An app's call of another app's action, or of its own action a person is asked about first,
//! sent through the device's dispatch: the one path every host of an app's code takes, a window's
//! process and the session's services alike. The dispatch decides the call as the calling app's,
//! for its person, asks them where it has to, and answers; the run that made the call waits on
//! that answer ([`answer_of`]).
//!
//! What reaches the dispatch is the host's own: [`CallLink`] sends the built call on the
//! connection the host has.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::remote::{ActionCallError, InvokeRequest, ReceiptStatus};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::value::Value;

use crate::call::Callees;
use crate::task::Answered;

/// How a host sends a call to the device's dispatch: on its own connection, as its own caller.
pub trait CallLink {
    /// Send `request`, a call of `app`'s action.
    ///
    /// # Errors
    /// Why the call reached no dispatch.
    fn send(&self, app: &str, request: InvokeRequest) -> Result<(), String>;
}

/// The calls a runtime sends through the dispatch, by the id each went out under: what the
/// answer to each is read back against ([`answer_of`]).
pub type Calls = Rc<RefCell<BTreeMap<String, Invocation>>>;

/// The runtime's invoker for the calls it does not run itself: each is built from the packages
/// the app may use ([`Callees`]), sent through `link`, and waited on under the id it went out as.
pub struct Dispatched<L> {
    /// The workspace a call names none of is made in.
    workspace: String,
    callees: Rc<Callees>,
    calls: Calls,
    link: L,
}

impl<L> Dispatched<L> {
    /// Calls made in `workspace` of the packages `callees` names, sent through `link` and kept in
    /// `calls` until answered.
    pub const fn new(workspace: String, callees: Rc<Callees>, calls: Calls, link: L) -> Self {
        Self {
            workspace,
            callees,
            calls,
            link,
        }
    }
}

impl<L: CallLink> Invoker for Dispatched<L> {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let call = match self.callees.call(invocation) {
            Ok(call) => call,
            Err(answer) => return self.callees.answered(invocation, answer),
        };
        let field = |key: &str| call.get(key).cloned().unwrap_or_default();
        let text = |key: &str| field(key).as_str().unwrap_or_default().to_owned();
        let app = text("app");
        // A secret the call names by where it is (what the person typed for this run, what the
        // host holds for the app) is filled in here, on its way out, and never was the code's.
        let mut arguments = field("arguments");
        let keep = match crate::page::fill_secrets(&mut arguments) {
            Ok(keep) => keep,
            Err(why) => return Invoked::Refused(why),
        };
        let invocation_id = tilo_actions::InvocationId::new();
        // A call of a host's own record (the shell's asking view resolving an invocation) names
        // its provider itself; every other goes to the action's app in this workspace.
        let workspace = match text("workspace") {
            named if !named.is_empty() => named,
            _ => self.workspace.clone(),
        };
        let target = match call.get("provider").and_then(serde_json::Value::as_str) {
            Some(provider) => tilo_actions::remote::RemoteActionTarget {
                provider: provider.to_owned(),
                address: tilo_actions::ActionAddress::new(text("resource"), text("action")),
            },
            None => tilo_actions::remote::application_action_target(
                &workspace,
                &app,
                text("resource"),
                text("action"),
            ),
        };
        let request = InvokeRequest {
            invocation: invocation_id,
            target,
            argument: tilo_actions::remote::InvokeArgument::Json(
                arguments.as_object().cloned().unwrap_or_default(),
            ),
            context: asked_facts(invocation),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        if let Err(error) = self.link.send(&app, request) {
            tracing::warn!(to = app, %error, "a child call did not reach the dispatch");
            return Invoked::Failed(format!("the call did not reach {app}: {error}"));
        }
        let call_id = invocation_id.0.to_string();
        crate::page::keeping(&call_id, keep);
        self.calls
            .borrow_mut()
            .insert(call_id.clone(), invocation.clone());
        Invoked::Pending(call_id)
    }
}

/// What the call `call`, sent as `invocation`, answers the run that waits on it once the dispatch
/// says `status`: its output as the declaring action returns it, or why it did not run. `None`
/// while it has not ended: the dispatch runs it, or a person is being asked about it.
#[must_use]
pub fn answer_of(
    callees: &Callees,
    call: &str,
    invocation: Option<&Invocation>,
    status: &ReceiptStatus,
) -> Option<Invoked> {
    Some(match status {
        ReceiptStatus::Accepted
        | ReceiptStatus::Rejected {
            error: ActionCallError::ConfirmationRequired { .. },
        } => return None,
        ReceiptStatus::Completed { output } => {
            let mut output = serde_json::to_value(output).unwrap_or_default();
            crate::page::keep_answer(call, &mut output, tilo_clock::unix_now_i64());
            // A call resumed from a journal (its invocation is not kept with it) answers
            // nothing, so the run goes on with no result.
            match invocation {
                Some(invocation) => callees.answered(invocation, Answered::Done(output)),
                None => Invoked::Done(Value::Unit),
            }
        }
        ReceiptStatus::Failed { error } => Invoked::Failed(error.message.clone()),
        // A call a person did not allow, or one stopped on the way: the run sees a refusal.
        ReceiptStatus::Rejected { error } => Invoked::Refused(error.said()),
        ReceiptStatus::Cancelled { message } => Invoked::Refused(
            message
                .clone()
                .unwrap_or_else(|| "the call was stopped".to_owned()),
        ),
    })
}

/// What the person asked to allow `invocation` reads on the card: the facts the runtime read for
/// it through the run that makes it (a draft the run just wrote), which only this app can read
/// yet. The dispatch takes them only from the callee's own app.
fn asked_facts(invocation: &Invocation) -> Option<tilo_actions::ActionContext> {
    let Invocation::Action { shows, .. } = invocation else {
        return None;
    };
    if shows.is_empty() {
        return None;
    }
    Some(tilo_actions::ActionContext {
        shows: shows
            .iter()
            .map(|shown| tilo_actions::remote::asked::Fact::new(&shown.label, &shown.value))
            .collect(),
        ..tilo_actions::ActionContext::default()
    })
}
