//! The tickets one device serves, whoever issued them there: the one place a ticket is served,
//! spent, followed, answered and approved ([`Tickets`]). A device has one, its action service's
//! ([`ActionService::tickets`](crate::actions::ActionService::tickets)), which answers the scope a
//! device with no certificate calls ([`SCOPE`]) and `tilo.device`'s actions a certified device of
//! the person calls about a ticket. A daemon's `tilo device add`, a planned machine's ticket, an
//! agent's key and a page's tickets are all served here; what differs is only who issued them.
//!
//! Two directions, told apart by the ticket alone ([`Direction`]):
//!
//! - **Logging in.** A computer, or a tab that opened a pairing link, dials the device that served
//!   a [`Direction::Login`] ticket and calls [`CERTIFY`] with the ticket's secret and its own key
//!   ([`ServedTicket`]). The ticket's approver answers it: the person's root on the device that
//!   issued it, or the page that showed it, which follows the ticket ([`FOLLOW`]) and answers the
//!   question it is handed ([`ANSWER`]).
//! - **Signing in.** A tab, or an agent's key, asks for a [`Direction::SignIn`] ticket naming its
//!   own key: a page's tab asks with the key its call comes from ([`ISSUE`]), so the key the
//!   person approves is the one that asked. A device of the person's that holds their root calls
//!   [`PENDING`] with the ticket's secret, then [`APPROVE`] with the certificate the root signed
//!   for that key ([`Tab::certificate`]); the tab reads it, and the certificate of the device it
//!   enters through, when it follows its ticket. A tab that named its person is listed to that
//!   person's devices without a ticket in hand ([`PENDING`] with no secret), so a page holding
//!   their root approves it where it is.
//!
//! A page asks for its tickets and follows them with these same actions, over the link it reaches
//! its host's actions on, in [`SCOPE`]: a tab that is signing in holds no certificate yet, so the
//! ticket's id, which only the page that asked for it was told, and the key it asked with are what
//! each call stands on.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    ANSWER, APPROVE, Approval, Approved, Approver, Asked, CERTIFY, Declined, Direction, Entry,
    FOLLOW, ISSUE, KeyFor, PENDING, PROVIDER, REPORTED, RESOURCE, Reached, Report, SigningIn, Step,
    TICKET_LIFETIME_NS, Tab, Ticket, tab_capability,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::watch;

use super::{ServedTicket, ServedTickets};

/// Tickets pages keep at once on one device: a page makes one per setup it shows, and one left
/// behind by a tab that closed runs out in ten minutes.
const MAX_PAGE_TICKETS: usize = 64;
/// How long an answered page ticket stays readable by the page that showed it, so its last look
/// sees how it ended before the ticket goes.
const KEEP_ANSWERED_NS: u64 = 60 * 1_000_000_000;
/// How long after one look at the tickets kept elsewhere a computer whose secret nothing here
/// knows makes this device look again, so a guesser cannot make it read them on every call.
const LOOK_AGAIN_NS: u64 = 5_000_000_000;

/// What looks again at the tickets this device issued and keeps elsewhere, serving each one that
/// still waits ([`Tickets::look_again_with`]): a planned machine's, kept with its plan.
pub type LookAgain = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The host of the pages this device serves tickets to ([`Tickets::serve_pages`]): where it is
/// dialled, what a device entering through it pins, and where the devices it adds are kept.
pub trait PageHost: Send + Sync {
    /// Where this host is dialled now, as a ticket names it: `None` while it names no address.
    fn reached(&self) -> Option<Reached>;
    /// The certificate of the device a page's computer or tab enters through: this host's.
    fn server_cert(&self) -> DeviceCert;
    /// The devices this host knows are revoked: an approval by one of them is never handed on.
    fn revocations(&self) -> RevocationList;
    /// Keep `cert`, which the person's root just signed for a device one of this host's page
    /// tickets added, in the person's record, so their devices list it and the workspaces here
    /// admit it.
    fn keep(&self, cert: DeviceCert);
}

/// Why a ticket's action was refused: the sentence it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Malformed,
    Unknown,
    TooMany,
    NoPages,
    NotThisPerson,
    NotSignedIn,
    NotTheTab,
    Spent,
}

impl Refusal {
    /// The sentence the caller is told.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Malformed => "not a request this action takes",
            Self::Unknown => "no such ticket, or it ran out; ask for a new one",
            Self::TooMany => "too many tickets are waiting here; try again in a minute",
            Self::NoPages => "this device serves no pages their tickets",
            Self::NotThisPerson => "this ticket is being signed in for another person",
            Self::NotSignedIn => "this ticket does not sign anything in",
            Self::NotTheTab => "this ticket was asked for by another tab",
            Self::Spent => "this ticket was used already: ask for a new one",
        }
    }
}

/// A key waiting for the person's root to certify it: a tab's, or an agent's.
struct SignIn {
    ticket: Ticket,
    tab: Tab,
    /// Whether a page asked for it: the certificate is kept in the person's record where the
    /// page's host keeps them.
    from_page: bool,
    /// When it can no longer be signed in, in nanoseconds.
    expires_at_ns: u64,
    /// The person it is signed in for: the one the tab named, else the first whose device asked.
    person: Mutex<Option<[u8; 32]>>,
    /// The approving device's label, once it approved.
    by: Mutex<Option<String>>,
    /// The certificate the person's root signed for it, once a device of theirs approved it.
    certified: watch::Sender<Option<DeviceCert>>,
}

impl SignIn {
    fn secret(&self) -> String {
        self.ticket.secret_hex()
    }

    fn spent(&self) -> bool {
        self.certified.borrow().is_some()
    }

    /// Bind it to `person`'s device, or refuse: a ticket one person's device asked for is never
    /// another person's, and one that is signed in is not signed in twice.
    fn bind(&self, person: [u8; 32]) -> Result<(), Refusal> {
        if self.spent() {
            return Err(Refusal::Spent);
        }
        let mut bound = self.person.lock_recover();
        match *bound {
            Some(open) if open != person => Err(Refusal::NotThisPerson),
            Some(_) => Ok(()),
            None => {
                *bound = Some(person);
                drop(bound);
                Ok(())
            }
        }
    }

    /// What the device that holds the person's root is shown of it.
    fn signing_in(&self) -> SigningIn {
        SigningIn {
            device: self.tab.device_public_key.clone(),
            label: self.tab.fitted_label(),
            capability: self
                .tab
                .key_for
                .as_ref()
                .map_or_else(|| tab_capability().to_owned(), KeyFor::capability),
            key_for: self.tab.key_for.clone(),
        }
    }
}

/// A computer or tab logging in with a ticket a page showed, and what it did with it.
struct Setup {
    served: ServedTicket,
    /// The computer's request, once it showed up.
    asked: Mutex<Option<Asked>>,
    /// The page's answer, once it gave one.
    decision: watch::Sender<Option<Result<Approved, Declined>>>,
    /// The certificate the computer was answered with.
    certified: Mutex<Option<DeviceCert>>,
    /// Why a computer's call ended without a certificate.
    declined: Mutex<Option<String>>,
    /// The latest failure a computer reported.
    detail: Mutex<Option<String>>,
    /// The steps the computer reported, each once.
    steps: Mutex<Vec<Step>>,
    /// The agent the computer named.
    agent: Mutex<Option<String>>,
}

impl Setup {
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

/// What a page showed: a ticket a computer or tab logs in with, or one that signs a tab in.
#[derive(Clone)]
enum Shown {
    Setup(Arc<Setup>),
    SignIn(Arc<SignIn>),
}

/// A ticket a page asked for, by the id it follows it with.
struct PageTicket {
    shown: Shown,
    /// When it is let go of: once it ran out, and its last answer was read.
    gone_at_ns: u64,
}

/// The calls answered here at once, by invocation, with the device that made each.
type Calls = Mutex<HashMap<InvocationId, ([u8; 32], InvocationState)>>;

/// The tickets one device serves. A clone is the same tickets.
#[derive(Clone)]
pub struct Tickets(Arc<Inner>);

struct Inner {
    /// Every login ticket served here, each answering the certify call that brings its secret.
    served: ServedTickets,
    /// What looks again at the tickets kept elsewhere when a call brings a secret nothing here
    /// knows, and when it last did.
    look_again: std::sync::OnceLock<LookAgain>,
    looked: Mutex<u64>,
    /// The keys waiting to be signed in, by their ticket's secret.
    sign_ins: Mutex<HashMap<String, Arc<SignIn>>>,
    /// The tickets pages asked for, by the id each page keeps.
    pages: Mutex<HashMap<[u8; 32], PageTicket>>,
    /// The host of the pages this device serves, once it serves any.
    serving_host: std::sync::OnceLock<Arc<dyn PageHost>>,
    calls: Calls,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for Tickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tickets")
            .field("signing_in", &self.0.sign_ins.lock_recover().len())
            .field("pages", &self.0.pages.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl Tickets {
    /// The tickets of a device that answers on `runtime`.
    #[must_use]
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self(Arc::new(Inner {
            served: ServedTickets::default(),
            look_again: std::sync::OnceLock::new(),
            looked: Mutex::default(),
            sign_ins: Mutex::default(),
            pages: Mutex::default(),
            serving_host: std::sync::OnceLock::new(),
            calls: Mutex::default(),
            runtime,
        }))
    }

    /// Serve `ticket` beside the others: a computer that brings its secret is answered by it.
    pub fn serve(&self, ticket: ServedTicket) {
        self.0.served.serve(ticket);
    }

    /// Stop serving `ticket`: another took its place.
    pub fn withdraw(&self, ticket: &ServedTicket) {
        self.0.served.withdraw(ticket);
    }

    /// Look again with `look` at the tickets this device keeps elsewhere when a computer brings a
    /// secret nothing served here knows. Named once; a later call keeps the first.
    pub fn look_again_with(&self, look: LookAgain) {
        let _ = self.0.look_again.set(look);
    }

    /// Note that the tickets kept elsewhere were just looked at, so the next look waits.
    pub fn looked(&self) {
        *self.0.looked.lock_recover() = tilo_clock::unix_now_ns();
    }

    /// Serve the pages of `host` their tickets. Named once; a later call keeps the first.
    pub fn serve_pages(&self, host: Arc<dyn PageHost>) {
        let _ = self.0.serving_host.set(host);
    }

    /// Serve a sign-in ticket at `issuer` for `tab`, a key asked for what [`Tab::key_for`] names
    /// (an agent in its one workspace, or a device of that workspace alone), asking the person's
    /// root to certify it so. It lasts until it is spent, or until a ticket for the same thing
    /// replaces it. The receiver hears the certificate once a device that holds the root approved
    /// it.
    ///
    /// # Errors
    /// `tab` names no key, or nothing it is for in a workspace.
    pub fn serve_key(
        &self,
        tab: Tab,
        issuer: Reached,
    ) -> Result<(Ticket, watch::Receiver<Option<DeviceCert>>), String> {
        tab.key()
            .map_err(|_| "the key asked for is not a device key".to_owned())?;
        let key_for = tab
            .key_for
            .clone()
            .ok_or_else(|| "a ticket for a key names what it is for".to_owned())?;
        key_for
            .capabilities()
            .map_err(|_| "the key's workspace is not a workspace's id".to_owned())?;
        let ticket = new_ticket(Direction::SignIn, issuer)?;
        let (certified, heard) = watch::channel(None);
        let sign_in = Arc::new(SignIn {
            ticket: ticket.clone(),
            tab,
            from_page: false,
            expires_at_ns: u64::MAX,
            person: Mutex::default(),
            by: Mutex::default(),
            certified,
        });
        let mut sign_ins = self.0.sign_ins.lock_recover();
        sign_ins.retain(|_, kept| kept.tab.key_for.as_ref() != Some(&key_for));
        sign_ins.insert(sign_in.secret(), sign_in);
        drop(sign_ins);
        Ok((ticket, heard))
    }

    /// Whether a sign-in ticket whose secret is `secret` (hex) waits here.
    #[must_use]
    pub fn signs_in(&self, secret: &str) -> bool {
        self.0.sign_ins.lock_recover().contains_key(secret)
    }

    fn sign_in(&self, secret: &str) -> Result<Arc<SignIn>, Refusal> {
        let sign_in = self
            .0
            .sign_ins
            .lock_recover()
            .get(secret)
            .cloned()
            .ok_or(Refusal::NotSignedIn)?;
        if sign_in.expires_at_ns <= tilo_clock::unix_now_ns() {
            return Err(Refusal::Unknown);
        }
        Ok(sign_in)
    }

    /// The key the sign-in ticket whose secret is `secret` waits for, to a device of `person`'s
    /// that holds their root: binding the ticket to them.
    ///
    /// # Errors
    /// No such ticket waits here, it is another person's, or it was signed in already.
    pub fn pending(&self, person: [u8; 32], secret: &str) -> Result<SigningIn, Refusal> {
        let sign_in = self.sign_in(secret)?;
        sign_in.bind(person)?;
        Ok(sign_in.signing_in())
    }

    /// The tabs that named `person` and wait for one of their devices that holds their root, with
    /// each ticket's secret: what a page of theirs approves without a ticket in hand.
    #[must_use]
    pub fn waiting_for(&self, person: [u8; 32]) -> Vec<(String, SigningIn)> {
        let now = tilo_clock::unix_now_ns();
        self.0
            .sign_ins
            .lock_recover()
            .values()
            .filter(|sign_in| {
                sign_in.from_page
                    && !sign_in.spent()
                    && sign_in.expires_at_ns > now
                    && *sign_in.person.lock_recover() == Some(person)
            })
            .map(|sign_in| (sign_in.secret(), sign_in.signing_in()))
            .collect()
    }

    /// Sign in the key the ticket whose secret is `secret` waits for, with `cert`, the
    /// certificate `person`'s root signed for exactly that key ([`Tab::certificate`]), approved by
    /// their device named `by`.
    ///
    /// # Errors
    /// No such ticket waits here, it is another person's or was signed in already, or `cert` is
    /// not the root's over exactly the key that asked, as it asked.
    pub fn approve(
        &self,
        person: [u8; 32],
        secret: &str,
        cert: DeviceCert,
        by: Option<&str>,
    ) -> Result<Value, String> {
        let sign_in = self
            .sign_in(secret)
            .map_err(|why| why.message().to_owned())?;
        sign_in
            .bind(person)
            .map_err(|why| why.message().to_owned())?;
        check_signed_in(&sign_in.tab, person, &cert)?;
        if let Some(host) = self.0.serving_host.get()
            && host
                .revocations()
                .is_revoked(&cert.user_public_key, &cert.device_public_key)
        {
            return Err("this key was revoked: it is signed in no more".to_owned());
        }
        *sign_in.by.lock_recover() =
            Some(by.map(str::trim).filter(|by| !by.is_empty()).map_or_else(
                || "Your device".to_owned(),
                |by| by.chars().take(64).collect(),
            ));
        let answer = json!({
            "label": cert.device_label,
            "device": hex::encode(cert.device_public_key),
        });
        if sign_in.from_page
            && let Some(host) = self.0.serving_host.get()
        {
            host.keep(cert.clone());
        }
        sign_in.certified.send_replace(Some(cert));
        // A key no page follows is done with: its asker heard the certificate.
        if !sign_in.from_page {
            self.0.sign_ins.lock_recover().remove(secret);
        }
        Ok(answer)
    }

    /// Keep one step of what the computer `device` did with a ticket a page showed: only the
    /// computer that ticket certified reports against it.
    ///
    /// # Errors
    /// The report names no ticket a page showed here, or `device` is not the one it certified.
    pub fn reported(&self, device: [u8; 32], report: &Report) -> Result<(), String> {
        let ticket = Ticket::parse(&report.ticket).map_err(|_| {
            "the report names no Tilo ticket; ask the page for a new one".to_owned()
        })?;
        let secret = ticket.secret_hex();
        let setup = self
            .0
            .pages
            .lock_recover()
            .values()
            .find_map(|page| match &page.shown {
                Shown::Setup(setup) if setup.served.ticket().secret_hex() == secret => {
                    Some(Arc::clone(setup))
                }
                _ => None,
            })
            .ok_or_else(|| Refusal::Unknown.message().to_owned())?;
        let certified = setup.certified.lock_recover().clone();
        if certified.is_none_or(|cert| cert.device_public_key != device) {
            return Err("only the computer this ticket added reports what it did".to_owned());
        }
        setup.report(
            report.step,
            report.agent.as_deref(),
            report.detail.as_deref(),
        );
        Ok(())
    }

    /// The one provider a device answers a caller with no certificate through, in [`SCOPE`]: a
    /// computer's certify call, routed to the ticket whose secret it brings, and a page's calls
    /// for the tickets it shows.
    #[must_use]
    pub fn uncertified(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        Arc::new(Uncertified(self.clone()))
    }

    /// `tilo.device`'s actions a certified device of the person calls about a ticket served here:
    /// [`PENDING`], [`APPROVE`] and [`REPORTED`]. A device whose dispatcher answers `tilo.device`
    /// itself hands these to [`Self::certified_invoke`].
    #[must_use]
    pub fn certified(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        Arc::new(Certified(self.clone()))
    }

    /// Whether `request` is one of the calls about a ticket a certified device makes here, and
    /// not one for the device it reaches to carry out: it names a ticket's secret, or lists the
    /// tabs waiting for the caller's person.
    #[must_use]
    pub fn answers_certified(request: &InvokeRequest) -> bool {
        let address = &request.target.address;
        if request.target.provider != PROVIDER || address.resource != RESOURCE {
            return false;
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return false;
        };
        match address.action.as_str() {
            PENDING => !argument.contains_key("ticket"),
            APPROVE => argument.contains_key("secret"),
            REPORTED => true,
            _ => false,
        }
    }

    /// Answer `request`, one [`Self::answers_certified`] takes, from `caller`.
    #[must_use]
    pub fn certified_invoke(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let Some(device) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        if let Some((from, state)) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(if *from == device {
                Ok(state.receipt())
            } else {
                Err(ActionCallError::NotPermitted)
            });
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(ActionCallError::refused(
                "a device action asks with its arguments",
            )));
        };
        let answer = match request.target.address.action.as_str() {
            PENDING | APPROVE => match caller.user_public_key().copied() {
                Some(person) => self.as_person(&request.target.address.action, person, argument),
                None => Err(ActionCallError::NotPermitted),
            },
            REPORTED => serde_json::from_value::<Report>(Value::Object(argument.clone()))
                .map_err(|_| ActionCallError::refused("a report names its ticket and step"))
                .and_then(|report| {
                    self.reported(device, &report)
                        .map_err(ActionCallError::refused)
                })
                .map(|()| json!({})),
            _ => Err(ActionCallError::UnknownAction),
        };
        Pending::ready(Ok(self.settle(invocation, device, answer)))
    }

    /// Where a call answered here stands, to the device that made it.
    #[must_use]
    pub fn certified_observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let device = caller.device_public_key()?;
        self.0
            .calls
            .lock_recover()
            .get(id)
            .filter(|(from, _)| from == device)
            .map(|(_, state)| state.clone())
    }

    /// [`PENDING`] or [`APPROVE`] from a device of `person`'s.
    fn as_person(
        &self,
        action: &str,
        person: [u8; 32],
        argument: &Map<String, Value>,
    ) -> Result<Value, ActionCallError> {
        let secret = argument.get("secret").and_then(Value::as_str);
        match (action, secret) {
            (PENDING, None) => {
                let waiting: Vec<Value> = self
                    .waiting_for(person)
                    .into_iter()
                    .map(|(secret, signing_in)| {
                        json!({
                            "secret": secret,
                            "device": signing_in.device,
                            "label": signing_in.label,
                            "capability": signing_in.capability,
                        })
                    })
                    .collect();
                Ok(json!({ "waiting": waiting }))
            }
            (PENDING, Some(secret)) => self
                .pending(person, secret)
                .map_err(|why| ActionCallError::refused(why.message()))
                .and_then(|signing_in| {
                    serde_json::to_value(signing_in)
                        .map_err(|_| ActionCallError::refused(Refusal::Malformed.message()))
                }),
            (_, Some(secret)) => {
                let cert: DeviceCert = argument
                    .get("cert")
                    .cloned()
                    .and_then(|cert| serde_json::from_value(cert).ok())
                    .ok_or_else(|| {
                        ActionCallError::refused(
                            "signing a key in takes the certificate the person's root signed for it",
                        )
                    })?;
                let by = argument.get("label").and_then(Value::as_str);
                self.approve(person, secret, cert, by)
                    .map_err(ActionCallError::refused)
            }
            (_, None) => Err(ActionCallError::refused(Refusal::NotSignedIn.message())),
        }
    }

    /// The person whose device `device` is: the person themselves, when the call comes from their
    /// root and names it (`user`), as a page that holds the root calls; else by the certificate a
    /// page shows for its key (`as`), signed by their root, for that key, in force, and revoked
    /// nowhere this device knows.
    fn presented(
        &self,
        device: [u8; 32],
        argument: &Map<String, Value>,
    ) -> Result<[u8; 32], ActionCallError> {
        let Some(cert) = argument.get("as") else {
            return (hex_key(argument.get("user").and_then(Value::as_str)) == Some(device))
                .then_some(device)
                .ok_or_else(|| {
                    ActionCallError::refused(
                        "a page acts as its person with their root, or shows its own certificate",
                    )
                });
        };
        let cert: DeviceCert = serde_json::from_value(cert.clone())
            .map_err(|_| ActionCallError::refused("the certificate shown is not one"))?;
        if cert.device_public_key != device || cert.issuer.is_some() {
            return Err(ActionCallError::refused(
                "the certificate shown is not this key's, from the person's root",
            ));
        }
        tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns())
            .map_err(|_| ActionCallError::refused("the certificate shown does not hold"))?;
        if let Some(host) = self.0.serving_host.get()
            && host
                .revocations()
                .is_revoked(&cert.user_public_key, &cert.device_public_key)
        {
            return Err(ActionCallError::refused("this key was revoked"));
        }
        Ok(cert.user_public_key)
    }

    /// Keep what the call `invocation` from `device` was answered, and its receipt.
    fn settle(
        &self,
        invocation: InvocationId,
        device: [u8; 32],
        answer: Result<Value, ActionCallError>,
    ) -> InvocationReceipt {
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
        self.0
            .calls
            .lock_recover()
            .insert(invocation, (device, state));
        receipt
    }

    /// A page's call for a ticket it shows, from the key `device`: one made, what was done with
    /// it, or the page's answer.
    fn page(&self, device: [u8; 32], action: &str, argument: Value) -> Result<Value, Refusal> {
        match action {
            ISSUE => {
                let mint: Mint =
                    serde_json::from_value(argument).map_err(|_| Refusal::Malformed)?;
                let minted = self.mint(device, &mint)?;
                serde_json::to_value(minted).map_err(|_| Refusal::Malformed)
            }
            FOLLOW => {
                let asking: Asking =
                    serde_json::from_value(argument).map_err(|_| Refusal::Malformed)?;
                self.follow(device, &asking.id)
            }
            _ => {
                let answering: Answering =
                    serde_json::from_value(argument).map_err(|_| Refusal::Malformed)?;
                self.answer(&answering).map(|()| json!({}))
            }
        }
    }

    /// Make the ticket `mint` asks for, for the page whose call came from `device`.
    fn mint(&self, device: [u8; 32], mint: &Mint) -> Result<Minted, Refusal> {
        let host = self.0.serving_host.get().cloned().ok_or(Refusal::NoPages)?;
        let reached = host.reached().ok_or(Refusal::Malformed)?;
        let now = tilo_clock::unix_now_ns();
        let full = |pages: &mut HashMap<[u8; 32], PageTicket>| {
            pages.retain(|_, page| page.gone_at_ns > now);
            pages.len() >= MAX_PAGE_TICKETS
        };
        if full(&mut self.0.pages.lock_recover()) {
            return Err(Refusal::TooMany);
        }
        let expires_at_ns = now.saturating_add(TICKET_LIFETIME_NS);
        let ticket = new_ticket(mint.direction, reached.clone()).map_err(|_| Refusal::Malformed)?;
        let shown = match mint.direction {
            Direction::Login => self.show_login(mint, &host, reached, &ticket, expires_at_ns)?,
            Direction::SignIn => self.show_sign_in(device, mint, &ticket, now, expires_at_ns)?,
        };
        let mut id = [0u8; 32];
        getrandom::fill(&mut id).map_err(|_| Refusal::Malformed)?;
        let mut pages = self.0.pages.lock_recover();
        if full(&mut pages) {
            return Err(Refusal::TooMany);
        }
        pages.insert(
            id,
            PageTicket {
                shown,
                gone_at_ns: expires_at_ns.saturating_add(KEEP_ANSWERED_NS),
            },
        );
        drop(pages);
        Ok(Minted {
            id: hex::encode(id),
            ticket: ticket.encode(),
        })
    }

    /// Serve a page's login `ticket`, which the page that asked for it answers.
    fn show_login(
        &self,
        mint: &Mint,
        host: &Arc<dyn PageHost>,
        reached: Reached,
        ticket: &Ticket,
        expires_at_ns: u64,
    ) -> Result<Shown, Refusal> {
        let user = hex_key(mint.user.as_deref()).ok_or(Refusal::Malformed)?;
        let workspace = mint
            .workspace
            .clone()
            .filter(|workspace| !workspace.is_empty())
            .ok_or(Refusal::Malformed)?;
        let host_origin = mint.host_origin.clone().unwrap_or_default();
        let entry = Entry {
            workspace,
            workspace_name: mint.name.clone(),
            server_cert: host.server_cert(),
            server: reached,
            site: mint.site.clone().unwrap_or_else(|| host_origin.clone()),
            host_origin,
        };
        let setup = Arc::new_cyclic(|setup: &std::sync::Weak<Setup>| {
            let approver = Arc::new(PageApprover(setup.clone()));
            let served = if mint.browser {
                ServedTicket::for_browser(
                    ticket.clone(),
                    user,
                    entry,
                    approver,
                    host.revocations(),
                    expires_at_ns,
                )
            } else {
                ServedTicket::new(
                    ticket.clone(),
                    user,
                    entry,
                    tilo_user_identity::DeviceCapabilities::member(),
                    approver,
                    host.revocations(),
                    expires_at_ns,
                )
            };
            Setup {
                served,
                asked: Mutex::default(),
                decision: watch::Sender::new(None),
                certified: Mutex::default(),
                declined: Mutex::default(),
                detail: Mutex::default(),
                steps: Mutex::default(),
                agent: Mutex::default(),
            }
        });
        self.0.served.serve(setup.served.clone());
        self.watch_certified(&setup, host);
        Ok(Shown::Setup(setup))
    }

    /// Keep a page's sign-in `ticket` for the tab whose key is `device`.
    fn show_sign_in(
        &self,
        device: [u8; 32],
        mint: &Mint,
        ticket: &Ticket,
        now: u64,
        expires_at_ns: u64,
    ) -> Result<Shown, Refusal> {
        let tab = Tab {
            device_public_key: hex::encode(device),
            label: mint
                .label
                .clone()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Browser".to_owned()),
            key_for: None,
        };
        // The name it asks to be listed by is one a certificate carries.
        tab.certificate(device, now)
            .map_err(|_| Refusal::Malformed)?;
        let named = match mint.user.as_deref() {
            Some(user) => Some(hex_key(Some(user)).ok_or(Refusal::Malformed)?),
            None => None,
        };
        let sign_in = Arc::new(SignIn {
            ticket: ticket.clone(),
            tab,
            from_page: true,
            expires_at_ns,
            person: Mutex::new(named),
            by: Mutex::default(),
            certified: watch::Sender::new(None),
        });
        let mut sign_ins = self.0.sign_ins.lock_recover();
        sign_ins.retain(|_, kept| kept.expires_at_ns.saturating_add(KEEP_ANSWERED_NS) > now);
        sign_ins.insert(sign_in.secret(), Arc::clone(&sign_in));
        drop(sign_ins);
        Ok(Shown::SignIn(sign_in))
    }

    /// Record the certificate `setup`'s ticket gives once it gives one, and keep it where `host`
    /// keeps the person's devices.
    fn watch_certified(&self, setup: &Arc<Setup>, host: &Arc<dyn PageHost>) {
        let mut certified = setup.served.certified();
        let setup = Arc::downgrade(setup);
        let host = Arc::clone(host);
        self.0.runtime.spawn(async move {
            loop {
                let given = certified.borrow().clone();
                if let Some(cert) = given {
                    host.keep(cert.clone());
                    if let Some(setup) = setup.upgrade() {
                        *setup.certified.lock_recover() = Some(cert);
                    }
                    return;
                }
                if certified.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    fn shown(&self, id: &str) -> Result<Shown, Refusal> {
        let id = hex_key(Some(id)).ok_or(Refusal::Unknown)?;
        let now = tilo_clock::unix_now_ns();
        self.0
            .pages
            .lock_recover()
            .get(&id)
            .filter(|page| page.gone_at_ns > now)
            .map(|page| page.shown.clone())
            .ok_or(Refusal::Unknown)
    }

    /// What was done with the ticket a page follows by `id`, asked from the key `device`.
    fn follow(&self, device: [u8; 32], id: &str) -> Result<Value, Refusal> {
        match self.shown(id)? {
            Shown::Setup(setup) => {
                let mut answer = Map::new();
                let asked = setup.asked.lock_recover().clone();
                if let Some(asked) = asked {
                    answer.insert(
                        "asked".to_owned(),
                        serde_json::to_value(asked).map_err(|_| Refusal::Malformed)?,
                    );
                }
                let certified = setup.certified.lock_recover().clone();
                if let Some(cert) = certified {
                    answer.insert(
                        "certified".to_owned(),
                        serde_json::to_string(&cert)
                            .map_err(|_| Refusal::Malformed)?
                            .into(),
                    );
                }
                let declined = setup.declined.lock_recover().clone();
                if let Some(declined) = declined {
                    answer.insert("declined".to_owned(), declined.into());
                }
                let agent = setup.agent.lock_recover().clone();
                if let Some(agent) = agent {
                    answer.insert("agent".to_owned(), agent.into());
                }
                let detail = setup.detail.lock_recover().clone();
                if let Some(detail) = detail {
                    answer.insert("detail".to_owned(), detail.into());
                }
                answer.insert(
                    "steps".to_owned(),
                    serde_json::to_value(setup.steps.lock_recover().clone())
                        .map_err(|_| Refusal::Malformed)?,
                );
                Ok(Value::Object(answer))
            }
            Shown::SignIn(sign_in) => {
                // The certificate is the tab's alone: only the key that asked reads it.
                if sign_in.tab.device_public_key != hex::encode(device) {
                    return Err(Refusal::NotTheTab);
                }
                let mut answer = Map::new();
                let person = *sign_in.person.lock_recover();
                if let Some(person) = person {
                    let by = sign_in
                        .by
                        .lock_recover()
                        .clone()
                        .unwrap_or_else(|| "Your device".to_owned());
                    answer.insert(
                        "claimed".to_owned(),
                        json!({ "user": hex::encode(person), "by": by }),
                    );
                }
                let certified = sign_in.certified.borrow().clone();
                if let Some(cert) = certified {
                    answer.insert(
                        "certified".to_owned(),
                        serde_json::to_string(&cert)
                            .map_err(|_| Refusal::Malformed)?
                            .into(),
                    );
                    // As text, as the certificate is: a page reads its signed times exactly.
                    let host = self.0.serving_host.get().ok_or(Refusal::NoPages)?;
                    answer.insert(
                        "hostCert".to_owned(),
                        serde_json::to_string(&host.server_cert())
                            .map_err(|_| Refusal::Malformed)?
                            .into(),
                    );
                }
                Ok(Value::Object(answer))
            }
        }
    }

    /// Take the page's answer to the computer's question.
    fn answer(&self, answer: &Answering) -> Result<(), Refusal> {
        let Shown::Setup(setup) = self.shown(&answer.id)? else {
            return Err(Refusal::Malformed);
        };
        // The served ticket kept the revocations known when it was minted: a page whose device
        // was removed since approves nothing, which only the host's live list knows.
        let removed = |approved: &Approved| {
            self.0.serving_host.get().is_some_and(|host| {
                host.revocations().is_revoked(
                    &approved.approver.user_public_key,
                    &approved.approver.device_public_key,
                )
            })
        };
        let decision = match (&answer.approved, &answer.declined) {
            (Some(approved), None) if removed(approved) => Err(Declined(
                "the device that showed this link was removed from its person's devices: \
                 make a new link on a device they still use"
                    .to_owned(),
            )),
            (Some(approved), None) => Ok(approved.clone()),
            (None, Some(reason)) => Err(Declined(reason.clone())),
            (None, None) => Err(Declined("the page said no".to_owned())),
            (Some(_), Some(_)) => return Err(Refusal::Malformed),
        };
        if let Err(declined) = &decision {
            *setup.declined.lock_recover() = Some(declined.0.clone());
        }
        setup.decision.send_replace(Some(decision));
        Ok(())
    }

    /// A computer's certify call, answered by the ticket whose secret it brings, after another
    /// look at the tickets kept elsewhere when nothing served here knows it.
    fn certify(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let now = tilo_clock::unix_now_ns();
        let look = self.0.look_again.get().cloned().filter(|_| {
            !self.0.served.knows(&request)
                && now.saturating_sub(*self.0.looked.lock_recover()) >= LOOK_AGAIN_NS
        });
        let Some(look) = look else {
            return self.0.served.invoke(caller, request);
        };
        self.looked();
        let (done, answer) = tilo_actions::remote::pending();
        let caller = caller.clone();
        let tickets = self.clone();
        self.0.runtime.spawn(async move {
            look().await;
            let answered = tickets.0.served.invoke(&caller, request).await;
            done.complete(answered.unwrap_or_else(|| {
                Err(ActionCallError::Unavailable {
                    reason: "the ticket's issuer stopped".into(),
                })
            }));
        });
        answer
    }
}

/// A ticket `direction` is served in, for the device reached at `issuer`, with a fresh secret.
fn new_ticket(direction: Direction, issuer: Reached) -> Result<Ticket, String> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|error| format!("no secret for a ticket: {error}"))?;
    Ok(Ticket {
        issuer,
        direction,
        secret,
    })
}

/// `text`, 32 bytes hex.
fn hex_key(text: Option<&str>) -> Option<[u8; 32]> {
    hex::decode(text?).ok()?.try_into().ok()
}

/// Whether `cert` is what `person`'s root signs for `tab`, as it signs one for any device of
/// theirs ([`Tab::certificate`]): exactly the key that asked, under the name it asked for, with
/// what its kind of key is given and for as long.
fn check_signed_in(tab: &Tab, person: [u8; 32], cert: &DeviceCert) -> Result<(), String> {
    if cert.user_public_key != person || cert.issuer.is_some() {
        return Err(
            "a key is signed in with its own certificate, signed by the person's root".to_owned(),
        );
    }
    tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns()).map_err(|_| {
        "the certificate the person's root signed for the key does not hold".to_owned()
    })?;
    let want = tab
        .certificate(cert.user_public_key, cert.issued_at_ns)
        .map_err(|error| format!("the key cannot be certified: {error}"))?;
    if cert.device_public_key != want.device_public_key {
        return Err("the certificate names another key than the one that asked".to_owned());
    }
    if cert.device_label != want.device_label
        || cert.capabilities != want.capabilities
        || cert.expires_at_ns != want.expires_at_ns
    {
        return Err(match &tab.key_for {
            Some(key_for) => format!(
                "the certificate gives the key more or other than {}",
                key_for.capability()
            ),
            None => "the certificate gives the key more or other than a browser of the person's"
                .to_owned(),
        });
    }
    Ok(())
}

/// A page asking for a ticket.
#[derive(Deserialize)]
struct Mint {
    /// What the ticket is for; logging in when not said.
    #[serde(default)]
    direction: Direction,
    /// Logging in: the person whose root approves the computer. Signing in: the person the tab
    /// is a device of, when it knows, whose devices are shown it.
    #[serde(default)]
    user: Option<String>,
    /// Logging in: the workspace the computer enters, as its host names it.
    #[serde(default)]
    workspace: Option<String>,
    /// That workspace's name, as the page shows it.
    #[serde(default)]
    name: Option<String>,
    /// The page's own origin, which the login keeps as its site.
    #[serde(default)]
    site: Option<String>,
    /// The origin the page reached this host at.
    #[serde(default)]
    host_origin: Option<String>,
    /// Logging in: a browser, which a pairing link adds, rather than a computer.
    #[serde(default)]
    browser: bool,
    /// Signing in: the name the tab asks to be listed by.
    #[serde(default)]
    label: Option<String>,
}

/// The ticket a page shows, and the id it follows it by.
#[derive(Serialize)]
struct Minted {
    id: String,
    ticket: String,
}

/// A page asking what was done with its ticket.
#[derive(Deserialize)]
struct Asking {
    id: String,
}

/// The page's yes or no to the computer's request.
#[derive(Deserialize)]
struct Answering {
    id: String,
    /// The approval the page signed.
    #[serde(default)]
    approved: Option<Approved>,
    /// Why the page said no.
    #[serde(default)]
    declined: Option<String>,
}

/// The page's side of an approval: the computer's question is handed to the page that follows
/// the ticket, and its answer awaited.
struct PageApprover(std::sync::Weak<Setup>);

impl Approver for PageApprover {
    fn approve(&self, asked: Asked) -> Approval {
        let Some(setup) = self.0.upgrade() else {
            return Box::pin(std::future::ready(Err(Declined(
                "the page went away".to_owned(),
            ))));
        };
        Box::pin(async move {
            *setup.asked.lock_recover() = Some(asked);
            let mut decision = setup.decision.subscribe();
            let waited = tokio::time::timeout(Duration::from_nanos(TICKET_LIFETIME_NS), async {
                loop {
                    let decided = decision.borrow_and_update().clone();
                    if let Some(answer) = decided {
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

/// [`Tickets::uncertified`].
struct Uncertified(Tickets);

impl ActionProvider for Uncertified {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "a device with no certificate calls only a ticket's actions".into(),
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER || address.resource != RESOURCE {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        if address.action == CERTIFY {
            return self.0.certify(caller, request);
        }
        if !matches!(
            address.action.as_str(),
            ISSUE | FOLLOW | ANSWER | PENDING | APPROVE
        ) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let Some(device) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let invocation = request.invocation;
        if let Some((from, state)) = self.0.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(if *from == device {
                Ok(state.receipt())
            } else {
                Err(ActionCallError::NotPermitted)
            });
        }
        let argument = match &request.argument {
            InvokeArgument::Json(argument) => argument.clone(),
            _ => Map::new(),
        };
        let answer = if matches!(address.action.as_str(), PENDING | APPROVE) {
            // A page of the person's holds no session in this scope: it shows its own
            // certificate, for the key its call comes from, to say whose devices it lists or signs
            // a key in for.
            self.0
                .presented(device, &argument)
                .and_then(|person| self.0.as_person(&address.action, person, &argument))
        } else {
            self.0
                .page(device, &address.action, Value::Object(argument))
                .map_err(|refusal| ActionCallError::refused(refusal.message()))
        };
        Pending::ready(Ok(self.0.settle(invocation, device, answer)))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0
            .certified_observe(caller, id)
            .or_else(|| self.0.0.served.observe(caller, id))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.0.0.served.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if self.0.certified_observe(caller, id).is_some() {
            return Pending::ready(Err(ActionCallError::NotCancellable {
                reason: "a ticket's action ends as it is called".into(),
            }));
        }
        self.0.0.served.cancel(caller, id)
    }
}

/// [`Tickets::certified`].
struct Certified(Tickets);

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
        if !Tickets::answers_certified(&request) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        self.0.certified_invoke(caller, &request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.certified_observe(caller, id)
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
