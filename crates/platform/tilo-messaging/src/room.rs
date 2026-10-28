//! The person's Tilo messages where no native messaging host runs (a web
//! page): sent and kept as the private conversations of the room the
//! person is in, which its governor serves to every device of the room
//! (`tilo_relay_protocol::rooms::private`). A page reaches that door over
//! its HTTPS session; the device certificate the session proved is the
//! sender, so this host holds no key or credential of its own.
//!
//! The Tilo sender account is the person's one `tilo` account in their
//! accounts record. Where they have none yet, [`RoomEnrollment`] is its
//! sign-up: the person reviews it and the host publishes it there, as the
//! native host does after its own review.
//!
//! A route's recipient is the person the room knows: their root key (64
//! hex digits), or one of their devices' addresses (`root@device`, as the
//! native host writes them), which names the same person.
use crate::{
    Delivery, Direction, EnrollmentApprovalFuture, EnrollmentHost, EnrollmentPhase,
    EnrollmentReview, EnrollmentReviewFuture, HistoryCursor, HistoryError, HistoryFuture,
    HistoryHost, HistoryPage, LinkedAccount, Message, MessagingClient, MessagingHost,
    PairingFuture, SendError, SendFuture, SendReceipt, SendRequest, ServiceAccount,
};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use tilo_contact_directory::routing::Route;
use tilo_relay_protocol::rooms::private::{
    PrivateConversation, PrivateOperation, PrivateReply, read_whole,
};
use zeroize::Zeroizing;

/// What the room answered one private operation.
pub type PrivateAnswer = Pin<Box<dyn Future<Output = Result<PrivateReply, String>> + Send>>;

/// The room's private door, as the host reaches it.
pub trait PrivateDoor: Send + Sync {
    /// Carry out `operation` as this host's authenticated device.
    fn ask(&self, operation: PrivateOperation) -> PrivateAnswer;
    /// Run `work` to its end beside the app (the page's task queue): how a
    /// conversation is followed while a window watches it.
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>);
}

type Observers = Vec<Weak<dyn Fn() + Send + Sync>>;

struct Inner {
    door: Arc<dyn PrivateDoor>,
    /// The person's root key, as the room names message senders.
    me: String,
    observers: Mutex<Observers>,
    /// The conversations followed for the observers, by peer.
    followed: Mutex<BTreeSet<String>>,
}

/// The messaging service a host without a native messaging host lends
/// Messages: its Tilo account, carried by the room's private conversations.
#[derive(Clone)]
pub struct RoomMessaging {
    inner: Arc<Inner>,
}

impl RoomMessaging {
    /// The service of the person whose root key is `me` (hex), through
    /// `door`.
    #[must_use]
    pub fn new(me: &str, door: Arc<dyn PrivateDoor>) -> Self {
        Self {
            inner: Arc::new(Inner {
                door,
                me: me.to_ascii_lowercase(),
                observers: Mutex::default(),
                followed: Mutex::default(),
            }),
        }
    }

    /// The client lent to Messages for `principal`.
    #[must_use]
    pub fn client(&self, principal: &str) -> MessagingClient {
        MessagingClient::new(principal, Arc::new(self.clone()))
    }

    /// The person `route` reaches, when it is a Tilo route of this host:
    /// the sender is the person's Tilo account and the recipient someone
    /// other than them.
    fn peer(&self, route: &Route) -> Option<String> {
        if route.service != "tilo" || route.scope != scope(route.sender.as_str()) {
            return None;
        }
        peer_of(&route.recipient).filter(|peer| *peer != self.inner.me)
    }

    /// Open the conversation with `peer`: the room keeps one for the pair
    /// from then on, and reads and sends name it.
    /// Its first page comes with the answer.
    async fn open(door: &Arc<dyn PrivateDoor>, peer: &str) -> Result<PrivateConversation, String> {
        match door
            .ask(PrivateOperation::Open {
                peer: peer.to_owned(),
            })
            .await?
        {
            PrivateReply::Conversation(opened) if opened.peer == peer => Ok(opened),
            _ => Err("the room did not open the conversation".into()),
        }
    }

    /// The whole conversation with `peer`: the page opening it, or read
    /// page by page when it holds more.
    async fn conversation(
        door: Arc<dyn PrivateDoor>,
        peer: &str,
    ) -> Result<PrivateConversation, String> {
        let opened = Self::open(&door, peer).await?;
        if !opened.has_more {
            return Ok(opened);
        }
        read_whole(|operation| door.ask(operation), peer).await
    }

    /// Follow the conversation with `peer` from `after` while anyone
    /// observes, waking them each time it changes.
    fn follow(&self, peer: String, after: u64) {
        let Ok(mut followed) = self.inner.followed.lock() else {
            return;
        };
        if followed.contains(&peer) || !self.observed() {
            return;
        }
        followed.insert(peer.clone());
        drop(followed);
        let messaging = self.clone();
        self.inner.door.spawn(Box::pin(async move {
            let mut after = after;
            while messaging.observed() {
                let changed = messaging
                    .inner
                    .door
                    .ask(PrivateOperation::Watch {
                        peer: peer.clone(),
                        after_sequence: after,
                    })
                    .await;
                match changed {
                    // The poll ran out with nothing new: wait again.
                    Ok(PrivateReply::Changed(sequence)) if sequence == after => {}
                    Ok(PrivateReply::Changed(sequence)) if sequence > after => {
                        after = sequence;
                        messaging.changed();
                    }
                    // Refused, gone, or a cursor going back: the next read
                    // follows it again.
                    _ => break,
                }
            }
            if let Ok(mut followed) = messaging.inner.followed.lock() {
                followed.remove(&peer);
            }
        }));
    }

    fn observed(&self) -> bool {
        self.inner
            .observers
            .lock()
            .is_ok_and(|observers| observers.iter().any(|observer| observer.strong_count() > 0))
    }

    fn changed(&self) {
        let observers: Vec<_> = self
            .inner
            .observers
            .lock()
            .map(|observers| observers.iter().filter_map(Weak::upgrade).collect())
            .unwrap_or_default();
        for observer in observers {
            observer();
        }
    }
}

/// The private scope of the Tilo account `account`, as the accounts record
/// keeps it.
#[must_use]
pub fn scope(account: &str) -> String {
    format!("tilo:{account}")
}

/// The person `recipient` names: a root key, or the root of a device
/// address `root@device`.
fn peer_of(recipient: &str) -> Option<String> {
    let root = match recipient.split_once('@') {
        Some((root, device)) if key(device) => root,
        Some(_) => return None,
        None => recipient,
    };
    key(root).then(|| root.to_ascii_lowercase())
}

fn key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The newest page of `messages` before `cursor`, as Messages reads it.
fn page(
    route: Route,
    me: &str,
    messages: &[tilo_relay_protocol::rooms::private::PrivateMessage],
    cursor: Option<&HistoryCursor>,
    limit: usize,
) -> Result<HistoryPage, HistoryError> {
    let end = match cursor {
        None => messages.len(),
        Some(cursor) => messages
            .iter()
            .position(|message| message.id == cursor.as_str())
            .ok_or(HistoryError::InvalidCursor)?,
    };
    let start = end.saturating_sub(limit);
    Ok(HistoryPage {
        route,
        messages: messages
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|message| {
                let outgoing = message.sender == me;
                Message {
                    id: message.id.clone(),
                    text: Zeroizing::new(message.text.clone()),
                    direction: if outgoing {
                        Direction::Outgoing
                    } else {
                        Direction::Incoming
                    },
                    timestamp_ms: u64::try_from(message.received_at)
                        .unwrap_or(0)
                        .saturating_mul(1000)
                        .max(1),
                    // The governor kept it: that is all a room says.
                    delivery: outgoing.then_some(Delivery::ServerAcknowledged),
                }
            })
            .collect(),
        more: start > 0,
    })
}

impl HistoryHost for RoomMessaging {
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        {
            let mut observers = self.inner.observers.lock().ok()?;
            observers.retain(|observer| observer.strong_count() > 0);
            observers.push(Arc::downgrade(&changed));
        }
        // The guard holds the observer; dropped, it is gone.
        Some(Box::new(changed))
    }

    fn read(&self, route: Route, cursor: Option<HistoryCursor>, limit: usize) -> HistoryFuture {
        let Some(peer) = self.peer(&route) else {
            return Box::pin(std::future::ready(Err(HistoryError::InvalidRoute)));
        };
        let messaging = self.clone();
        Box::pin(async move {
            let conversation = Self::conversation(messaging.inner.door.clone(), &peer)
                .await
                .map_err(|_| HistoryError::Unavailable)?;
            let last = conversation
                .messages
                .last()
                .map_or(0, |message| message.sequence);
            messaging.follow(peer, last);
            page(
                route,
                &messaging.inner.me,
                &conversation.messages,
                cursor.as_ref(),
                limit,
            )
        })
    }
}

impl MessagingHost for RoomMessaging {
    fn account_state(&self, account: &ServiceAccount) -> crate::AccountState {
        if account.service == "tilo" && !account.account.is_empty() {
            crate::AccountState::Ready
        } else {
            crate::AccountState::Unavailable
        }
    }

    fn supports(&self, route: &Route) -> bool {
        self.peer(route).is_some()
    }

    fn send(&self, request: SendRequest) -> SendFuture {
        let Some(peer) = self.peer(&request.route) else {
            return Box::pin(std::future::ready(Err(SendError::Unavailable)));
        };
        // The operation is the room's idempotency key: a retry of one send
        // is the same message.
        let Ok(invocation) = uuid::Uuid::parse_str(&request.operation) else {
            return Box::pin(std::future::ready(Err(SendError::InvalidRequest)));
        };
        let door = self.inner.door.clone();
        Box::pin(async move {
            // Refused here, nothing was sent: the room does not share the two.
            Self::open(&door, &peer)
                .await
                .map_err(|_| SendError::Revoked)?;
            let answer = door
                .ask(PrivateOperation::Send {
                    peer,
                    invocation,
                    text: request.text.to_string(),
                })
                .await;
            match answer {
                Ok(PrivateReply::Sent) => Ok(SendReceipt {
                    route: request.route,
                    operation: request.operation,
                    delivery: Delivery::ServerAcknowledged,
                }),
                // The room refuses a person it does not share with this one.
                Ok(PrivateReply::Refused) => Err(SendError::Revoked),
                // No answer, or another one: the room may hold it or not.
                _ => Err(SendError::Uncertain),
            }
        })
    }
}

/// Publishes the person's Tilo account `(id, display name, scope)` in their
/// accounts record; refused where they already have one.
pub type PublishAccount = Arc<dyn Fn(&str, &str, &str) -> Result<(), String> + Send + Sync>;

struct Attempt {
    review: EnrollmentReview,
    phase: EnrollmentPhase,
}

/// Signing up for the person's Tilo account where no native host does it:
/// reviewed in Messages, then published in their accounts record. The
/// room's door is the account's whole custody, so nothing else is asked.
pub struct RoomEnrollment {
    /// What the review shows the person as: who the room knows them as.
    identity: String,
    publish: PublishAccount,
    attempt: Mutex<Option<Attempt>>,
}

impl RoomEnrollment {
    #[must_use]
    pub fn new(identity: &str, publish: PublishAccount) -> Self {
        Self {
            identity: identity.to_owned(),
            publish,
            attempt: Mutex::new(None),
        }
    }

    /// The client lent to Messages for `principal`.
    #[must_use]
    pub fn client(self, principal: &str) -> crate::EnrollmentClient {
        crate::EnrollmentClient::new(principal.to_owned(), Arc::new(self))
    }

    /// The review of `account` while its attempt is in `phase`.
    fn current(
        &self,
        account: &ServiceAccount,
        phase: EnrollmentPhase,
    ) -> Option<EnrollmentReview> {
        self.attempt
            .lock()
            .ok()?
            .as_ref()
            .filter(|attempt| attempt.review.account == *account && attempt.phase == phase)
            .map(|attempt| attempt.review.clone())
    }

    fn set_phase(&self, account: &ServiceAccount, phase: EnrollmentPhase) {
        if let Ok(mut attempt) = self.attempt.lock()
            && let Some(current) = attempt
                .as_mut()
                .filter(|attempt| attempt.review.account == *account)
        {
            current.phase = phase;
        }
    }

    /// A new account to review, unless an attempt is under way.
    fn prepared(&self, service: &str) -> Result<EnrollmentReview, SendError> {
        if service != "tilo" {
            return Err(SendError::InvalidRequest);
        }
        let mut attempt = self.attempt.lock().map_err(|_| SendError::Unavailable)?;
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.phase != EnrollmentPhase::Interrupted)
        {
            return Err(SendError::AlreadyReserved);
        }
        let account = uuid::Uuid::new_v4().to_string();
        let review = EnrollmentReview {
            scope: scope(&account),
            account: ServiceAccount::new("tilo", account),
            generation: 1,
            number: Some(self.identity.clone()),
        };
        *attempt = Some(Attempt {
            review: review.clone(),
            phase: EnrollmentPhase::Review,
        });
        drop(attempt);
        Ok(review)
    }

    /// Publish the reviewed `account` as `name`: ready once it is.
    fn reviewed(&self, account: &ServiceAccount, name: &str) -> Result<String, SendError> {
        let review = self
            .current(account, EnrollmentPhase::Review)
            .ok_or(SendError::Revoked)?;
        if (self.publish)(&account.account, name, &review.scope).is_err() {
            self.set_phase(account, EnrollmentPhase::Interrupted);
            return Err(SendError::AlreadyReserved);
        }
        self.set_phase(account, EnrollmentPhase::Ready);
        Ok(account.account.clone())
    }
}

impl EnrollmentHost for RoomEnrollment {
    fn prepare(&self, service: String) -> EnrollmentReviewFuture {
        Box::pin(std::future::ready(self.prepared(&service)))
    }

    fn begin(&self, _: String) -> PairingFuture {
        Box::pin(std::future::ready(Err(SendError::Unavailable)))
    }

    fn finish(&self, _: ServiceAccount, _: String) -> EnrollmentReviewFuture {
        Box::pin(std::future::ready(Err(SendError::Unavailable)))
    }

    fn review(&self, account: ServiceAccount, display_name: String) -> EnrollmentApprovalFuture {
        Box::pin(std::future::ready(self.reviewed(&account, &display_name)))
    }

    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        self.attempt
            .lock()
            .ok()
            .and_then(|attempt| {
                attempt
                    .as_ref()
                    .filter(|attempt| attempt.review.account == *account)
                    .map(|attempt| attempt.phase)
            })
            .unwrap_or(EnrollmentPhase::Empty)
    }

    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        let review = self.current(account, EnrollmentPhase::Ready)?;
        Some(LinkedAccount {
            account: account.clone(),
            generation: review.generation,
            scope: review.scope,
        })
    }

    fn cancel(&self, account: &ServiceAccount) {
        if let Ok(mut attempt) = self.attempt.lock()
            && let Some(current) = attempt
                .as_mut()
                .filter(|attempt| attempt.review.account == *account)
            && current.phase != EnrollmentPhase::Ready
        {
            current.phase = EnrollmentPhase::Interrupted;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Id;

    const ME: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
    const PEER: &str = "bb00000000000000000000000000000000000000000000000000000000000000";

    struct Closed;
    impl PrivateDoor for Closed {
        fn ask(&self, _: PrivateOperation) -> PrivateAnswer {
            Box::pin(std::future::ready(Err("closed".into())))
        }
        fn spawn(&self, _: Pin<Box<dyn Future<Output = ()> + Send>>) {}
    }

    fn route(recipient: &str, scope: &str) -> Route {
        Route {
            person: Id::from_raw("person"),
            association: Id::from_raw("association"),
            sender: Id::from_raw("account"),
            service: "tilo".into(),
            scope: scope.into(),
            recipient: recipient.into(),
            revision: "revision".into(),
        }
    }

    #[test]
    fn a_route_reaches_the_person_its_recipient_names_through_this_account_only() {
        let messaging = RoomMessaging::new(ME, Arc::new(Closed));
        let device = format!("{PEER}@{ME}");
        assert!(messaging.supports(&route(PEER, "tilo:account")));
        assert_eq!(
            messaging.peer(&route(&device, "tilo:account")).as_deref(),
            Some(PEER)
        );
        for refused in [
            route(PEER, "tilo:another"),
            route(ME, "tilo:account"),
            route("not-a-key", "tilo:account"),
            route(&format!("{PEER}@device"), "tilo:account"),
        ] {
            assert!(!messaging.supports(&refused));
        }
        let mut signal = route(PEER, "tilo:account");
        signal.service = "signal".into();
        assert!(!messaging.supports(&signal));
    }

    fn ready<T>(future: Pin<Box<dyn Future<Output = T> + Send>>) -> T {
        let mut future = future;
        match future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("answered at once"),
        }
    }

    #[test]
    fn signing_up_publishes_the_reviewed_account_once() {
        let published = Arc::new(Mutex::new(Vec::new()));
        let kept = published.clone();
        let enrollment = RoomEnrollment::new(
            "Alice",
            Arc::new(move |id: &str, name: &str, scope: &str| {
                let mut kept = kept.lock().unwrap();
                if !kept.is_empty() {
                    return Err("the original Tilo sender already exists".into());
                }
                kept.push((id.to_owned(), name.to_owned(), scope.to_owned()));
                drop(kept);
                Ok(())
            }),
        );
        assert!(ready(enrollment.prepare("signal".into())).is_err());
        let review = ready(enrollment.prepare("tilo".into())).unwrap();
        assert_eq!(review.scope, scope(&review.account.account));
        assert_eq!(review.number.as_deref(), Some("Alice"));
        assert_eq!(
            ready(enrollment.prepare("tilo".into())).err(),
            Some(SendError::AlreadyReserved),
            "one attempt at a time"
        );
        assert_eq!(enrollment.phase(&review.account), EnrollmentPhase::Review);
        assert!(enrollment.linked(&review.account).is_none());
        ready(enrollment.review(review.account.clone(), "Tilo".into())).unwrap();
        assert_eq!(enrollment.phase(&review.account), EnrollmentPhase::Ready);
        assert_eq!(
            enrollment.linked(&review.account).unwrap().scope,
            review.scope
        );
        assert_eq!(
            *published.lock().unwrap(),
            [(
                review.account.account.clone(),
                "Tilo".to_owned(),
                review.scope.clone()
            )]
        );
        assert!(
            ready(enrollment.review(review.account, "Tilo".into())).is_err(),
            "a published account is not reviewed again"
        );
    }

    #[test]
    fn a_refused_publication_interrupts_the_attempt() {
        let enrollment = RoomEnrollment::new(
            "Alice",
            Arc::new(|_: &str, _: &str, _: &str| Err("exists".into())),
        );
        let review = ready(enrollment.prepare("tilo".into())).unwrap();
        assert!(ready(enrollment.review(review.account.clone(), "Tilo".into())).is_err());
        assert_eq!(
            enrollment.phase(&review.account),
            EnrollmentPhase::Interrupted
        );
        assert!(
            ready(enrollment.prepare("tilo".into())).is_ok(),
            "another attempt may start"
        );
    }

    #[test]
    fn pages_end_at_the_cursor_and_say_whether_older_messages_wait() {
        let messages: Vec<_> = (1..=5u64)
            .map(|n| tilo_relay_protocol::rooms::private::PrivateMessage {
                id: format!("m{n}"),
                sender: if n % 2 == 0 { ME } else { PEER }.into(),
                sequence: n,
                received_at: 0,
                text: format!("text {n}"),
            })
            .collect();
        let route = route(PEER, "tilo:account");
        let newest = page(route.clone(), ME, &messages, None, 2).unwrap();
        assert_eq!(
            newest
                .messages
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["m4", "m5"]
        );
        assert!(newest.more);
        assert_eq!(newest.messages[0].direction, Direction::Outgoing);
        assert_eq!(newest.messages[1].delivery, None);
        let older = page(
            route.clone(),
            ME,
            &messages,
            Some(&HistoryCursor::before(&newest.messages[0])),
            2,
        )
        .unwrap();
        assert_eq!(older.messages[0].id, "m2");
        assert!(older.more);
        let missing = Message {
            id: "unknown".into(),
            ..newest.messages[0].clone()
        };
        assert_eq!(
            page(
                route,
                ME,
                &messages,
                Some(&HistoryCursor::before(&missing)),
                2
            ),
            Err(HistoryError::InvalidCursor)
        );
    }
}
