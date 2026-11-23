//! Tickets a page's host serves for it: a tab has no iroh endpoint of its own, so the host a page
//! is on mints the tickets its agent works with ([`tilo_device_identity::login`]).
//!
//! Two directions, told apart by the ticket alone ([`Direction`]):
//!
//! - **Setting up.** The tab holds the person's root and asks its host for a [`Direction::Login`]
//!   ticket naming the workspace its agent's computer enters. The host serves it the way a daemon
//!   serves one it printed ([`tilo_peer_state::new_device::ServedTicket`]): a computer with no
//!   certificate dials the host's endpoint, calls `tilo.device`'s `devices#certify` with the
//!   ticket's secret and its own key, and the host answers once the page approved it. The page
//!   signs the certificate the host built with its root and signs the approval with its own device
//!   key; the host checks both with
//!   [`check_approval`](tilo_device_identity::login::check_approval) before the computer is
//!   answered. The computer then reports each step it took ([`Step`]), which the page shows.
//! - **Signing in.** The tab holds nothing of the person yet and asks for a
//!   [`Direction::SignIn`] ticket naming its own device key. A computer of the person's that holds
//!   their root dials the host and calls `devices#pending` for the tab, then `devices#approve`
//!   with a certificate the person's root signs for the tab's key, as it signs one for any device
//!   of theirs. The host answers the certification request the tab pairs with. No device issues a
//!   browser's certificate: the root does.
//!
//! Nothing is kept once a ticket is answered or runs out, and a ticket names no workspace of its
//! own: the page says which workspace a computer enters, and the host fills in where that
//! workspace is reached and through which device.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    APPROVE, Approval, Approved, Approver, Asked, Declined, Direction, Entry, PENDING, PROVIDER,
    REPORTED, Reached, Report, ServerAddress, SigningIn, Step, TICKET_LIFETIME_NS, Tab, Ticket,
    tab_capability,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::new_device::ServedTicket;
use tilo_server_protocol::web::ServerInfo;
use tilo_user_identity::DeviceCert;

/// Where a page asks its host for a ticket, and where it reads and answers what the computer
/// asks in turn. Below the API prefix, beside the routes every page reaches its host at.
pub(super) const MINT_ROUTE: &str = "/ticket";
pub(super) const ASKED_ROUTE: &str = "/ticket/asked";
pub(super) const ANSWER_ROUTE: &str = "/ticket/answer";

/// Tickets one host keeps at once: a page makes one per setup it shows, and one left behind by a
/// tab that closed runs out in ten minutes.
const MAX_TICKETS: usize = 64;
/// How long an answered ticket stays readable by the page that showed it, so its last poll sees
/// the certificate before the ticket goes.
const KEEP_ANSWERED: Duration = Duration::from_secs(60);
/// How often a sign-in waiting for the tab's pairing request looks for it.
const LOOK_AGAIN: Duration = Duration::from_millis(200);
/// How long a sign-in waits for the tab to pair before it tells the computer to try again: well
/// inside the deadline one device action has to answer.
const TAB_WAIT: Duration = Duration::from_secs(20);

/// A page asking its host for the ticket its agent works with.
#[derive(Deserialize)]
pub(super) struct Mint {
    /// What the ticket is for; a setup's when not said.
    #[serde(default)]
    pub direction: Direction,
    /// Setting up: the person whose root approves the computer, hex.
    #[serde(default)]
    pub user: Option<String>,
    /// Setting up: the workspace the computer enters, as its host names it.
    #[serde(default)]
    pub workspace: Option<String>,
    /// That workspace's name, as the page shows it.
    #[serde(default)]
    pub name: Option<String>,
    /// The page's own origin, which the login keeps as its site.
    #[serde(default)]
    pub site: Option<String>,
}

/// The ticket a page shows, and the name it asks the host under.
#[derive(Serialize)]
pub(super) struct Minted {
    pub id: String,
    pub ticket: String,
}

/// A page asking whether its computer has shown up, and what it asked or did.
#[derive(Deserialize)]
pub(super) struct Asking {
    pub id: String,
}

/// What the page is answered: the computer's question, where its call stands, and the steps a
/// computer reported.
#[derive(Serialize, Default)]
pub(super) struct AskingAnswer {
    /// The computer's request, for the page to sign, once it has shown up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<Asked>,
    /// The computer's certificate, once the host answered it: the setup is over.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certified: Option<String>,
    /// Why a computer was not certified, where its call ended that way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declined: Option<String>,
    /// The agent that runs on the computer, once it named itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The latest failure a computer reported, in one sentence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The steps a computer reported, in order, each at most once.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Signing in: whom the approving device is a device of, and the invite the tab pairs with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed: Option<Claimed>,
}

/// Whom a sign-in ticket was approved for, and what the tab pairs with.
#[derive(Clone, Serialize)]
pub(super) struct Claimed {
    /// The person's root, hex.
    pub user: String,
    /// The invite the tab's certification request names, hex (16 bytes).
    pub invite: String,
    /// The approving device's label (`Claude Code on build-box`).
    pub by: String,
}

/// The page's yes or no to the computer's request.
#[derive(Deserialize)]
pub(super) struct Answering {
    pub id: String,
    /// The approval the page signed, as [`Approved`] JSON.
    #[serde(default)]
    pub approved: Option<Approved>,
    /// Why the page said no.
    #[serde(default)]
    pub declined: Option<String>,
}

/// Why a ticket route or a ticket's action was refused: the sentence it answers.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Malformed,
    Unknown,
    TooMany,
    NotWaiting,
    NotThisPerson,
    NotSignedIn,
    Spent,
}

impl Refusal {
    pub(super) const fn message(&self) -> &'static str {
        match self {
            Self::Malformed => "not a request this route takes",
            Self::Unknown => "no such ticket, or it ran out; ask the page for a new one",
            Self::TooMany => "too many tickets are waiting here; try again in a minute",
            Self::NotWaiting => "no browser is waiting to be signed in with this ticket",
            Self::NotThisPerson => "this ticket is being signed in for another person",
            Self::NotSignedIn => "this ticket does not sign a browser in",
            Self::Spent => "this browser is signed in already: ask for a new ticket",
        }
    }
}

/// Signing in: the person whose computer asked for the tab, and the certificate their root
/// signed for it once it approved.
struct Signing {
    /// The person, from the approving device's certificate.
    user: [u8; 32],
    /// The approving device's label, once it approved.
    by: Option<String>,
    /// The tab's certificate, once the device gave one.
    cert: Option<DeviceCert>,
}

/// Bind the sign-in a slot waits for to `person`'s computer, or refuse: a ticket one person's
/// computer asked for is never another person's, and one that is signed in is not signed in
/// twice (`Spent`).
fn bind(signing: &mut Option<Signing>, person: [u8; 32]) -> Result<(), Refusal> {
    match signing {
        Some(open) if open.user != person => Err(Refusal::NotThisPerson),
        Some(open) if open.cert.is_some() => Err(Refusal::Spent),
        Some(_) => Ok(()),
        None => {
            *signing = Some(Signing {
                user: person,
                by: None,
                cert: None,
            });
            Ok(())
        }
    }
}

/// Whether a ticket that can be used until `expires_at_ns` has run out at `now_ns`.
const fn expired_at(expires_at_ns: u64, now_ns: u64) -> bool {
    now_ns >= expires_at_ns
}

/// Whether `slot`'s ticket can no longer be used.
fn expired(slot: &Slot) -> bool {
    expired_at(slot.expires_at_ns, tilo_clock::unix_now_ns())
}

/// One ticket a page showed, and what the computer did with it.
struct Slot {
    /// The ticket's secret, hex: what the computer's call is routed by.
    secret: String,
    /// What the ticket is for.
    direction: Direction,
    /// Setting up: the ticket as the host serves it, the certify action once a computer asks.
    served: Mutex<Option<ServedTicket>>,
    /// Setting up: the computer's request, once it showed up.
    asked: Mutex<Option<Asked>>,
    /// Setting up: the page's answer, once it gave one.
    decision: tokio::sync::watch::Sender<Option<Result<Approved, Declined>>>,
    /// Setting up: the certificate the host answered the computer with.
    certified: Mutex<Option<DeviceCert>>,
    /// Why a computer's call ended without a certificate.
    declined: Mutex<Option<String>>,
    /// The latest failure a computer reported, for the page to say.
    detail: Mutex<Option<String>>,
    /// The steps the computer reported.
    steps: Mutex<Vec<Step>>,
    /// The agent the computer named.
    agent: Mutex<Option<String>>,
    /// When the ticket can no longer be used, in nanoseconds.
    expires_at_ns: u64,
    /// Signing in: the invite the tab's pairing names.
    invite: Option<[u8; 16]>,
    /// Signing in: the person whose computer asked for the tab, and what it saw.
    signing: Mutex<Option<Signing>>,
}

impl Slot {
    fn decided(&self) -> Option<Result<Approved, Declined>> {
        self.decision.borrow().clone()
    }

    fn served(&self) -> Option<ServedTicket> {
        self.served.lock_recover().clone()
    }

    /// Record `step`, naming `agent`; a step is listed once.
    fn report(&self, step: Step, agent: Option<&str>, detail: Option<&str>) {
        if step == Step::Failed {
            *self.detail.lock_recover() = detail.map(str::to_owned);
        }
        if let Some(agent) = agent.map(str::trim).filter(|agent| !agent.is_empty()) {
            *self.agent.lock_recover() = Some(agent.chars().take(64).collect());
        }
        let mut steps = self.steps.lock_recover();
        if !steps.contains(&step) {
            steps.push(step);
        }
    }
}

/// The invocations a certified device took on this host, by id, with the device that took each.
type Calls = Arc<Mutex<HashMap<InvocationId, ([u8; 32], InvocationState)>>>;

/// The tickets a host serves for its pages, by the id each page keeps.
#[derive(Clone)]
pub(super) struct Tickets {
    entries: Arc<Mutex<HashMap<[u8; 32], Arc<Slot>>>>,
    /// The host's certification, for the revocations it knows, the record each certified
    /// device joins, and the request a sign-in answers.
    certification: crate::certification::CertificationProtocol,
    /// Each action a certified device called, and where it is.
    calls: Calls,
    runtime: tokio::runtime::Handle,
}

impl Tickets {
    /// The tickets of a host whose person's devices `certification` records.
    ///
    /// # Panics
    /// Outside a runtime context, where a sign-in could not be answered.
    pub(super) fn new(certification: crate::certification::CertificationProtocol) -> Self {
        Self {
            entries: Arc::default(),
            certification,
            calls: Arc::default(),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Make a ticket for `mint`, and answer the page its text and the id it asks by.
    ///
    /// # Errors
    /// The request is malformed, or too many tickets are waiting here already.
    pub(super) fn mint(&self, mint: &Mint, info: &ServerInfo) -> Result<Minted, Refusal> {
        let mut entries = self.entries.lock_recover();
        if entries.len() >= MAX_TICKETS {
            return Err(Refusal::TooMany);
        }
        let address = info.address.as_ref().ok_or(Refusal::Malformed)?;
        let reached = reached_of(address);
        let (ticket, slot) = match mint.direction {
            Direction::Login => {
                let user: [u8; 32] = mint
                    .user
                    .as_deref()
                    .and_then(|user| hex::decode(user).ok())
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or(Refusal::Malformed)?;
                let workspace = mint
                    .workspace
                    .clone()
                    .filter(|workspace| !workspace.is_empty())
                    .ok_or(Refusal::Malformed)?;
                let host_origin = info.origin.clone().unwrap_or_default();
                let entry = Entry {
                    workspace,
                    workspace_name: mint.name.clone(),
                    server_cert: info.server_cert.clone(),
                    server: reached.clone(),
                    site: mint.site.clone().unwrap_or_else(|| host_origin.clone()),
                    host_origin,
                };
                let ticket = Ticket::in_direction(Direction::Login, reached);
                let slot = Arc::new(Slot {
                    secret: ticket.secret_hex(),
                    direction: Direction::Login,
                    served: Mutex::new(None),
                    asked: Mutex::default(),
                    decision: tokio::sync::watch::Sender::new(None),
                    certified: Mutex::default(),
                    declined: Mutex::default(),
                    detail: Mutex::default(),
                    steps: Mutex::default(),
                    agent: Mutex::default(),
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(TICKET_LIFETIME_NS),
                    invite: None,
                    signing: Mutex::default(),
                });
                let served = ServedTicket::new(
                    ticket.clone(),
                    user,
                    entry,
                    tilo_user_identity::DeviceCapabilities::member(),
                    Arc::new(PageApprover(Arc::clone(&slot))),
                    self.certification.revocations(),
                    TICKET_LIFETIME_NS,
                );
                *slot.served.lock_recover() = Some(served.clone());
                self.watch_certified(Arc::clone(&slot), &served);
                (ticket, slot)
            }
            Direction::SignIn => {
                let mut invite = [0u8; 16];
                getrandom::fill(&mut invite).map_err(|_| Refusal::Malformed)?;
                let ticket = Ticket::in_direction(Direction::SignIn, reached);
                let slot = Arc::new(Slot {
                    secret: ticket.secret_hex(),
                    direction: Direction::SignIn,
                    served: Mutex::default(),
                    asked: Mutex::default(),
                    decision: tokio::sync::watch::Sender::new(None),
                    certified: Mutex::default(),
                    declined: Mutex::default(),
                    detail: Mutex::default(),
                    steps: Mutex::default(),
                    agent: Mutex::default(),
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(TICKET_LIFETIME_NS),
                    invite: Some(invite),
                    signing: Mutex::default(),
                });
                (ticket, slot)
            }
        };
        let mut id = [0u8; 32];
        getrandom::fill(&mut id).map_err(|_| Refusal::Malformed)?;
        entries.insert(id, Arc::clone(&slot));
        drop(entries);
        self.expire(id);
        Ok(Minted {
            id: hex::encode(id),
            ticket: ticket.encode(),
        })
    }

    /// Record the certificate `served` gives, once it gives one, and keep the ticket readable
    /// for a while so the page that showed it sees the setup is over.
    fn watch_certified(&self, slot: Arc<Slot>, served: &ServedTicket) {
        let certification = self.certification.clone();
        let mut certified = served.certified();
        self.runtime.spawn(async move {
            loop {
                let given = certified.borrow().clone();
                if let Some(cert) = given {
                    certification
                        .keep_issued_device(&cert, &cert.device_label)
                        .await;
                    *slot.certified.lock_recover() = Some(cert);
                    return;
                }
                if certified.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// Take the ticket `id` away once it can no longer be used: a page that never answered one,
    /// or a computer that never came.
    fn expire(&self, id: [u8; 32]) {
        let tickets = self.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(
                Duration::from_nanos(TICKET_LIFETIME_NS).saturating_add(KEEP_ANSWERED),
            )
            .await;
            tickets.entries.lock_recover().remove(&id);
        });
    }

    fn get(&self, id: &str) -> Result<Arc<Slot>, Refusal> {
        let id: [u8; 32] = hex::decode(id)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Refusal::Unknown)?;
        self.entries
            .lock_recover()
            .get(&id)
            .cloned()
            .ok_or(Refusal::Unknown)
    }

    /// The ticket whose secret is `secret`, hex.
    fn by_secret(&self, secret: &str) -> Option<Arc<Slot>> {
        self.entries
            .lock_recover()
            .values()
            .find(|slot| slot.secret == secret)
            .cloned()
    }

    /// What the computer did with ticket `id`, and where its call stands.
    ///
    /// # Errors
    /// No such ticket is live here.
    pub(super) fn asking(&self, id: &str) -> Result<AskingAnswer, Refusal> {
        let slot = self.get(id)?;
        let asked = slot.asked.lock_recover().clone();
        let signing = slot.signing.lock_recover();
        let claimed = signing.as_ref().map(|signing| Claimed {
            user: hex::encode(signing.user),
            invite: slot.invite.map(hex::encode).unwrap_or_default(),
            by: signing
                .by
                .clone()
                .unwrap_or_else(|| "Your device".to_owned()),
        });
        drop(signing);
        Ok(AskingAnswer {
            agent: slot.agent.lock_recover().clone(),
            asked,
            certified: slot
                .certified
                .lock_recover()
                .as_ref()
                .and_then(|cert| serde_json::to_string(cert).ok()),
            declined: slot.declined.lock_recover().clone(),
            detail: slot.detail.lock_recover().clone(),
            steps: slot.steps.lock_recover().clone(),
            claimed,
        })
    }

    /// Take the page's answer to the computer's question.
    ///
    /// # Errors
    /// No such ticket is live here, or it was answered already.
    pub(super) fn answer(&self, answer: &Answering) -> Result<(), Refusal> {
        let slot = self.get(&answer.id)?;
        let decision = match (&answer.approved, &answer.declined) {
            (Some(approved), None) => Ok(approved.clone()),
            (None, Some(reason)) => Err(Declined(reason.clone())),
            (None, None) => Err(Declined("the page said no".to_owned())),
            (Some(_), Some(_)) => return Err(Refusal::Malformed),
        };
        if let Err(declined) = &decision {
            *slot.declined.lock_recover() = Some(declined.0.clone());
        }
        slot.decision.send_replace(Some(decision));
        Ok(())
    }

    /// Whether a tab's request naming `user` and `invite` is one a sign-in ticket here waits to
    /// answer: what queues it for the invitation-only path and for
    /// [`CertificationProtocol::issue_named`](crate::certification::CertificationProtocol).
    pub(super) fn expects(&self, user: &[u8; 32], invite: &[u8; 16]) -> bool {
        self.entries.lock_recover().values().any(|slot| {
            slot.invite == Some(*invite) && {
                let signing = slot.signing.lock_recover();
                signing
                    .as_ref()
                    .is_some_and(|signing| signing.user == *user)
            }
        })
    }

    /// What a certified device's action said, and where it stands once taken.
    fn settle(
        &self,
        invocation: InvocationId,
        device: [u8; 32],
        update: InvocationUpdate,
    ) -> InvocationReceipt {
        let state = InvocationState::new(invocation, update);
        let receipt = state.receipt();
        self.calls
            .lock_recover()
            .insert(invocation, (device, state));
        receipt
    }
}

/// Where the host is dialled, from the address it names to pages.
fn reached_of(address: &ServerAddress) -> Reached {
    let addr = &address.endpoint_addr;
    Reached {
        endpoint: *addr.id.as_bytes(),
        relay: address
            .relay_url
            .clone()
            .or_else(|| addr.relay_urls().next().map(ToString::to_string)),
        direct: addr.ip_addrs().copied().collect(),
    }
}

/// The host's side of a page's approval: it hands the computer's question to the page through
/// [`Tickets::asking`] and waits for [`Tickets::answer`].
struct PageApprover(Arc<Slot>);

impl Approver for PageApprover {
    fn approve(&self, asked: Asked) -> Approval {
        let slot = Arc::clone(&self.0);
        Box::pin(async move {
            *slot.asked.lock_recover() = Some(asked);
            let mut decision = slot.decision.subscribe();
            let waited = tokio::time::timeout(Duration::from_nanos(TICKET_LIFETIME_NS), async {
                loop {
                    if let Some(answer) = slot.decided() {
                        return answer;
                    }
                    if decision.changed().await.is_err() {
                        return Err(Declined("the page went away".to_owned()));
                    }
                }
            })
            .await;
            waited.unwrap_or_else(|_| Err(Declined("the page did not answer in time".to_owned())))
        })
    }
}

/// The ticket's secret as the computer's call carries it, hex.
fn secret_of(request: &InvokeRequest) -> Option<String> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return None;
    };
    argument.get("secret")?.as_str().map(str::to_owned)
}

/// The one provider the host admits a computer with no certificate to
/// ([`tilo_peer_state::actions::ActionService::admit_uncertified`]): it routes each call to the
/// ticket whose secret it carries, so several pages' setups are served at once.
pub(super) struct Router {
    tickets: Tickets,
}

impl Router {
    pub(super) fn new(tickets: Tickets) -> Self {
        Self { tickets }
    }

    /// The ticket `request` names by its secret.
    fn of(&self, request: &InvokeRequest) -> Option<ServedTicket> {
        let secret = secret_of(request)?;
        self.tickets.by_secret(&secret)?.served()
    }

    fn slots(&self) -> Vec<Arc<Slot>> {
        self.tickets
            .entries
            .lock_recover()
            .values()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tickets")
    }
}

impl ActionProvider for Router {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "a computer with no certificate calls only the certify action".into(),
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(served) = self.of(&request) else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "this ticket was used already: ask for a new one".into(),
            }));
        };
        served.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.slots()
            .into_iter()
            .find_map(|slot| slot.served()?.observe(caller, id))
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.slots()
            .into_iter()
            .find_map(|slot| slot.served()?.next_change(caller, id))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        // A call waits on the page's answer, which ends it; nothing else can.
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "the person is asked already; their answer ends it".into(),
        }))
    }
}

/// `tilo.device`'s actions a certified device of the person calls on the host that served a
/// ticket: the tab a sign-in waits for, its approval, and the steps a computer reports as it
/// logs in. Beside the workspaces' provider on the device action service
/// ([`tilo_peer_state::actions::ActionService::add_provider`]).
pub(super) struct Certified {
    tickets: Tickets,
}

impl Certified {
    pub(super) fn new(tickets: Tickets) -> Self {
        Self { tickets }
    }

    /// Refuse `reason` as the caller reads it.
    fn refused(reason: &str) -> ActionCallError {
        ActionCallError::Refused {
            reason: reason.to_owned(),
        }
    }

    /// The tab a sign-in ticket waits for, to `caller`: binding the ticket to their person, so
    /// the page learns whom to pair with, then waiting for the tab's certification request, whose
    /// device and label the approval prompt names.
    fn pending(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        argument: &serde_json::Map<String, serde_json::Value>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(person) = caller.user_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let Some(secret) = argument
            .get("secret")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return Pending::ready(Err(Self::refused(Refusal::NotWaiting.message())));
        };
        let Some(slot) = self
            .tickets
            .by_secret(&secret)
            .filter(|slot| slot.direction == Direction::SignIn)
        else {
            return Pending::ready(Err(Self::refused(Refusal::NotSignedIn.message())));
        };
        let Some(invite) = slot.invite else {
            return Pending::ready(Err(Self::refused(Refusal::NotWaiting.message())));
        };
        if expired(&slot) {
            return Pending::ready(Err(Self::refused(Refusal::Unknown.message())));
        }
        {
            let mut signing = slot.signing.lock_recover();
            if let Err(refusal) = bind(&mut signing, person) {
                return Pending::ready(Err(Self::refused(refusal.message())));
            }
        }
        let certification = self.tickets.certification.clone();
        let calls = Arc::clone(&self.tickets.calls);
        let runtime = self.tickets.runtime.clone();
        let Some(device) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let (completer, pending) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let started = std::time::Instant::now();
            let asked = loop {
                let now = tilo_clock::unix_now_ns();
                if let Some(request) = certification.waiting_for(&person, &invite, now) {
                    break Some(request);
                }
                if started.elapsed() >= TAB_WAIT {
                    break None;
                }
                tokio::time::sleep(LOOK_AGAIN).await;
            };
            let answer = match asked {
                Some(request) => {
                    let tab = Tab {
                        device_public_key: hex::encode(request.device_public_key),
                        label: request.device_label,
                    };
                    let label = tab.fitted_label();
                    Ok(serde_json::json!(SigningIn {
                        device: tab.device_public_key,
                        label,
                        capability: tab_capability().to_owned(),
                    }))
                }
                None => Err(Self::refused(
                    "the browser has not asked to be signed in yet: try again in a moment",
                )),
            };
            let (receipt, state) = receipt_of(invocation, answer);
            calls.lock_recover().insert(invocation, (device, state));
            completer.complete(Ok(receipt));
        });
        pending
    }

    /// Sign the tab's certificate with the approving device's key, and have the host answer the
    /// request the tab paired with.
    fn approve(
        &self,
        caller: &Caller,
        argument: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, ActionCallError> {
        let person = caller
            .user_public_key()
            .copied()
            .ok_or(ActionCallError::NotPermitted)?;
        let secret = argument
            .get("secret")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Self::refused(Refusal::NotWaiting.message()))?;
        let cert: Option<DeviceCert> = argument
            .get("cert")
            .and_then(|cert| serde_json::from_value(cert.clone()).ok());
        let slot = self
            .tickets
            .by_secret(secret)
            .filter(|slot| slot.direction == Direction::SignIn)
            .ok_or_else(|| Self::refused(Refusal::NotSignedIn.message()))?;
        if expired(&slot) {
            return Err(Self::refused(Refusal::Unknown.message()));
        }
        let Some(invite) = slot.invite else {
            return Err(Self::refused(Refusal::NotWaiting.message()));
        };
        {
            let mut signing = slot.signing.lock_recover();
            bind(&mut signing, person).map_err(|refusal| Self::refused(refusal.message()))?;
            signing.as_mut().ok_or_else(|| {
                Self::refused("run `tilo device add` with a ticket whose browser is waiting")
            })?;
        }
        // A computer without the person's root hands the signing to one that holds it: the
        // tab's request is already queued for the person's approving devices, which answer it
        // with their root.
        let Some(cert) = cert else {
            return Ok(serde_json::json!({ "waiting": true }));
        };
        // The certificate is the person's root's over exactly the tab that asked, as it signs
        // one for any device of theirs: nothing a device issued, for a browser and nothing more.
        let now = tilo_clock::unix_now_ns();
        if cert.user_public_key != person || cert.issuer.is_some() {
            return Err(Self::refused(
                "a sign-in is the tab's own certificate, signed by the person's root",
            ));
        }
        let waiting = self
            .tickets
            .certification
            .waiting_for(&person, &invite, now)
            .ok_or_else(|| Self::refused("the browser has not asked to be signed in yet"))?;
        if cert.device_public_key != waiting.device_public_key
            || cert.device_label != waiting.device_label
        {
            return Err(Self::refused(
                "the certificate does not name the browser that asked",
            ));
        }
        tilo_user_identity::verify_device_cert_at(&cert, now).map_err(|_| {
            Self::refused("the certificate the person's root signed for the tab does not hold")
        })?;
        let by = argument
            .get("label")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map_or_else(
                || "Your device".to_owned(),
                |label| label.chars().take(64).collect(),
            );
        self.tickets
            .certification
            .issue_named(&person, &invite, cert.clone(), |_| true, now)
            .map_err(|reason| Self::refused(&reason))?;
        let mut signing = slot.signing.lock_recover();
        if let Some(open) = signing.as_mut() {
            open.cert = Some(cert);
            open.by = Some(by);
        }
        drop(signing);
        Ok(serde_json::json!({}))
    }

    /// Keep one step of what the computer did, from the device the ticket certified.
    fn reported(
        &self,
        caller: &Caller,
        argument: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, ActionCallError> {
        let device = caller
            .device_public_key()
            .copied()
            .ok_or(ActionCallError::NotPermitted)?;
        let report: Report = serde_json::from_value(serde_json::Value::Object(argument.clone()))
            .map_err(|_| Self::refused("a report names its ticket and step"))?;
        let ticket = Ticket::parse(&report.ticket).map_err(|_| {
            Self::refused("the report names no Tilo ticket; ask the page for a new one")
        })?;
        let slot = self
            .tickets
            .by_secret(&ticket.secret_hex())
            .filter(|slot| slot.direction == Direction::Login)
            .ok_or_else(|| Self::refused(Refusal::Unknown.message()))?;
        // Only the computer this ticket certified reports against it.
        let certified = slot.certified.lock_recover().clone();
        if certified.is_none_or(|cert| cert.device_public_key != device) {
            return Err(ActionCallError::NotPermitted);
        }
        slot.report(
            report.step,
            report.agent.as_deref(),
            report.detail.as_deref(),
        );
        Ok(serde_json::json!({}))
    }
}

impl std::fmt::Debug for Certified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tickets")
    }
}

impl ActionProvider for Certified {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "the device actions here are called by a device of the person's own".into(),
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some((from, state)) = self.tickets.calls.lock_recover().get(&invocation) {
            let device = caller.device_public_key().copied();
            return if device.is_some_and(|device| device == *from) {
                Pending::ready(Ok(state.receipt()))
            } else {
                Pending::ready(Err(ActionCallError::NotPermitted))
            };
        }
        let Some(device) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        if request.target.provider != PROVIDER {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(Self::refused(
                "a device action asks with its arguments",
            )));
        };
        if request.target.address.action == PENDING {
            return self.pending(caller, invocation, argument);
        }
        let answer = match request.target.address.action.as_str() {
            APPROVE => self.approve(caller, argument),
            REPORTED => self.reported(caller, argument),
            _ => return Pending::ready(Err(ActionCallError::UnknownAction)),
        };
        let update = match answer {
            Ok(value) => InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            Err(error) => InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "device-action".to_owned(),
                    message: action_message(&error),
                    details: None,
                    retryable: false,
                },
            },
        };
        Pending::ready(Ok(self.tickets.settle(invocation, device, update)))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let device = caller.device_public_key()?;
        self.tickets
            .calls
            .lock_recover()
            .get(id)
            .filter(|(from, _)| from == device)
            .map(|(_, state)| state.clone())
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a device action ends as it is called".into(),
        }))
    }
}

/// The receipt of a device action's one answer, and the state it leaves for a follower.
fn receipt_of(
    invocation: InvocationId,
    answer: Result<serde_json::Value, ActionCallError>,
) -> (InvocationReceipt, InvocationState) {
    let update = match answer {
        Ok(value) => InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
        Err(error) => InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "device-action".to_owned(),
                message: action_message(&error),
                details: None,
                retryable: false,
            },
        },
    };
    let state = InvocationState::new(invocation, update);
    let receipt = state.receipt();
    (receipt, state)
}

/// What a failed device action says, as its caller reads it.
fn action_message(error: &ActionCallError) -> String {
    match error {
        ActionCallError::Refused { reason }
        | ActionCallError::Unavailable { reason }
        | ActionCallError::InvalidRequest { reason }
        | ActionCallError::NotCancellable { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

/// The certification requests a page's sign-in tickets expect: the tab pairs with the invite its
/// ticket named, and the host answers the request with the certificate the person's device
/// signed ([`Certified::approve`]).
pub(super) struct Expecting(pub(super) Tickets);

impl crate::certification::TabRequests for Expecting {
    fn expects(&self, request: &tilo_user_identity::CertificationRequest, _now_ns: u64) -> bool {
        self.0.expects(&request.user_public_key, &request.invite)
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, Signing, bind, expired_at};

    fn signing(cert: Option<tilo_user_identity::DeviceCert>) -> Signing {
        Signing {
            user: [7; 32],
            by: None,
            cert,
        }
    }

    tilo_test_env::scenario! {
        fn a_sign_in_ticket_is_one_persons_and_is_spent_once;

        phase a_ticket_one_persons_computer_asked_for_is_no_other_persons;
        {
            let mut open = Some(signing(None));
            assert_eq!(bind(&mut open, [7; 32]), Ok(()), "the person who asked, again");
            assert_eq!(bind(&mut open, [8; 32]), Err(Refusal::NotThisPerson));
            assert_eq!(
                open.as_ref().map(|open| open.user),
                Some([7; 32]),
                "the person who asked first keeps the ticket",
            );
        }

        phase a_ticket_that_signed_a_browser_in_words_no_second_sign_in;
        {
            let cert = tilo_user_identity::DeviceCert {
                user_public_key: [7; 32],
                device_public_key: [9; 32],
                device_label: "A browser".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: Some(2),
                signature: [0; 64],
                issuer: None,
            };
            let mut signed = Some(signing(Some(cert)));
            assert_eq!(bind(&mut signed, [7; 32]), Err(Refusal::Spent));
            let mut unasked: Option<Signing> = None;
            assert_eq!(bind(&mut unasked, [7; 32]), Ok(()), "the first computer to ask");
        }
    }

    #[test]
    fn a_ticket_runs_out_at_its_life() {
        assert!(!expired_at(100, 1));
        assert!(!expired_at(100, 99));
        assert!(expired_at(100, 100));
        assert!(expired_at(100, 101));
    }
}
