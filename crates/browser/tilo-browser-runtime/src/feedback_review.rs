//! The model a room's host lends its reporters to look over their reports.
//!
//! The page never holds the host's key: the report's review prompt, written
//! from the model view (personal values only as placeholders), goes to the
//! destination's owner over the feedback connection that admits this
//! reporter, and the owner answers with its model's quotes to hide. To the
//! composer it is a model like any other ([`ui::models::Models`]); the
//! owner asks nothing but the review contract.
use std::cell::RefCell;
use std::rc::Rc;
use std::task::Poll;

use ui::models::{
    CompletionFuture, Content, ErrorKind, Message, ModelBackend, ModelError, ModelRequest,
    ModelResponse, Models, Role, StopReason, Usage,
};

use crate::feedback_delivery::Connection;

/// Where the owner's answer to one review lands: nothing while it is out,
/// then the model's answer, or why there is none.
#[derive(Clone, Default)]
pub(crate) struct Answer(Rc<RefCell<Option<Result<String, ModelError>>>>);

impl Answer {
    /// The owner answered with `answer`; `None` when its model gave none.
    pub(crate) fn set(&self, answer: Option<String>) {
        *self.0.borrow_mut() = Some(answer.ok_or_else(|| ModelError::new(ErrorKind::Unavailable)));
    }
}

/// Asks the owner's model over the feedback connection.
struct OwnerModel(Rc<RefCell<Connection>>);

impl ModelBackend for OwnerModel {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        let prompt: String = request
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let answer = Answer::default();
        let asked = self.0.borrow_mut().ask_review(prompt, answer.clone());
        Box::pin(std::future::poll_fn(move |_| {
            if let Err(error) = &asked {
                return Poll::Ready(Err(ModelError {
                    kind: ErrorKind::Unavailable,
                    message: error.clone(),
                }));
            }
            let Some(answer) = answer.0.borrow_mut().take() else {
                return Poll::Pending;
            };
            Poll::Ready(answer.map(|text| ModelResponse {
                message: Message::text(Role::Assistant, text),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            }))
        }))
    }
}

/// The model the owner of peer `peer`'s feedback destination lends its
/// reporters, and what the review calls it; `None` when it lends none.
pub(crate) fn owner_model(peer: u32) -> Option<(Models, String)> {
    let connection = crate::feedback_delivery::connection(peer)?;
    let name = connection.borrow().reviewer.clone()?;
    Some((Models::new(Rc::new(OwnerModel(connection))), name))
}
