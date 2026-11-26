//! A person's decisions on the work their assistant proposed. The host owns the list
//! and standing instructions; after a decision the window asks it for both again.

mod rows;

use std::cell::Cell;
use std::rc::Rc;

use ui::prelude::*;
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::{
    InvokeArgument, workspace_activity::argument, workspace_initiative as initiative,
};
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::{Window, page, tr};

enum Decision {
    Accept { id: String, always: bool },
    Decline { id: String, why: Option<String> },
    AskFirst(String),
    ProposeAgain(String),
}

impl Decision {
    fn action(self) -> (&'static str, InvokeArgument) {
        match self {
            Self::Accept { id, always } => (
                initiative::ACCEPT,
                argument(&initiative::Accept {
                    proposal: id,
                    always,
                }),
            ),
            Self::Decline { id, why } => (
                initiative::DECLINE,
                argument(&initiative::Decline { proposal: id, why }),
            ),
            Self::AskFirst(kind) => (
                initiative::ASK_FIRST,
                argument(&initiative::OfKind { kind }),
            ),
            Self::ProposeAgain(kind) => (
                initiative::PROPOSE_AGAIN,
                argument(&initiative::OfKind { kind }),
            ),
        }
    }
}

#[derive(Clone)]
struct Request {
    call: InvocationCall,
    is_list: bool,
}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.call.state().same_state(&other.call.state())
    }
}

#[derive(Clone)]
struct Review {
    remote: RemoteActions,
    person: ui::tilo_resource::MemberId,
    request: LocalState<Request>,
    /// Only the controls of a displayed, completed list may send a decision.
    ready: Rc<Cell<bool>>,
    notice: LocalState<String>,
}

impl Review {
    fn list(remote: &RemoteActions) -> Request {
        Request {
            call: remote.invoke(
                initiative::target(initiative::PROPOSALS, initiative::LIST),
                InvokeArgument::None,
                None,
            ),
            is_list: true,
        }
    }

    fn refresh(&self) {
        self.ready.set(false);
        self.request.set(Self::list(&self.remote));
    }

    fn decide(&self, decision: Decision) {
        // A widget action may also be invoked by the broker. Only the person's own
        // input may start a new signed decision from their window. External callers
        // ask the initiative provider directly, under their own admission.
        if ui::invocation_scope::delivered_invocation().is_some()
            || ui::invocation_scope::current_invoker().is_some_and(|who| {
                !who.acting.is_person() || who.acting.member_id() != Some(&self.person)
            })
        {
            self.notice.set(tr("assistant-proposals-human-only"));
            return;
        }
        if !self.ready.replace(false) {
            return;
        }
        let (verb, arguments) = decision.action();
        self.notice.set(String::new());
        self.request.set(Request {
            call: self.remote.invoke(
                initiative::target(initiative::PROPOSALS, verb),
                arguments,
                None,
            ),
            is_list: false,
        });
    }

    fn settled(
        &self,
        request: &Request,
        value: Result<Option<serde_json::Value>, String>,
    ) -> Element {
        if !request.is_list {
            if let Err(why) = value {
                self.notice.set(why);
            }
            self.refresh();
            return waiting();
        }
        let mut parts = vec![Box::new(
            Text::from_state(&self.notice, String::clone)
                .with_id("assistant-proposal-decision-notice")
                .with_live(ui::a11y::Politeness::Polite),
        ) as Element];
        match value.and_then(|value| {
            serde_json::from_value::<initiative::Proposals>(value.unwrap_or_default())
                .map_err(|_| tr("assistant-proposals-invalid-answer"))
        }) {
            Ok(list) => {
                self.ready.set(true);
                parts.extend(rows::rows(self, &list));
            }
            Err(why) => {
                parts.push(Box::new(EmptyState::error(
                    tr("assistant-proposals-refused"),
                    why,
                )));
            }
        }
        let review = self.clone();
        parts.push(Box::new(
            Button::new("assistant-proposals-refresh", "assistant-proposals-refresh").on_view(
                move || {
                    review.notice.set(String::new());
                    review.refresh();
                },
            ),
        ));
        page(parts)
    }
}

fn waiting() -> Element {
    page(vec![Box::new(
        Text::new(&tr("assistant-proposals-pending")).with_live(ui::a11y::Politeness::Polite),
    )])
}

pub(super) fn view(cx: &ViewCx<Window>) -> Element {
    let Some(person) = cx
        .me()
        .filter(|person| person.is_person() && cx.mode().is_live())
    else {
        return page(vec![Box::new(EmptyState::new(tr(
            "assistant-proposals-unavailable",
        )))]);
    };
    let remote = RemoteActions::current();
    let request = cx.local(Review::list(&remote));
    let review = Review {
        remote,
        person: person.clone(),
        request: request.clone(),
        ready: Rc::new(Cell::new(false)),
        notice: cx.local(String::new()),
    };
    cx.switch(&request, move |cx, request| {
        let (review, request) = (review.clone(), request.clone());
        cx.switch(&request.call.state(), move |_, arrival| {
            let result = match arrival {
                Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                    Ok(output.value.clone())
                }
                Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                    Err(error.message.clone())
                }
                Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Err(message
                    .clone()
                    .unwrap_or_else(|| tr("assistant-proposals-cancelled"))),
                Arrival::Failed(error) => Err(error.to_string()),
                _ => return waiting(),
            };
            review.settled(&request, result)
        })
    })
}
