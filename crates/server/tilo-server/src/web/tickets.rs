//! Tickets a page's host serves for it: a tab has no iroh endpoint of its own, so the host a page
//! is on mints the ticket its agent's computer logs in with, and the tab approves that computer
//! with the root it holds ([`tilo_device_identity::login`]).
//!
//! The host serves the ticket the way a daemon serves one it printed
//! ([`tilo_peer_state::new_device::ServedTicket`]): a computer with no certificate dials the
//! host's endpoint, calls `tilo.device`'s `devices#certify` with the ticket's secret and its own
//! key, and the host answers once someone approved it. Here that someone is the page: the ticket
//! is made for the page's person, the page asks who is asking, signs the certificate the host
//! built with its root and signs the approval with its own device key, and the host checks both
//! with [`check_approval`](tilo_device_identity::login::check_approval) before the computer is
//! answered.
//!
//! Nothing is kept once a ticket is answered or runs out, and a ticket names no workspace of its
//! own: the page says which workspace the computer enters (its person's own), and the host fills
//! in where that workspace is reached and through which device.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Declined, Direction, Entry, Reached, ServerAddress,
    TICKET_LIFETIME_NS, Ticket,
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

/// A page asking its host for the ticket its agent's computer logs in with.
#[derive(Deserialize)]
pub(super) struct Mint {
    /// The person whose root approves the computer, hex.
    pub user: String,
    /// The workspace the computer enters, as its host names it.
    pub workspace: String,
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

/// A page asking whether the computer has shown up, and what it asked.
#[derive(Deserialize)]
pub(super) struct Asking {
    pub id: String,
}

/// What the page is answered: the computer's question, or where its call stands.
#[derive(Serialize, Default)]
pub(super) struct AskingAnswer {
    /// The computer's request, for the page to sign, once it has shown up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<Asked>,
    /// The computer's certificate, once the host answered it: the setup is over.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certified: Option<String>,
    /// Why the computer was not certified, where its call ended that way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declined: Option<String>,
    /// The agent that runs on the computer, once it named itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
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

/// Why a ticket route was refused: the sentence it answers.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Malformed,
    Unknown,
    TooMany,
}

impl Refusal {
    pub(super) const fn message(&self) -> &'static str {
        match self {
            Self::Malformed => "not a request this route takes",
            Self::Unknown => "no such ticket, or it ran out; ask the page for a new one",
            Self::TooMany => "too many tickets are waiting here; try again in a minute",
        }
    }
}

/// One ticket a page showed, and the question the computer asked with it.
struct Slot {
    /// The ticket's secret, hex: what the computer's call is routed by.
    secret: String,
    /// The ticket as the host serves it: the certify action, once a computer asks.
    served: Mutex<Option<ServedTicket>>,
    /// The computer's request, once it showed up.
    asked: Mutex<Option<Asked>>,
    /// The page's answer, once it gave one.
    decision: tokio::sync::watch::Sender<Option<Result<Approved, Declined>>>,
    /// The certificate the host answered the computer with.
    certified: Mutex<Option<DeviceCert>>,
    /// Why the computer's call ended without a certificate.
    declined: Mutex<Option<String>>,
}

impl Slot {
    fn decided(&self) -> Option<Result<Approved, Declined>> {
        self.decision.borrow().clone()
    }

    fn served(&self) -> Option<ServedTicket> {
        self.served.lock_recover().clone()
    }
}

/// The tickets a host serves for its pages, by the id each page keeps.
#[derive(Clone)]
pub(super) struct Tickets {
    entries: Arc<Mutex<HashMap<[u8; 32], Arc<Slot>>>>,
    /// The host's certification, for the revocations it knows and the record each certified
    /// device joins.
    certification: crate::certification::CertificationProtocol,
}

impl Tickets {
    /// The tickets of a host whose person's devices `certification` records.
    pub(super) fn new(certification: crate::certification::CertificationProtocol) -> Self {
        Self {
            entries: Arc::default(),
            certification,
        }
    }

    /// Make a ticket for `mint`'s person, entering `mint`'s workspace through this host, and
    /// answer the page its text and the id it asks by.
    ///
    /// # Errors
    /// The request is malformed, or too many tickets are waiting here already.
    pub(super) fn mint(&self, mint: &Mint, info: &ServerInfo) -> Result<Minted, Refusal> {
        let user: [u8; 32] = hex::decode(&mint.user)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Refusal::Malformed)?;
        let address = info.address.as_ref().ok_or(Refusal::Malformed)?;
        let host_origin = info.origin.clone().unwrap_or_default();
        let mut entries = self.entries.lock_recover();
        if entries.len() >= MAX_TICKETS {
            return Err(Refusal::TooMany);
        }
        let reached = reached_of(address);
        let entry = Entry {
            workspace: mint.workspace.clone(),
            workspace_name: mint.name.clone(),
            server_cert: info.server_cert.clone(),
            server: reached.clone(),
            site: mint.site.clone().unwrap_or_else(|| host_origin.clone()),
            host_origin,
        };
        let ticket = Ticket::in_direction(Direction::Login, reached);
        let slot = Arc::new(Slot {
            secret: ticket.secret_hex(),
            served: Mutex::new(None),
            asked: Mutex::default(),
            decision: tokio::sync::watch::Sender::new(None),
            certified: Mutex::default(),
            declined: Mutex::default(),
        });
        let served = ServedTicket::new(
            ticket.clone(),
            user,
            entry,
            Arc::new(PageApprover(Arc::clone(&slot))),
            self.certification.revocations(),
        );
        let mut id = [0u8; 32];
        getrandom::fill(&mut id).map_err(|_| Refusal::Malformed)?;
        *slot.served.lock_recover() = Some(served.clone());
        entries.insert(id, Arc::clone(&slot));
        drop(entries);
        self.watch_certified(Arc::clone(&slot), served);
        self.expire(id);
        Ok(Minted {
            id: hex::encode(id),
            ticket: ticket.encode(),
        })
    }

    /// Record the certificate `served` gives, once it gives one, and keep the ticket readable
    /// for a while so the page that showed it sees the setup is over.
    fn watch_certified(&self, slot: Arc<Slot>, served: ServedTicket) {
        let certification = self.certification.clone();
        let mut certified = served.certified();
        tokio::spawn(async move {
            loop {
                if let Some(cert) = certified.borrow().clone() {
                    certification.keep_issued_device(&cert, &cert.device_label);
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
    /// or a computer that never showed up.
    fn expire(&self, id: [u8; 32]) {
        let tickets = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_nanos(TICKET_LIFETIME_NS) + KEEP_ANSWERED).await;
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

    /// What the computer asked with ticket `id`, or where its call stands.
    ///
    /// # Errors
    /// No such ticket is live here.
    pub(super) fn asking(&self, id: &str) -> Result<AskingAnswer, Refusal> {
        let slot = self.get(id)?;
        let asked = slot.asked.lock_recover().clone();
        Ok(AskingAnswer {
            agent: asked.as_ref().map(|asked| asked.agent.clone()),
            asked,
            certified: slot
                .certified
                .lock_recover()
                .as_ref()
                .and_then(|cert| serde_json::to_string(cert).ok()),
            declined: slot.declined.lock_recover().clone(),
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
        if decision.is_err() {
            *slot.declined.lock_recover() = Some(match &decision {
                Err(declined) => declined.0.clone(),
                Ok(_) => unreachable!("a no is an error"),
            });
        }
        slot.decision.send_replace(Some(decision));
        Ok(())
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
        self.tickets
            .entries
            .lock_recover()
            .values()
            .find(|slot| slot.secret == secret)?
            .served()
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
