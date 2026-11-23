//! Private, one-shot requests to the session's services, on the app's own
//! route. These replies never enter the document, history or durable event
//! queues.
//!
//! A window takes its client as it is built ([`Client::capture`]) and uses it
//! whenever it needs the session: while its route is still joining, once
//! the session admitted it, or in a workspace whose record this device's own
//! channel serves. What the session answers is the session's decision on
//! that route, taken per request; the client only carries the request there.
use super::native_initial::{InitialReadBridge, service_capture_bridge};
pub use runtime_collab::{
    BrowserServiceSessionCommand, BrowserServiceSessionDescriptor, BrowserServiceSessionReply,
    ItemField,
};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Clone)]
pub struct Client {
    to: To,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Who answers a client's requests.
#[derive(Clone)]
enum To {
    /// The session, on the window's own route.
    Session(InitialReadBridge),
    /// A test, answering as the session would ([`Client::answering`]).
    #[cfg(feature = "test-support")]
    Answering(std::sync::Arc<Answer>),
}

/// What a test answers each request with, as the session would.
#[cfg(feature = "test-support")]
type Answer = dyn Fn(BrowserServiceSessionCommand) -> Result<BrowserServiceSessionReply, String>
    + Send
    + Sync;

impl Client {
    /// A client whose requests `answer` answers, as the session would: for a test of an app's
    /// side of these requests, which runs no session.
    #[cfg(feature = "test-support")]
    pub fn answering(
        answer: impl Fn(BrowserServiceSessionCommand) -> Result<BrowserServiceSessionReply, String>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            to: To::Answering(std::sync::Arc::new(answer)),
            busy: Default::default(),
        }
    }

    /// The workspace the client's route is in: `None` for one a test answers.
    fn workspace(&self) -> Option<String> {
        match &self.to {
            To::Session(bridge) => Some(bridge.workspace()),
            #[cfg(feature = "test-support")]
            To::Answering(_) => None,
        }
    }

    /// Capture this window's connection to the session while the native host
    /// constructs its component: `None` outside a session (a test, a
    /// `--data-dir` profile), which keeps its own stores.
    ///
    /// The captured connection names the app, not a moment: it serves the
    /// window's requests before its route joined, after, and across a record
    /// switch on the same app's route.
    pub fn capture() -> Option<Self> {
        Self::captured().ok()
    }
    /// [`Self::capture`], or why there is none.
    fn captured() -> Result<Self, &'static str> {
        service_capture_bridge().map(|bridge| Self {
            to: To::Session(bridge),
            busy: Default::default(),
        })
    }
    /// [`Self::capture`] while the window is built; after that, the client this thread captured
    /// for a window in `workspace` then. A view the framework builds again later (when the
    /// window moves to another record of its app, as Mail's does once the workspace's mailbox
    /// arrives) is no longer being built, and still reaches the session on its own route.
    pub fn capture_kept(workspace: Option<&str>) -> Option<Self> {
        thread_local! {
            static KEPT: std::cell::RefCell<Vec<Client>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            let why = match Self::captured() {
                Ok(captured) => {
                    let workspace = captured.workspace();
                    kept.retain(|client| client.workspace() != workspace);
                    kept.push(captured.another());
                    return Some(captured);
                }
                Err(why) => why,
            };
            let found = workspace.and_then(|workspace| {
                kept.iter()
                    .find(|client| client.workspace().as_deref() == Some(workspace))
                    .map(Self::another)
            });
            // A window an app builds in a workspace runs in a session: one with no route to it
            // cannot ask the session for anything, which its person sees as a failure later. The
            // session's own windows (a thread that never built an app's window, or a connection
            // that is no broker's) need no route.
            let app_window = !crate::native_initial::session_own(why)
                && (why != crate::native_initial::NOT_BUILDING || !kept.is_empty());
            if found.is_none() && workspace.is_some() && app_window {
                tracing::warn!(why, "this window has no route to the session's services");
            }
            found
        })
    }
    /// Another client on the same route, with its own request in flight: a clone shares the
    /// one this client has.
    #[must_use]
    pub fn another(&self) -> Self {
        Self {
            to: self.to.clone(),
            busy: Default::default(),
        }
    }
    /// Submit without waiting on the frame thread.
    pub fn request(
        &self,
        command: BrowserServiceSessionCommand,
        wake: Option<crate::WakeHandle>,
    ) -> Result<Pending, String> {
        if !command.valid()
            || wake.as_ref().is_some_and(|wake| !wake.is_active())
            || self
                .busy
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_err()
        {
            return Err("This account could not be opened".into());
        }
        let asked = match &self.to {
            To::Session(bridge) => bridge.service_request(command, wake),
            #[cfg(feature = "test-support")]
            To::Answering(answer) => {
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(answer(command))
                    .map(|()| rx)
                    .map_err(|error| error.to_string())
            }
        };
        let received = match asked {
            Ok(received) => received,
            Err(error) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                return Err(error);
            }
        };
        Ok(Pending {
            received,
            busy: self.busy.clone(),
            deadline: crate::time::now() + crate::time::Duration::from_secs(10),
            done: false,
        })
    }
}
/// What a request the session did not answer before its deadline is told.
const UNANSWERED: &str = "This device did not answer in time. Try again.";

pub struct Pending {
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    received: Receiver<Result<BrowserServiceSessionReply, String>>,
    deadline: crate::time::Instant,
    done: bool,
}
impl Pending {
    /// A reply is returned at most once; dropping this object drops private ownership.
    pub fn poll(&mut self) -> Option<Result<BrowserServiceSessionReply, String>> {
        if self.done {
            return None;
        }
        let result = match self.received.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) if crate::time::now() < self.deadline => return None,
            Err(_) => Err(UNANSWERED.into()),
        };
        self.done = true;
        self.busy.store(false, std::sync::atomic::Ordering::Release);
        Some(result)
    }
    /// Wait for the reply, no longer than [`Self::poll`] would: only ever off the frame
    /// thread, on a worker that has nothing else to do until the session answered.
    #[cfg(not(target_family = "wasm"))]
    pub fn wait(mut self) -> Result<BrowserServiceSessionReply, String> {
        let left = self.deadline.saturating_duration_since(crate::time::now());
        let result = self
            .received
            .recv_timeout(left)
            .unwrap_or_else(|_| Err(UNANSWERED.into()));
        self.done = true;
        self.busy.store(false, std::sync::atomic::Ordering::Release);
        result
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.done {
            self.busy.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// An app engine's asks for the secrets of the workspace's accounts it signs in to, on its
/// window's own route: how any app (Mail reading and sending mail, Files reaching a `WebDAV` or
/// Nextcloud server) gets an account's password or grant without its sandbox reaching a vault,
/// a key store or a file.
///
/// Each ask names the vault entry the account names as its sign-in: the account's
/// `passwordItem` in the published accounts record (`accounts::email::password_item`), a
/// `vault.item:<id>` reference. The session answers it from this device's copy of the
/// workspace's vault (`BrowserServiceSessionCommand::ReleaseAccountSecret`): only where the
/// person may use Passwords in that workspace, only for an enabled account that names that
/// entry, only for the servers that account names, and with the use written into the entry
/// first. A released secret stays in the app's memory.
///
/// One ask is in flight at a time on the window's route; the rest wait their turn. Ask while
/// the window is up, and take what was answered on each pass ([`Self::poll`]).
pub struct AccountSecrets {
    client: Client,
    waiting: std::collections::VecDeque<String>,
    asking: Option<(String, Pending)>,
}

/// What the session answered for one entry.
pub enum AccountSecret {
    /// The secret of the account that names the entry `item`.
    Released {
        item: String,
        secret: zeroize::Zeroizing<String>,
    },
    /// Not released, with the session's words for why: shown to the person as they are.
    Refused { item: String, error: String },
}

impl AccountSecrets {
    /// The asks of the window whose route `client` is ([`Client::capture_kept`], or the
    /// `session()` of an app's view context).
    #[must_use]
    pub fn through(client: Client) -> Self {
        Self {
            client,
            waiting: std::collections::VecDeque::new(),
            asking: None,
        }
    }

    /// Ask for the secret of the account that names the vault entry `item`, unless it is asked
    /// for already.
    pub fn ask(&mut self, item: &str) {
        if !self.awaits(item) {
            self.waiting.push_back(item.to_owned());
        }
    }

    /// Whether the secret of `item` is asked for and not answered yet.
    #[must_use]
    pub fn awaits(&self, item: &str) -> bool {
        self.asking.as_ref().is_some_and(|(asked, _)| asked == item)
            || self.waiting.iter().any(|waiting| waiting == item)
    }

    /// What the session answered since the last pass, sending the next ask as each is
    /// answered. `wake` wakes the window when an answer arrives.
    pub fn poll(&mut self, wake: Option<&crate::WakeHandle>) -> Vec<AccountSecret> {
        let mut answered = Vec::new();
        loop {
            if let Some((item, pending)) = &mut self.asking {
                let Some(reply) = pending.poll() else {
                    break;
                };
                let item = std::mem::take(item);
                self.asking = None;
                answered.push(match reply {
                    Ok(BrowserServiceSessionReply::AccountSecret {
                        item: released,
                        secret,
                    }) if released == item => AccountSecret::Released { item, secret },
                    Ok(_) => AccountSecret::Refused {
                        item,
                        error: "The session answered something else".into(),
                    },
                    Err(error) => AccountSecret::Refused { item, error },
                });
            }
            let Some(item) = self.waiting.pop_front() else {
                break;
            };
            match self.client.request(
                BrowserServiceSessionCommand::ReleaseAccountSecret { item: item.clone() },
                wake.cloned(),
            ) {
                Ok(pending) => self.asking = Some((item, pending)),
                Err(error) => answered.push(AccountSecret::Refused { item, error }),
            }
        }
        answered
    }
}
