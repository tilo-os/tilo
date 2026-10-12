//! Name the agent behind the writes a Files change makes, before making them.
//!
//! An agent changes files only by invoking Files: Files then writes through
//! the mount, so the version is this device's, for its person. When a change
//! runs under an invocation, Files first asks the broker to name the agent for
//! the paths the change writes. Files never names anybody: the broker takes
//! the agent, if any, from that invocation's own custody. Whatever the answer,
//! the change is then carried out; an attribution that cannot be made leaves
//! the writes the person's, and never stops them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::collaboration::{CollabEvent, CollabRequest};
use ui::tilo_actions::InvocationId;
use ui::time::{Duration, Instant};

/// How long a change waits for the broker before it is carried out anyway.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// The change a waiting attribution lets write.
pub(crate) trait Waiting {
    /// Carry the change out: attributed or not, it writes.
    fn proceed(self);
    /// Whether the change was given up meanwhile.
    fn is_cancelled(&self) -> bool;
}

impl Waiting for ui::framework::EffectReply {
    fn proceed(self) {
        self.finish(Ok(ui::tilo_actions::ActionOutput::default()));
    }

    fn is_cancelled(&self) -> bool {
        Self::is_cancelled(self)
    }
}

/// A change waiting to name its agent before it writes.
pub(crate) struct Attributing<W = ui::framework::EffectReply> {
    pub invocation: InvocationId,
    pub paths: Vec<String>,
    /// Let go once the broker answered or the wait ran out.
    pub reply: W,
}

/// Files' side of write attribution: the changes waiting and the requests in
/// flight.
pub(crate) struct Client<W = ui::framework::EffectReply> {
    queue: Rc<RefCell<Vec<Attributing<W>>>>,
    awaiting: HashMap<u64, (W, Instant)>,
    next_request: u64,
}

impl<W> Default for Client<W> {
    fn default() -> Self {
        Self {
            queue: Rc::default(),
            awaiting: HashMap::new(),
            next_request: 0,
        }
    }
}

impl<W: Waiting> Client<W> {
    /// Where the change effect queues what it is about to write.
    pub(crate) fn queue(&self) -> Rc<RefCell<Vec<Attributing<W>>>> {
        Rc::clone(&self.queue)
    }

    /// The requests to send for the changes queued since, and every change
    /// whose wait ran out, carried out.
    pub(crate) fn take_requests(&mut self, now: Instant) -> Vec<CollabRequest> {
        let expired: Vec<u64> = self
            .awaiting
            .iter()
            .filter(|(_, (_, until))| now >= *until)
            .map(|(request, _)| *request)
            .collect();
        for request in expired {
            if let Some((reply, _)) = self.awaiting.remove(&request) {
                reply.proceed();
            }
        }
        let queued = std::mem::take(&mut *self.queue.borrow_mut());
        let mut requests = Vec::new();
        for attributing in queued {
            if attributing.reply.is_cancelled() {
                continue;
            }
            self.next_request = self.next_request.wrapping_add(1);
            let request_id = self.next_request;
            self.awaiting.insert(
                request_id,
                (
                    attributing.reply,
                    now.checked_add(ANSWER_WITHIN).unwrap_or(now),
                ),
            );
            requests.push(CollabRequest::AttributeFileWrites {
                request_id,
                paths: attributing.paths,
                on_behalf_of: attributing.invocation,
            });
        }
        requests
    }

    /// Carry out the change the broker just answered for.
    pub(crate) fn event(&mut self, event: &CollabEvent) {
        let CollabEvent::FileWritesAttributed { request_id, result } = event else {
            return;
        };
        if let Err(error) = result {
            tracing::debug!(%error, "files: writes stay unattributed");
        }
        if let Some((reply, _)) = self.awaiting.remove(request_id) {
            reply.proceed();
        }
    }

    /// When the next change must be carried out or asked about.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if !self.queue.borrow().is_empty() {
            return Some(ui::time::now());
        }
        self.awaiting.values().map(|(_, until)| *until).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change that records whether it was let go to write.
    #[derive(Clone, Default)]
    struct Change(Rc<RefCell<bool>>);

    impl Waiting for Change {
        fn proceed(self) {
            *self.0.borrow_mut() = true;
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn requested(request: &CollabRequest) -> (u64, Vec<String>, InvocationId) {
        let CollabRequest::AttributeFileWrites {
            request_id,
            paths,
            on_behalf_of,
        } = request
        else {
            panic!("not an attribution request: {request:?}");
        };
        (*request_id, paths.clone(), *on_behalf_of)
    }

    /// A change asks the broker about exactly its paths and its own
    /// invocation, names nobody itself, and writes once the broker answered
    /// -- whatever the answer -- or the wait ran out.
    #[test]
    fn a_change_names_its_paths_and_invocation_then_writes_whatever_the_answer() {
        let mut client = Client::<Change>::default();
        let now = ui::time::now();
        let invocation = InvocationId::new();
        let (answered, refused, silent) = (Change::default(), Change::default(), Change::default());
        for (change, path) in [
            (&answered, "/var/workspace/Reports"),
            (&refused, "/var/workspace/Notes"),
            (&silent, "/var/workspace/Drafts"),
        ] {
            client.queue().borrow_mut().push(Attributing {
                invocation,
                paths: vec![path.into()],
                reply: change.clone(),
            });
        }
        assert!(client.next_wake().is_some(), "queued changes want a turn");
        let requests = client.take_requests(now);
        let asked: Vec<_> = requests.iter().map(requested).collect();
        assert_eq!(
            asked
                .iter()
                .map(|(_, paths, on)| (paths.clone(), *on))
                .collect::<Vec<_>>(),
            [
                (vec!["/var/workspace/Reports".to_string()], invocation),
                (vec!["/var/workspace/Notes".to_string()], invocation),
                (vec!["/var/workspace/Drafts".to_string()], invocation),
            ]
        );
        assert!(
            !*answered.0.borrow(),
            "nothing writes before the broker answered"
        );

        client.event(&CollabEvent::FileWritesAttributed {
            request_id: asked[0].0,
            result: Ok(true),
        });
        client.event(&CollabEvent::FileWritesAttributed {
            request_id: asked[1].0,
            result: Err("no live invocation".into()),
        });
        assert!(*answered.0.borrow());
        assert!(
            *refused.0.borrow(),
            "a refusal leaves the writes the person's"
        );
        assert!(!*silent.0.borrow());

        let later = now.checked_add(ANSWER_WITHIN).unwrap();
        assert!(client.take_requests(later).is_empty());
        assert!(*silent.0.borrow(), "an unanswered change writes anyway");
        assert_eq!(client.next_wake(), None);
    }
}
