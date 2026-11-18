//! A window's way to its person's vault through its own session ([`crate::page_items`]).
//!
//! In a session that holds a workspace's Browser, a turn of an agent has a field of a site's
//! page filled from a vault item by reference. The window asks its session for the item's
//! text, on the private request that carries website sessions too, and the session asks the
//! workspace's server, which keeps the vault and decides.
//!
//! The window is told nothing by its launch: it asks its session once whether items are
//! lent to its pages ([`LendingQuestion`]), and only a window whose session says so offers
//! the fill. The session decides every single use again, so an answer that came to stand
//! wrongly would offer an action that is refused, and release nothing.
//!
//! What goes to the session is the page action, the item's reference and the site the
//! engine committed: who asked and why is what the server itself knows of the fill it
//! handed out. What comes back is the text, held once and printed by nothing, or that its
//! person is asked first, or a refusal with its reason.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use ui::collaboration::browser_service_sessions::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, Client,
    ItemField, Pending,
};
use ui::tilo_actions::InvocationId;
use ui::webview::{FillInto, ReleasedText};

use crate::page_items::{ItemAnswer, ItemAsk, ItemSource};

/// The most asks that wait for the one before them: the session answers one at a time.
const MAX_WAITING: usize = 16;
/// The most answers kept for fills that have not read theirs yet.
const MAX_ANSWERS: usize = 16;
/// How often a window asks its session whether items are lent before it stops asking.
const MAX_QUESTIONS: u8 = 6;

const TOO_MANY: &str =
    "Too many fills wait for this window's session already, so nothing was filled.";
const ANOTHERS_ANSWER: &str = "This window's session answered something that is not this \
                               fill's item, so nothing was filled.";

/// One request to the session that is on its way.
pub(crate) trait ItemAnswering {
    /// The session's answer, handed out once; `None` while there is none.
    fn poll(&mut self) -> Option<Result<Reply, String>>;
}

/// A window's own way to its session for vault items. It carries one request at a time, and
/// letting go of a request lets go of whatever would have answered it.
pub(crate) trait ItemRequests {
    /// Send `command`. Its answer is read from what comes back.
    ///
    /// # Errors
    ///
    /// The request could not be sent.
    fn request(&self, command: Command) -> Result<Box<dyn ItemAnswering>, String>;
}

/// The session's client, a window's own for vault items, with what wakes the window when
/// the session answers.
pub(crate) struct SessionRequests {
    client: Client,
    wake: ui::WakeHandle,
}

impl SessionRequests {
    pub(crate) const fn new(client: Client, wake: ui::WakeHandle) -> Self {
        Self { client, wake }
    }
}

impl ItemRequests for SessionRequests {
    fn request(&self, command: Command) -> Result<Box<dyn ItemAnswering>, String> {
        let pending = self.client.request(command, Some(self.wake.clone()))?;
        Ok(Box::new(pending))
    }
}

impl ItemAnswering for Pending {
    fn poll(&mut self) -> Option<Result<Reply, String>> {
        Self::poll(self)
    }
}

/// Whether a window's session has vault items used for its pages: asked of the session, and
/// asked again a bounded number of times while the session gives no answer.
pub(crate) struct LendingQuestion {
    requests: Rc<dyn ItemRequests>,
    asking: Option<Box<dyn ItemAnswering>>,
    /// How often the session was asked.
    asked: u8,
    settled: bool,
}

impl LendingQuestion {
    pub(crate) fn new(requests: Rc<dyn ItemRequests>) -> Self {
        Self {
            requests,
            asking: None,
            asked: 0,
            settled: false,
        }
    }

    /// Whether nothing more is asked: the session answered, or was asked often enough.
    pub(crate) const fn settled(&self) -> bool {
        self.settled
    }

    /// Ask the session, or read what it answered. The window's way to its person's vault,
    /// once, when the session says it lends items; `None` in every other pass.
    pub(crate) fn drive(&mut self) -> Option<Rc<dyn ItemSource>> {
        if self.settled {
            return None;
        }
        if self.asking.is_none() {
            if self.asked >= MAX_QUESTIONS {
                self.settled = true;
                return None;
            }
            self.asked = self.asked.saturating_add(1);
            match self.requests.request(Command::LendsItems {}) {
                Ok(asking) => self.asking = Some(asking),
                // A request that cannot be sent is tried again in a later pass.
                Err(_) => return None,
            }
        }
        let answered = self.asking.as_mut()?.poll()?;
        self.asking = None;
        match answered {
            Ok(Reply::ItemsLent { lent }) => {
                self.settled = true;
                lent.then(|| {
                    Rc::new(SessionItems::new(Rc::clone(&self.requests))) as Rc<dyn ItemSource>
                })
            }
            // Refused, or not answered in time: asked again in a later pass.
            _ => None,
        }
    }
}

impl std::fmt::Debug for LendingQuestion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LendingQuestion")
            .field("asked", &self.asked)
            .field("asking", &self.asking.is_some())
            .field("settled", &self.settled)
            .finish_non_exhaustive()
    }
}

/// A person's vault as a window reaches it through its session.
pub(crate) struct SessionItems {
    requests: Rc<dyn ItemRequests>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// The asks that wait for the one before them.
    waiting: VecDeque<Waiting>,
    /// The ask the session is answering, by the page action it is for.
    asking: Option<(InvocationId, Box<dyn ItemAnswering>)>,
    /// What the session answered, until the fill it is for reads it.
    answers: Vec<(InvocationId, ItemAnswer)>,
}

/// What the session is told of one ask: the page action, the item and the site.
struct Waiting {
    invocation: InvocationId,
    item: String,
    origin: String,
}

impl SessionItems {
    pub(crate) fn new(requests: Rc<dyn ItemRequests>) -> Self {
        Self {
            requests,
            state: RefCell::default(),
        }
    }

    /// Read what the session answered the ask it holds, and send it the next that waits.
    fn advance(&self) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        loop {
            if let Some((invocation, asking)) = state.asking.as_mut() {
                let Some(answered) = asking.poll() else {
                    return;
                };
                let invocation = *invocation;
                state.asking = None;
                keep(
                    &mut state.answers,
                    invocation,
                    answer_of(invocation, answered),
                );
            }
            let Some(next) = state.waiting.pop_front() else {
                return;
            };
            let command = Command::UseItem {
                invocation: next.invocation,
                item: next.item,
                origin: next.origin,
            };
            match self.requests.request(command) {
                Ok(asking) => state.asking = Some((next.invocation, asking)),
                Err(error) => keep(
                    &mut state.answers,
                    next.invocation,
                    ItemAnswer::Refused {
                        reason: unanswered(&error),
                    },
                ),
            }
        }
    }
}

impl ItemSource for SessionItems {
    fn ask(&self, ask: ItemAsk) {
        {
            let mut state = self.state.borrow_mut();
            if state.waiting.len() >= MAX_WAITING {
                keep(
                    &mut state.answers,
                    ask.invocation,
                    ItemAnswer::Refused {
                        reason: TOO_MANY.to_owned(),
                    },
                );
                return;
            }
            state.waiting.push_back(Waiting {
                invocation: ask.invocation,
                item: ask.item,
                origin: ask.origin,
            });
        }
        self.advance();
    }

    fn answered(&self, invocation: InvocationId) -> Option<ItemAnswer> {
        self.advance();
        let mut state = self.state.borrow_mut();
        let index = state
            .answers
            .iter()
            .position(|(asked, _)| *asked == invocation)?;
        Some(state.answers.swap_remove(index).1)
    }

    fn forget(&self, invocation: InvocationId) {
        {
            let mut state = self.state.borrow_mut();
            state
                .waiting
                .retain(|waiting| waiting.invocation != invocation);
            state.answers.retain(|(asked, _)| *asked != invocation);
            // Letting go of the request lets go of what would have answered it: a text that
            // comes later is held by nothing.
            if state
                .asking
                .as_ref()
                .is_some_and(|(asked, _)| *asked == invocation)
            {
                state.asking = None;
            }
        }
        self.advance();
    }
}

impl std::fmt::Debug for SessionItems {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("SessionItems")
            .field("waiting", &state.waiting.len())
            .field("asking", &state.asking.is_some())
            .field("answers", &state.answers.len())
            .finish_non_exhaustive()
    }
}

/// Keep `answer` for the fill `invocation` to read. The oldest answer nobody read is let go
/// when there are too many, and what it held with it.
fn keep(
    answers: &mut Vec<(InvocationId, ItemAnswer)>,
    invocation: InvocationId,
    answer: ItemAnswer,
) {
    if answers.len() >= MAX_ANSWERS {
        answers.remove(0);
    }
    answers.push((invocation, answer));
}

/// What the fill `invocation` is answered, from what the session answered its request.
fn answer_of(invocation: InvocationId, answered: Result<Reply, String>) -> ItemAnswer {
    match answered {
        Ok(Reply::ItemText {
            invocation: answers,
            mut text,
            into,
        }) if answers == invocation => ItemAnswer::Released {
            // The text moves on as it is: nothing copies it, and what held it is left empty.
            text: ReleasedText::new(std::mem::take(&mut *text)),
            into: match into {
                ItemField::Password => FillInto::Password,
                ItemField::Code => FillInto::Code,
                ItemField::Value => FillInto::Value,
            },
        },
        Ok(Reply::ItemAsked {
            invocation: answers,
            words,
        }) if answers == invocation => ItemAnswer::Asked { words },
        Ok(Reply::ItemRefused {
            invocation: answers,
            reason,
        }) if answers == invocation => ItemAnswer::Refused { reason },
        Ok(_) => ItemAnswer::Refused {
            reason: ANOTHERS_ANSWER.to_owned(),
        },
        Err(error) => ItemAnswer::Refused {
            reason: unanswered(&error),
        },
    }
}

/// What a fill is refused with when its session gave no answer at all.
fn unanswered(error: &str) -> String {
    format!(
        "This window's session gave no answer for the vault item ({error}), so nothing was \
         filled."
    )
}

/// A window's session, stood in for in tests: what it was sent, and what a test has it
/// answer.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "in a test's stand-in a failed step is the assertion"
)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::{Command, ItemAnswering, ItemRequests, Reply};

    type Kept = Rc<RefCell<Option<Result<Reply, String>>>>;

    #[derive(Default)]
    pub(crate) struct Session {
        sent: RefCell<Vec<(Command, Kept)>>,
        busy: Rc<Cell<bool>>,
        /// Whether no request can be sent at all.
        pub(crate) down: Cell<bool>,
    }

    struct Asked {
        answer: Kept,
        busy: Rc<Cell<bool>>,
    }

    impl ItemAnswering for Asked {
        fn poll(&mut self) -> Option<Result<Reply, String>> {
            let answer = self.answer.borrow_mut().take()?;
            self.busy.set(false);
            Some(answer)
        }
    }

    impl Drop for Asked {
        fn drop(&mut self) {
            self.busy.set(false);
        }
    }

    impl ItemRequests for Session {
        fn request(&self, command: Command) -> Result<Box<dyn ItemAnswering>, String> {
            if self.down.get() {
                return Err("the session is not there".to_owned());
            }
            assert!(
                !self.busy.replace(true),
                "the session is sent one request at a time"
            );
            let answer = Kept::default();
            self.sent.borrow_mut().push((command, Rc::clone(&answer)));
            Ok(Box::new(Asked {
                answer,
                busy: Rc::clone(&self.busy),
            }))
        }
    }

    impl Session {
        /// What it was sent so far, in order.
        pub(crate) fn sent(&self) -> Vec<Command> {
            self.sent
                .borrow()
                .iter()
                .map(|(command, _)| command.clone())
                .collect()
        }

        /// Answer the request it was sent as its `index`th.
        pub(crate) fn answers(&self, index: usize, answer: Result<Reply, String>) {
            let sent = self.sent.borrow();
            let (_, kept) = sent.get(index).expect("the session was sent that request");
            *kept.borrow_mut() = Some(answer);
        }

        /// Whether a request is on its way that nobody read the answer of or let go of.
        pub(crate) fn busy(&self) -> bool {
            self.busy.get()
        }

        /// The answer that carries `text` for the fill `invocation`, as it comes off the
        /// connection.
        pub(crate) fn text(
            invocation: ui::tilo_actions::InvocationId,
            text: &str,
            into: &str,
        ) -> Result<Reply, String> {
            Reply::decode(
                serde_json::json!({
                    "kind": "item-text",
                    "invocation": invocation,
                    "text": text,
                    "into": into,
                })
                .to_string()
                .as_bytes(),
            )
            .map_err(str::to_owned)
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test a failed step is the assertion"
)]
mod tests {
    use super::testing::Session;
    use super::*;

    /// A vault item's reference, made up.
    const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
    /// Where the made-up item is kept for.
    const ORIGIN: &str = "https://login.example.test";
    /// What the made-up item keeps: nobody's password, and it says so.
    const MADE_UP_TEXT: &str = "made-up item text, nobody's password/7+7%";
    /// Why the turn says it needs the item: the session is told nothing of it.
    const REASON: &str = "To sign in for Ada, who asked.";

    fn asks(invocation: InvocationId) -> ItemAsk {
        ItemAsk {
            invocation,
            item: ITEM.into(),
            origin: ORIGIN.into(),
            reason: REASON.into(),
            by: None,
        }
    }

    fn uses(invocation: InvocationId) -> Command {
        Command::UseItem {
            invocation,
            item: ITEM.into(),
            origin: ORIGIN.into(),
        }
    }

    fn source() -> (Rc<Session>, SessionItems) {
        let session = Rc::new(Session::default());
        let items = SessionItems::new(session.clone());
        (session, items)
    }

    /// Whether `answer` refuses with `reason` among its words, and holds nothing of the text.
    fn refuses(answer: Option<ItemAnswer>, reason: &str) -> bool {
        matches!(
            answer,
            Some(ItemAnswer::Refused { reason: said })
                if said.contains(reason) && !said.contains(MADE_UP_TEXT)
        )
    }

    #[test]
    fn the_session_is_told_the_page_action_the_item_and_the_site_and_answers_in_three_ways() {
        let (session, items) = source();
        let fill = InvocationId::new();
        items.ask(asks(fill));
        // The page action, the item by its reference and the site: who asked and why stay
        // with the window.
        assert_eq!(session.sent(), [uses(fill)]);
        let carried = session.sent()[0].encode().unwrap();
        assert!(!String::from_utf8_lossy(&carried).contains("Ada"));
        assert!(
            items.answered(fill).is_none(),
            "nothing before the session answers"
        );

        // The text comes back held once, with the kind of field it is for, and is handed out
        // once.
        session.answers(0, Session::text(fill, MADE_UP_TEXT, "password"));
        let Some(ItemAnswer::Released { text, into }) = items.answered(fill) else {
            panic!("the session's text is the fill's answer");
        };
        assert_eq!(into, FillInto::Password);
        assert_eq!(text.read(|held| held == MADE_UP_TEXT), Some(true));
        assert!(
            items.answered(fill).is_none(),
            "an answer is handed out once"
        );
        assert!(!session.busy());

        // That its person is asked first, and a refusal with its reason, in the words given.
        let (first, refused) = (InvocationId::new(), InvocationId::new());
        items.ask(asks(first));
        session.answers(1, Ok(Reply::item_asked(first, "Ada is asked first.")));
        assert!(matches!(
            items.answered(first),
            Some(ItemAnswer::Asked { words }) if words == "Ada is asked first."
        ));
        items.ask(asks(refused));
        session.answers(
            2,
            Ok(Reply::item_refused(
                refused,
                "Ada said no, so the item is not used.",
            )),
        );
        assert!(refuses(
            items.answered(refused),
            "Ada said no, so the item is not used."
        ));
        for (kind, into) in [("code", FillInto::Code), ("value", FillInto::Value)] {
            let fill = InvocationId::new();
            items.ask(asks(fill));
            let sent = session.sent().len() - 1;
            session.answers(sent, Session::text(fill, MADE_UP_TEXT, kind));
            assert!(matches!(
                items.answered(fill),
                Some(ItemAnswer::Released { into: for_field, .. }) if for_field == into
            ));
        }
    }

    #[test]
    fn the_session_is_asked_one_item_at_a_time_in_the_order_the_fills_asked() {
        let (session, items) = source();
        let fills = [
            InvocationId::new(),
            InvocationId::new(),
            InvocationId::new(),
        ];
        for fill in fills {
            items.ask(asks(fill));
        }
        assert_eq!(session.sent(), [uses(fills[0])]);
        // Reading another fill's answer sends nothing while the first is on its way.
        assert!(items.answered(fills[2]).is_none());
        assert_eq!(session.sent().len(), 1);
        session.answers(0, Session::text(fills[0], MADE_UP_TEXT, "password"));
        assert!(items.answered(fills[1]).is_none());
        assert_eq!(session.sent(), [uses(fills[0]), uses(fills[1])]);
        assert!(matches!(
            items.answered(fills[0]),
            Some(ItemAnswer::Released { .. })
        ));
        session.answers(1, Ok(Reply::item_refused(fills[1], "No.")));
        assert!(refuses(items.answered(fills[1]), "No."));
        assert_eq!(session.sent().len(), 3);

        // More asks than may wait are refused, each with that reason.
        let (session, items) = source();
        let many: Vec<InvocationId> = (0..MAX_WAITING + 2).map(|_| InvocationId::new()).collect();
        for fill in &many {
            items.ask(asks(*fill));
        }
        assert_eq!(session.sent().len(), 1);
        assert!(refuses(
            items.answered(many[MAX_WAITING + 1]),
            "Too many fills"
        ));
    }

    #[test]
    fn anothers_answer_a_session_that_is_not_there_and_a_request_that_ends_release_nothing() {
        // An answer for another page action, even one that carries a text.
        let (session, items) = source();
        let fill = InvocationId::new();
        items.ask(asks(fill));
        session.answers(
            0,
            Session::text(InvocationId::new(), MADE_UP_TEXT, "password"),
        );
        assert!(refuses(items.answered(fill), "not this fill's item"));
        // An answer that is not an item's at all.
        let fill = InvocationId::new();
        items.ask(asks(fill));
        session.answers(1, Ok(Reply::ItemsLent { lent: true }));
        assert!(refuses(items.answered(fill), "not this fill's item"));
        // A request that ends without the session's answer, as after its ten seconds.
        let fill = InvocationId::new();
        items.ask(asks(fill));
        session.answers(2, Err("This account could not be opened".to_owned()));
        assert!(refuses(
            items.answered(fill),
            "gave no answer for the vault item (This account could not be opened)"
        ));
        // A session no request reaches.
        session.down.set(true);
        let fill = InvocationId::new();
        items.ask(asks(fill));
        assert!(refuses(items.answered(fill), "the session is not there"));
        assert_eq!(session.sent().len(), 3);
    }

    #[test]
    fn a_fill_that_stopped_waiting_is_answered_for_no_more() {
        let (session, items) = source();
        let (gone, next, never) = (
            InvocationId::new(),
            InvocationId::new(),
            InvocationId::new(),
        );
        items.ask(asks(gone));
        items.ask(asks(next));
        items.ask(asks(never));
        // The wait for the first is over: its request is let go, and the next is sent.
        items.forget(gone);
        items.forget(never);
        assert_eq!(session.sent(), [uses(gone), uses(next)]);
        // What the session answers the first now reaches nobody.
        session.answers(0, Session::text(gone, MADE_UP_TEXT, "password"));
        assert!(items.answered(gone).is_none());
        assert!(items.answered(never).is_none());
        session.answers(1, Session::text(next, MADE_UP_TEXT, "password"));
        assert!(matches!(
            items.answered(next),
            Some(ItemAnswer::Released { .. })
        ));
        // An answer nobody read yet is let go with its fill, and what it held with it.
        let unread = InvocationId::new();
        items.ask(asks(unread));
        session.answers(2, Session::text(unread, MADE_UP_TEXT, "password"));
        items.forget(unread);
        assert!(items.answered(unread).is_none());
        assert_eq!(session.sent().len(), 3, "nothing was asked a second time");
    }

    #[test]
    fn a_window_learns_from_its_session_whether_items_are_lent() {
        // A session that lends items: the window is handed its way to the vault, once.
        let session = Rc::new(Session::default());
        let mut question = LendingQuestion::new(session.clone());
        assert!(question.drive().is_none());
        assert_eq!(session.sent(), [Command::LendsItems {}]);
        assert!(
            question.drive().is_none(),
            "nothing before the session answers"
        );
        assert_eq!(session.sent().len(), 1);
        session.answers(0, Ok(Reply::ItemsLent { lent: true }));
        assert!(question.drive().is_some());
        assert!(question.settled());
        assert!(question.drive().is_none());

        // A person's own session lends none: nothing is handed over and nothing asked again.
        let session = Rc::new(Session::default());
        let mut question = LendingQuestion::new(session.clone());
        assert!(question.drive().is_none());
        session.answers(0, Ok(Reply::ItemsLent { lent: false }));
        assert!(question.drive().is_none());
        assert!(question.settled());
        assert!(question.drive().is_none());
        assert_eq!(session.sent().len(), 1);

        // A session that refuses the question is asked again, a bounded number of times,
        // and the window is handed nothing.
        let session = Rc::new(Session::default());
        let mut question = LendingQuestion::new(session.clone());
        for _ in 0..40 {
            assert!(question.drive().is_none());
            if let Some(index) = session.sent().len().checked_sub(1)
                && session.busy()
            {
                session.answers(index, Err("private service-session request refused".into()));
            }
        }
        assert!(question.settled());
        assert_eq!(session.sent().len(), usize::from(MAX_QUESTIONS));
        assert!(
            session
                .sent()
                .iter()
                .all(|sent| *sent == Command::LendsItems {})
        );
    }

    #[test]
    fn nothing_printed_of_a_windows_way_to_its_session_holds_an_items_text() {
        let (session, items) = source();
        let (held, read) = (InvocationId::new(), InvocationId::new());
        items.ask(asks(read));
        session.answers(0, Session::text(read, MADE_UP_TEXT, "password"));
        let answer = items.answered(read);
        items.ask(asks(held));
        session.answers(1, Session::text(held, MADE_UP_TEXT, "code"));
        // The second answer is read off the connection and kept, unread by its fill.
        assert!(items.answered(read).is_none());
        let question = LendingQuestion::new(session.clone());
        for printed in [
            format!("{items:?}"),
            format!("{items:#?}"),
            format!("{answer:?}"),
            format!("{answer:#?}"),
            format!("{question:?}"),
            format!("{:?}", session.sent()),
            format!("{:?}", Session::text(held, MADE_UP_TEXT, "password")),
            // What a text that is no answer is refused with.
            format!("{:?}", Session::text(held, MADE_UP_TEXT, "elsewhere")),
            unanswered("This account could not be opened"),
        ] {
            assert!(
                !printed.contains(MADE_UP_TEXT) && !printed.contains("made-up"),
                "one of the prints holds the item's text"
            );
        }
    }
}
