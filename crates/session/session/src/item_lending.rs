//! Vault items for the pages of a workspace's Browser, as this session has them used.
//!
//! An agent that signs in to a site names an item of its person's vault and never holds what
//! the item keeps. The workspace's server hands the Browser window a page action that fills
//! a field from that item by reference, and the window asks its session for the item's text
//! ([`runtime_collab::BrowserServiceSessionCommand::UseItem`]). The session keeps no vault of
//! the person's: it asks the server, as the device the server placed, and the server decides.
//!
//! What stands here is what the session decides itself, before anyone is asked:
//!
//! - only Browser asks, and only where this session holds that workspace's Browser: a
//!   person's own session answers no such ask;
//! - one page action is asked for its item once;
//! - the server is waited for a bounded time, on the session's own tasks, so neither the
//!   connection that asked nor the session's loop waits for it.
//!
//! Every refusal is an answer of its own that says why, and releases nothing. The text
//! itself passes through here only inside the answer, which wipes itself and prints nothing.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use tilo_actions::InvocationId;
use tilo_actions::remote::environment_device::ItemUse;
use tilo_workspace_service::apps::services::ItemAnswering;
use ui::apps::BROWSER;

/// How long the workspace's server is waited for: less than the ten seconds Browser's own
/// request waits, so a server that is silent is answered for with its reason.
const SERVER_WAIT: Duration = Duration::from_secs(8);
/// The most page actions remembered as asked for: the newest of them.
const MAX_ASKED: usize = 256;

const ONLY_BROWSER: &str =
    "Only Browser has a field of a page filled from a vault item, so nothing was released.";
const HOLDS_NO_BROWSER: &str = "This session holds no Browser of that workspace, so it has no \
                                vault item used for a page: nothing was released.";
const NO_SERVER_YET: &str = "This session cannot ask its workspace's server yet, so nothing \
                             was released. Ask for the fill again in a moment.";
const ASKED_ALREADY: &str = "This page action was asked for its item already, and an item is \
                             released once for one fill: nothing was released.";
const WAIT_OVER: &str =
    "The workspace's server did not answer in time, so nothing was released and nothing filled.";
const ASK_ENDED: &str =
    "The ask to the workspace's server ended before it answered, so nothing was released.";
const NO_TASKS: &str = "This session has no task to ask its workspace's server on, so nothing \
                        was released.";

/// What the server's answer to one use of an item becomes for Browser, once it has come.
pub(crate) type Lending = Pin<Box<dyn Future<Output = Reply> + Send + 'static>>;

/// Whoever asks the workspace's server to use an item, as the device it placed, and opens
/// what it answers.
pub(crate) trait ItemLender: Send + Sync {
    /// Ask for `used`: the text, that its person is asked first, or the refusal with its
    /// reason, each as the answer to the page action `used` names.
    fn lend(&self, used: ItemUse) -> Lending;
}

impl<F> ItemLender for F
where
    F: Fn(ItemUse) -> Lending + Send + Sync,
{
    fn lend(&self, used: ItemUse) -> Lending {
        self(used)
    }
}

/// What this session answers Browser's asks for vault items with: nothing in a person's own
/// session, and the workspace's server's word in a session that holds a workspace's Browser.
pub(crate) struct ItemLending {
    state: Mutex<State>,
    /// How long the server is waited for.
    wait: Duration,
}

#[derive(Default)]
struct State {
    /// The workspace whose Browser this session holds, by its UUID.
    workspace: Option<String>,
    /// Who asks that workspace's server, once it can be asked.
    lender: Option<Arc<dyn ItemLender>>,
    /// The page actions asked for their item already.
    asked: VecDeque<InvocationId>,
}

impl Default for ItemLending {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            wait: SERVER_WAIT,
        }
    }
}

impl std::fmt::Debug for ItemLending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("ItemLending")
            .field("holds_a_browser", &state.workspace.is_some())
            .field("asks_its_server", &state.lender.is_some())
            .field("asked", &state.asked.len())
            .finish_non_exhaustive()
    }
}

impl ItemLending {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This session holds `workspace`'s Browser from now on.
    pub(crate) fn hold(&self, workspace: &str) {
        self.state().workspace = Some(workspace.to_owned());
    }

    /// The workspace's server is asked through `lender` from now on.
    pub(crate) fn lent_by(&self, lender: Arc<dyn ItemLender>) {
        self.state().lender = Some(lender);
    }

    /// Whether this session has vault items used for the pages of `workspace`'s Browser: it
    /// holds that Browser, whether or not its server can be asked yet.
    pub(crate) fn lends(&self, workspace: &str) -> bool {
        self.state().workspace.as_deref() == Some(workspace)
    }

    /// Answer `command`, an item's use that `app` sent on its own connection in `workspace`.
    ///
    /// It returns at once. A refusal decided here is already answered; an ask to the server
    /// goes on the session's own tasks and is waited for no longer than the wait.
    pub(crate) fn answer(&self, app: &str, workspace: &str, command: &Command) -> ItemAnswering {
        let Command::UseItem {
            invocation,
            item,
            origin,
        } = command
        else {
            return Box::pin(std::future::ready(Err("not an item's use".to_owned())));
        };
        let invocation = *invocation;
        let refused = |reason: &str| -> ItemAnswering {
            Box::pin(std::future::ready(Ok(Reply::item_refused(
                invocation, reason,
            ))))
        };
        if app != BROWSER {
            return refused(ONLY_BROWSER);
        }
        let lender = {
            let mut state = self.state();
            if state.workspace.as_deref() != Some(workspace) {
                return refused(HOLDS_NO_BROWSER);
            }
            let Some(lender) = state.lender.clone() else {
                return refused(NO_SERVER_YET);
            };
            if state.asked.contains(&invocation) {
                return refused(ASKED_ALREADY);
            }
            if state.asked.len() >= MAX_ASKED {
                state.asked.pop_front();
            }
            state.asked.push_back(invocation);
            drop(state);
            lender
        };
        let Ok(tasks) = tilo_rt::try_app() else {
            return refused(NO_TASKS);
        };
        let lending = lender.lend(ItemUse {
            invocation,
            item: item.clone(),
            origin: origin.clone(),
        });
        let wait = self.wait;
        // When the wait is over the ask is dropped where it stands: an answer that comes
        // later is opened by nobody.
        let asked = tasks.spawn(async move { tokio::time::timeout(wait, lending).await });
        Box::pin(async move {
            Ok(match asked.await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Reply::item_refused(invocation, WAIT_OVER),
                Err(_) => Reply::item_refused(invocation, ASK_ENDED),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault item's reference, made up.
    const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
    /// Where the made-up item is kept for.
    const ORIGIN: &str = "https://login.example.test";
    /// The workspace whose Browser the session holds.
    const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
    /// What the made-up item keeps: nobody's password, and it says so.
    const MADE_UP_TEXT: &str = "made-up item text, nobody's password";
    /// What the server says of an item no fill it handed out waits for.
    const NO_FILL: &str = "No fill this server handed this window waits for that item";

    /// What the workspace's server was asked, in order.
    type Asked = Arc<Mutex<Vec<ItemUse>>>;

    /// The workspace's server, stood in for: it notes what it was asked and answers what
    /// `answers` says, or never where that is `None`.
    fn server(
        asked: &Asked,
        answers: impl Fn(InvocationId) -> Option<Reply> + Send + Sync + 'static,
    ) -> Arc<dyn ItemLender> {
        let asked = Arc::clone(asked);
        Arc::new(move |used: ItemUse| -> Lending {
            let invocation = used.invocation;
            asked.lock().unwrap().push(used);
            match answers(invocation) {
                Some(reply) => Box::pin(std::future::ready(reply)),
                None => Box::pin(std::future::pending()),
            }
        })
    }

    fn released(invocation: InvocationId) -> Reply {
        Reply::ItemText {
            invocation,
            text: zeroize::Zeroizing::new(MADE_UP_TEXT.to_owned()),
            into: runtime_collab::ItemField::Password,
        }
    }

    fn uses(invocation: InvocationId) -> Command {
        Command::UseItem {
            invocation,
            item: ITEM.into(),
            origin: ORIGIN.into(),
        }
    }

    /// What `answering` comes to, as the connection's answer waits for it.
    fn settled(answering: ItemAnswering) -> Reply {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(answering)
            .expect("an item's use is always answered")
    }

    /// Whether `reply` refuses `invocation` with `reason` among its words.
    fn refuses(reply: &Reply, invocation: InvocationId, reason: &str) -> bool {
        matches!(
            reply,
            Reply::ItemRefused { invocation: refused, reason: said }
                if *refused == invocation && said.contains(reason) && !said.contains(MADE_UP_TEXT)
        )
    }

    #[test]
    fn only_a_session_that_holds_the_workspaces_browser_has_an_item_used_and_each_refusal_says_why()
    {
        let asked = Asked::default();
        let fill = InvocationId::new();

        // A person's own session: it holds no workspace's Browser.
        let own = ItemLending::default();
        assert!(!own.lends(WORKSPACE));
        own.lent_by(server(&asked, |invocation| Some(released(invocation))));
        assert!(refuses(
            &settled(own.answer(BROWSER, WORKSPACE, &uses(fill))),
            fill,
            "holds no Browser of that workspace"
        ));

        // A session that holds the workspace's Browser says so before it can ask its server,
        // and releases nothing until it can.
        let lending = ItemLending::default();
        lending.hold(WORKSPACE);
        assert!(lending.lends(WORKSPACE));
        assert!(!lending.lends("22222222-2222-4222-8222-222222222222"));
        assert!(refuses(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(fill))),
            fill,
            "cannot ask its workspace's server yet"
        ));

        lending.lent_by(server(&asked, |invocation| Some(released(invocation))));
        // An app that is not Browser, and Browser in a workspace this session holds no
        // Browser of.
        assert!(refuses(
            &settled(lending.answer("so.tilo.mail", WORKSPACE, &uses(fill))),
            fill,
            "Only Browser"
        ));
        assert!(refuses(
            &settled(lending.answer(BROWSER, "22222222-2222-4222-8222-222222222222", &uses(fill))),
            fill,
            "holds no Browser of that workspace"
        ));
        let asked_so_far = asked.lock().unwrap().len();
        assert_eq!(asked_so_far, 0, "nobody was asked for any of the refusals");

        // Browser's ask is carried to the server as it was named, and the text comes back
        // for that page action.
        let reply = settled(lending.answer(BROWSER, WORKSPACE, &uses(fill)));
        assert!(matches!(
            &reply,
            Reply::ItemText { invocation, text, into: runtime_collab::ItemField::Password }
                if *invocation == fill && text.as_str() == MADE_UP_TEXT
        ));
        let carried = asked.lock().unwrap().clone();
        assert_eq!(
            carried,
            [ItemUse {
                invocation: fill,
                item: ITEM.into(),
                origin: ORIGIN.into(),
            }]
        );

        // A second ask for the same fill is refused here, and the server is not asked again.
        assert!(refuses(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(fill))),
            fill,
            "asked for its item already"
        ));
        let asked_in_all = asked.lock().unwrap().len();
        assert_eq!(asked_in_all, 1);

        // What is no item's use is no answer at all.
        assert!(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(lending.answer(BROWSER, WORKSPACE, &Command::List {}))
                .is_err()
        );
    }

    #[test]
    fn the_servers_refusal_and_its_question_come_back_in_its_own_words() {
        let asked = Asked::default();
        let lending = ItemLending::default();
        lending.hold(WORKSPACE);
        let (unasked, first) = (InvocationId::new(), InvocationId::new());
        lending.lent_by(server(&asked, move |invocation| {
            Some(if invocation == first {
                Reply::item_asked(
                    invocation,
                    "Ada is asked whether the assistant may sign in.",
                )
            } else {
                Reply::item_refused(invocation, NO_FILL)
            })
        }));
        assert!(refuses(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(unasked))),
            unasked,
            NO_FILL
        ));
        assert!(matches!(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(first))),
            Reply::ItemAsked { invocation, words }
                if *invocation == first && words == "Ada is asked whether the assistant may sign in."
        ));
    }

    #[test]
    fn a_server_that_is_silent_is_waited_for_off_the_dispatch_and_only_for_the_wait() {
        let asked = Asked::default();
        let lending = ItemLending {
            wait: Duration::from_millis(100),
            ..ItemLending::default()
        };
        lending.hold(WORKSPACE);
        let (silent, late) = (InvocationId::new(), InvocationId::new());
        let slow = Arc::clone(&asked);
        lending.lent_by(Arc::new(move |used: ItemUse| -> Lending {
            let invocation = used.invocation;
            slow.lock().unwrap().push(used);
            if invocation == late {
                // An answer with the text, after the wait is over.
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    released(invocation)
                })
            } else {
                Box::pin(std::future::pending())
            }
        }));

        // The call that starts the ask comes back while the server has said nothing: what
        // waits is the answer.
        let mut answering = lending.answer(BROWSER, WORKSPACE, &uses(silent));
        let asked_at_once = asked.lock().unwrap().len();
        assert_eq!(asked_at_once, 1);
        assert!(
            answering
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending(),
            "nothing is answered before the server said anything or the wait is over"
        );
        let started = std::time::Instant::now();
        assert!(refuses(
            &settled(answering),
            silent,
            "did not answer in time"
        ));
        assert!(started.elapsed() < Duration::from_secs(2));

        // A text that comes after the wait is over is released to nobody.
        assert!(refuses(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(late))),
            late,
            "did not answer in time"
        ));
        // And the fill it was for is not asked for again.
        assert!(refuses(
            &settled(lending.answer(BROWSER, WORKSPACE, &uses(late))),
            late,
            "asked for its item already"
        ));
    }

    #[test]
    fn nothing_printed_of_the_lending_holds_an_items_text() {
        let lending = ItemLending::default();
        lending.hold(WORKSPACE);
        lending.lent_by(server(&Asked::default(), |invocation| {
            Some(released(invocation))
        }));
        let fill = InvocationId::new();
        let reply = settled(lending.answer(BROWSER, WORKSPACE, &uses(fill)));
        for printed in [
            format!("{lending:?}"),
            format!("{lending:#?}"),
            format!("{reply:?}"),
            format!("{:?}", uses(fill)),
        ] {
            assert!(
                !printed.contains(MADE_UP_TEXT) && !printed.contains("made-up"),
                "one of the prints holds the item's text"
            );
        }
    }
}
