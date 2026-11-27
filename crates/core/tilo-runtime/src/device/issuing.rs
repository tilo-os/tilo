//! Issuing and serving tickets for a new computer, the one way every device of the person's does
//! it: the session's desktop (Settings' "Add a device", `tilo machine add`) and the daemon of a
//! computer `tilo` runs on (`tilo device add`).
//!
//! A ticket is one string: where this device is reached, and a one-use secret
//! ([`tilo_device_identity::login`]). The new computer dials this device over its device action
//! service and calls the ticket's certify action there; the service admits that call alone
//! ([`SCOPE`](tilo_device_identity::login::SCOPE)), and the person's root, held here, signs the computer's certificate. The person
//! asked for the ticket themselves, so the computer that comes with it is approved as it arrives.
//!
//! A ticket can name a role: a server, which gets an entry among the person's servers, or a
//! planned machine, which takes its plan's entry. The entry is taken before the computer is
//! approved, and a computer whose entry was not taken is told why and joins nowhere.
//!
//! A planned machine's ticket is kept with its plan. The medium carrying it may sit on a shelf
//! for a day, and the device that issued it may restart meanwhile, so the ticket is an entry of
//! the vault of the person's Servers workspace, sealed to the person's devices like every entry
//! there, until the machine takes the plan, the plan goes, or the ticket runs out. A restarted
//! issuer serves every such ticket it issued again ([`TicketServer::restore`]), as it does when a
//! computer brings a secret nothing served here knows. Every ticket is served through the
//! device's one [`Tickets`](tilo_peer_state::new_device::Tickets). Only the device a ticket names serves it:
//! another device of the person's reads the entry and leaves it.
//!
//! What is the embedder's own (its endpoint and action service, how it signs, where its root and
//! its workspaces are, where a computer it adds enters) it supplies as an [`Issuer`]. The same
//! server names and revokes the person's devices in the record of their own workspace, and keeps
//! there each computer it adds.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};

use collab_types::signer::DeviceSigner;
use futures::future::BoxFuture;
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Declined, Entry, PLANNED_TICKET_LIFETIME_NS, Reached,
    TICKET_LIFETIME_NS, Ticket,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::ActionService;
use tilo_peer_state::new_device::{Keeping, ServedTicket};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, StatementSigner};
use tilo_workspace_records::vault_items::{SecretId, SecretItem, SecretItemKind, VaultChange};

use tilo_workspace_service::workspaces::{Workspaces, person_workspace, servers};

mod manage;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// The field of a vault entry that names the plan whose ticket the entry keeps.
const PLAN_FIELD: &str = "tilo.planned-machine.plan";
/// The field of a vault entry that says when the kept ticket runs out, in nanoseconds.
const EXPIRES_FIELD: &str = "tilo.planned-machine.expires";
/// The tag a planned machine's ticket carries in the vault.
const TICKET_TAG: &str = "planned-machine-ticket";
/// What a kept ticket's entry id is derived under, from its plan's id.
const ENTRY_DOMAIN: &[u8] = b"tilo-planned-machine-ticket-v1\0";

/// What this device takes a certified computer in as, beside one of the person's devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// A server, which keeps the person's workspaces: a new entry among their servers.
    Server,
    /// The machine the plan names: the plan's entry becomes the machine's.
    Plan(String),
}

/// The person's root, as it signs a computer's certificate.
pub type Root = Arc<dyn StatementSigner + Send + Sync>;

/// What the device a [`TicketServer`] runs on supplies: what is its own.
pub trait Issuer: Send + Sync + 'static {
    /// Where this device is reached, as a ticket names it, and the action service that answers a
    /// ticket's certify call there: why not, while the network is not up.
    ///
    /// # Errors
    /// This device is not reachable yet, or takes part as nobody.
    fn serving(&self) -> Result<(Reached, ActionService), String>;

    /// This device's certificate, which the person's root signed, and what signs as it.
    ///
    /// # Errors
    /// This device has no identity yet.
    fn device(&self) -> Result<(DeviceCert, Arc<dyn DeviceSigner>), String>;

    /// The person's root, when this device holds it.
    ///
    /// # Errors
    /// The root could not be read.
    fn root(&self) -> Result<Option<Root>, String>;

    /// Where a computer this device certifies enters.
    fn entry(&self) -> BoxFuture<'_, Result<Entry, String>>;

    /// The workspace service holding the person's Servers workspace here, when one does: where
    /// plans and their tickets are.
    fn servers(&self) -> Option<Workspaces>;

    /// The person's devices this device knows itself, beside the record of their own workspace:
    /// the keys its root certified here, which a revocation takes into the record when it has
    /// not got them yet. None by default.
    fn known(&self) -> Vec<DeviceCert> {
        Vec::new()
    }

    /// Take the certified computer `cert` in as `role`: by default through [`Self::servers`].
    fn admit<'a>(
        &'a self,
        role: &'a Role,
        cert: &'a DeviceCert,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(admit_in(self.servers(), role, cert))
    }
}

/// Take the certified computer `cert` in as `role` in the person's Servers workspace `workspaces`
/// holds.
///
/// # Errors
/// No Servers workspace is held here, or its document refused the entry.
pub async fn admit_in(
    workspaces: Option<Workspaces>,
    role: &Role,
    cert: &DeviceCert,
) -> Result<(), String> {
    let workspaces = workspaces
        .ok_or_else(|| "this computer holds no Servers workspace to take it into".to_owned())?;
    match role {
        Role::Plan(plan) => {
            servers::admit_planned(&workspaces, plan, &cert.device_public_key).await
        }
        Role::Server => servers::admit_reached(&workspaces, cert).await,
    }
}

/// The tickets this device serves: the one printed last for a computer at hand, and every
/// planned machine's, each kept with its plan.
#[derive(Clone)]
pub struct TicketServer(Arc<Inner>);

struct Inner {
    issuer: Arc<dyn Issuer>,
    /// The ticket issued last, and whether it is a planned machine's: a newer ticket for a
    /// computer at hand replaces it unless it is.
    printed: Mutex<Option<(ServedTicket, bool)>>,
    /// Each planned machine's ticket served here, by its plan.
    planned: Mutex<HashMap<String, ServedTicket>>,
}

impl TicketServer {
    #[must_use]
    pub fn new(issuer: Arc<dyn Issuer>) -> Self {
        Self(Arc::new(Inner {
            issuer,
            printed: Mutex::default(),
            planned: Mutex::default(),
        }))
    }

    /// The ticket issued last, while it is served.
    #[must_use]
    pub fn last(&self) -> Option<ServedTicket> {
        self.0
            .printed
            .lock_recover()
            .as_ref()
            .map(|(served, _)| served.clone())
    }

    /// Serve a ticket for a computer to become one of the person's devices, and what `role`
    /// says beside: the ticket, as the one string the person hands on is its encoding. A planned
    /// machine's is kept with its plan before it is served. `beside` serves it beside every
    /// ticket served before it, none taking another's place, so each computer it is handed to
    /// can still spend it: what the first tickets a founding hands back ask for. Otherwise a
    /// ticket for a computer at hand takes the place of the one printed before it.
    ///
    /// # Errors
    /// This device cannot add a device (no identity, no network, no root of the person's), the
    /// plan is not one still waiting for its machine, or its ticket could not be kept.
    pub async fn issue(&self, role: Option<Role>, beside: bool) -> Result<Ticket, String> {
        self.issued(role, !beside).await
    }

    /// [`Self::issue`], whose ticket for a computer at hand takes the place of the one printed
    /// before it when `replaces`.
    async fn issued(&self, role: Option<Role>, replaces: bool) -> Result<Ticket, String> {
        // A device with no identity is told that first: it is what the person sets up first.
        self.0.issuer.device()?;
        let (reached, actions) = self.0.issuer.serving()?;
        let planned = match &role {
            Some(Role::Plan(plan)) => Some(self.waiting(plan).await?),
            Some(Role::Server) if self.0.issuer.servers().is_none() => {
                return Err(
                    "This computer's workspace service is not open, so it cannot take \
                            a server into the person's Servers workspace."
                        .to_owned(),
                );
            }
            _ => None,
        };
        let ticket = Ticket::new(reached);
        if let Some(Waiting {
            workspaces,
            plan,
            label,
            expires_at_ns,
        }) = planned
        {
            let served = self
                .served(
                    ticket.clone(),
                    Some(Role::Plan(plan.clone())),
                    expires_at_ns,
                )
                .await?;
            // Kept before it is served, so the ticket never lives only in this process.
            keep(&workspaces, (&plan, &label), &ticket, expires_at_ns).await?;
            *self.0.printed.lock_recover() = Some((served.clone(), true));
            self.serve_planned(&actions, plan, served);
        } else {
            let served = self
                .served(
                    ticket.clone(),
                    role,
                    tilo_clock::unix_now_ns().saturating_add(TICKET_LIFETIME_NS),
                )
                .await?;
            // A newer ticket for a computer at hand takes the place of the one printed before
            // it, which stops admitting a computer; a planned machine's stays served, and so does
            // one issued beside it.
            let tickets = actions.tickets();
            if replaces {
                let previous = self
                    .0
                    .printed
                    .lock_recover()
                    .replace((served.clone(), false));
                if let Some((withdrawn, false)) = previous {
                    tickets.withdraw(&withdrawn);
                }
            }
            tickets.serve(served);
        }
        self.look_again_on(&actions);
        Ok(ticket)
    }

    /// Serve the plan `plan`'s ticket `served` on `actions`, in place of one served for it
    /// before.
    fn serve_planned(&self, actions: &ActionService, plan: String, served: ServedTicket) {
        let tickets = actions.tickets();
        let replaced = self.0.planned.lock_recover().insert(plan, served.clone());
        if let Some(replaced) = replaced {
            tickets.withdraw(&replaced);
        }
        tickets.serve(served);
    }

    /// Serve again every planned machine's ticket this device issued that still waits for its
    /// machine, and let go of those whose plan was taken or went, or that ran out: what a device
    /// does when it starts. Answers how many it serves.
    ///
    /// # Errors
    /// This device is not reachable yet, or the kept tickets could not be read.
    pub async fn restore(&self) -> Result<usize, String> {
        let (reached, actions) = self.0.issuer.serving()?;
        actions.tickets().looked();
        let Some(workspaces) = self.0.issuer.servers() else {
            return Ok(0);
        };
        let person = person_workspace::workspace_id(&workspaces);
        if workspaces.state(&person).await.is_none() {
            return Ok(0);
        }
        let kept = kept(&workspaces).await?;
        if kept.is_empty() {
            return Ok(0);
        }
        let plans = servers::read(&workspaces).await?;
        let now = tilo_clock::unix_now_ns();
        let mut gone = Vec::new();
        let mut serving = 0_usize;
        for Kept {
            plan,
            ticket,
            expires_at_ns,
        } in kept
        {
            // Another device of the person's issued it: only that device is dialled with it.
            if ticket.issuer.endpoint != reached.endpoint {
                continue;
            }
            if expires_at_ns <= now || plans.unclaimed(&plan).is_none() {
                gone.push(plan);
                continue;
            }
            serving = serving.saturating_add(1);
            let held = self.0.planned.lock_recover().get(&plan).cloned();
            if held.is_some_and(|held| held.ticket().secret == ticket.secret && held.waiting()) {
                continue;
            }
            let served = self
                .served(ticket, Some(Role::Plan(plan.clone())), expires_at_ns)
                .await?;
            self.serve_planned(&actions, plan, served);
        }
        forget(&workspaces, &gone).await?;
        if serving > 0 {
            self.look_again_on(&actions);
        }
        Ok(serving)
    }

    /// The person's Servers workspace here, the plan `plan` names while it waits for its
    /// machine, and when its ticket runs out: the plan's own expiry, within a day.
    async fn waiting(&self, plan: &str) -> Result<Waiting, String> {
        let workspaces = self.0.issuer.servers().ok_or_else(|| {
            "This computer holds no Servers workspace, so it cannot add a planned machine."
                .to_owned()
        })?;
        let plans = servers::read(&workspaces).await?;
        let waiting = plans.unclaimed(plan).ok_or_else(|| {
            "no planned machine waiting for its ticket is named by that id".to_owned()
        })?;
        let expires_at_ns = waiting
            .plan
            .expires_at_ns
            .min(tilo_clock::unix_now_ns().saturating_add(PLANNED_TICKET_LIFETIME_NS));
        if expires_at_ns <= tilo_clock::unix_now_ns() {
            return Err("this plan's ticket ran out: plan the machine again".to_owned());
        }
        Ok(Waiting {
            label: waiting.label.clone(),
            plan: plan.to_owned(),
            expires_at_ns,
            workspaces,
        })
    }

    /// `ticket`, served for a computer to become one of the person's devices and `role` until
    /// `expires_at_ns`, approved by the person's root here.
    async fn served(
        &self,
        ticket: Ticket,
        role: Option<Role>,
        expires_at_ns: u64,
    ) -> Result<ServedTicket, String> {
        let issuer = &self.0.issuer;
        let (cert, device) = issuer.device()?;
        // Only the person's root adds a computer: a member device. A server they issued the
        // ticket for is their own whatever it runs; any other computer is native when it reports
        // Tilo OS as it logs in, else foreign.
        let Some(root) = issuer.root()? else {
            return Err(
                "This computer does not hold your identity, so it cannot add a device: \
                        add one from a computer that does."
                    .to_owned(),
            );
        };
        if root.public_key() != cert.user_public_key {
            return Err(
                "This computer is logged in as another person than the one whose identity it \
                 holds."
                    .to_owned(),
            );
        }
        let enter = issuer.entry().await?;
        let serve = if role == Some(Role::Server) {
            ServedTicket::for_server
        } else {
            ServedTicket::new
        };
        let person = cert.user_public_key;
        let served = serve(
            ticket,
            person,
            enter,
            DeviceCapabilities::member(),
            Arc::new(RootApprover {
                root,
                device,
                cert,
                role,
                tickets: Arc::downgrade(&self.0),
            }),
            RevocationList::new(),
            expires_at_ns,
        );
        // Every device of the person's lists the computer from now on: the record of their own
        // workspace, which this device writes as the redemption completes.
        Ok(match self.0.issuer.servers() {
            Some(workspaces) => served.kept_by(Arc::new(KeptInRecord(workspaces))),
            None => served,
        })
    }

    /// Have the tickets `actions` serves look at the kept tickets again when a computer brings a
    /// secret nothing served there knows: its ticket may have been issued before this device
    /// last started.
    fn look_again_on(&self, actions: &ActionService) {
        let issued = Arc::downgrade(&self.0);
        actions.tickets().look_again_with(Arc::new(move || {
            let issued = issued.upgrade().map(TicketServer);
            Box::pin(async move {
                if let Some(issued) = issued
                    && let Err(error) = issued.restore().await
                {
                    tracing::warn!(%error, "the kept tickets were not read again");
                }
            })
        }));
    }

    /// The machine the plan `plan` describes took its entry: its ticket is done with.
    async fn taken(&self, plan: &str) {
        self.0.planned.lock_recover().remove(plan);
        let Some(workspaces) = self.0.issuer.servers() else {
            return;
        };
        if let Err(error) = forget(&workspaces, &[plan.to_owned()]).await {
            // The next start lets go of it: its plan is taken.
            tracing::warn!(%error, "a taken plan's ticket was not let go of yet");
        }
    }
}

/// The person's root approving a computer that came with a ticket: it signs the computer's
/// certificate, and the approval with this device's own key, which the root certified. A
/// computer the ticket names a role for is taken in as that before it is approved.
struct RootApprover {
    root: Root,
    device: Arc<dyn DeviceSigner>,
    cert: DeviceCert,
    role: Option<Role>,
    tickets: Weak<Inner>,
}

impl Approver for RootApprover {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        let signed = tilo_user_identity::sign_device_cert(&mut cert, self.root.as_ref());
        let device = Arc::clone(&self.device);
        let approver = self.cert.clone();
        let role = self.role.clone();
        let tickets = self.tickets.upgrade().map(TicketServer);
        Box::pin(async move {
            signed.map_err(|error| {
                Declined(format!("this computer's root could not sign: {error}"))
            })?;
            // A computer that did not take its entry would join as nobody's server: it is told
            // so, and its install stops with that reason.
            if let Some(role) = &role {
                let tickets = tickets.as_ref().ok_or_else(|| {
                    Declined("the device that served the ticket stopped".to_owned())
                })?;
                tickets.0.issuer.admit(role, &cert).await.map_err(|error| {
                    Declined(format!("its entry did not take this computer: {error}"))
                })?;
                if let Role::Plan(plan) = role {
                    tickets.taken(plan).await;
                }
            }
            let signature = device
                .sign(asked.digest.to_vec())
                .await
                .map_err(|error| Declined(format!("this computer could not sign: {error}")))?;
            Ok(Approved {
                cert,
                approver,
                signature,
            })
        })
    }
}

/// The record of the person's devices, in their own workspace `workspaces` holds: where a computer
/// a ticket here certified is listed, so every device of theirs reads it among their own and the
/// hosts holding the workspace admit it.
struct KeptInRecord(Workspaces);

impl Keeping for KeptInRecord {
    fn keep(&self, cert: DeviceCert) -> tilo_peer_state::new_device::Kept {
        let workspaces = self.0.clone();
        Box::pin(async move {
            if let Err(error) = person_workspace::devices::keep(&workspaces, &cert).await {
                tracing::warn!(%error, "the added computer is listed once it enters");
            }
        })
    }
}

/// A planned machine's ticket as the vault keeps it. It has no `Debug`: nothing prints it.
struct Kept {
    plan: String,
    ticket: Ticket,
    expires_at_ns: u64,
}

/// A plan waiting for its machine: the Servers workspace holding it, its id and name, and when
/// its ticket runs out.
struct Waiting {
    workspaces: Workspaces,
    plan: String,
    label: String,
    expires_at_ns: u64,
}

/// The id of the vault entry that keeps the plan `plan`'s ticket: one per plan, so a ticket
/// issued again for it replaces the one before.
fn entry_id(plan: &str) -> Result<SecretId, String> {
    let digest = tilo_user_identity::canonical::signing_digest(ENTRY_DOMAIN, &plan)
        .map_err(|error| error.to_string())?;
    Ok(SecretId(hex::encode(digest.get(..16).unwrap_or_default())))
}

/// Keep `ticket`, the plan `plan`'s (named `label`), live until `expires_at_ns`, in the vault of
/// the person's Servers workspace.
async fn keep(
    workspaces: &Workspaces,
    (plan, label): (&str, &str),
    ticket: &Ticket,
    expires_at_ns: u64,
) -> Result<(), String> {
    let now = tilo_clock::unix_now();
    let entry = SecretItem {
        id: entry_id(plan)?,
        kind: SecretItemKind::Other,
        title: format!("Ticket for {label}"),
        username: String::new(),
        password: ticket.encode(),
        origins: BTreeSet::new(),
        notes: String::new(),
        fields: BTreeMap::from([
            (PLAN_FIELD.to_owned(), plan.to_owned()),
            (EXPIRES_FIELD.to_owned(), expires_at_ns.to_string()),
        ]),
        tags: BTreeSet::from([TICKET_TAG.to_owned()]),
        created_unix: now,
        updated_unix: now,
    };
    let workspace = person_workspace::workspace_id(workspaces);
    let opened = workspaces.open_vault(&workspace).await?;
    // The person's own Servers workspace, kept by their device for them.
    workspaces
        .write_vault(&opened, vec![VaultChange::Keep(entry)], None, None)
        .await
        .map_err(|error| format!("the planned machine's ticket was not kept: {error}"))
}

/// Every planned machine's ticket the vault of the person's Servers workspace keeps.
async fn kept(workspaces: &Workspaces) -> Result<Vec<Kept>, String> {
    let workspace = person_workspace::workspace_id(workspaces);
    // Looking for kept tickets makes no keys: a vault nobody wrote keeps none.
    let Some(opened) = workspaces.read_made_vault(&workspace).await? else {
        return Ok(Vec::new());
    };
    Ok(opened
        .entries()
        .iter()
        .filter(|(entry, _)| entry.tags.contains(TICKET_TAG))
        .filter_map(|(entry, _)| {
            Some(Kept {
                plan: entry.fields.get(PLAN_FIELD)?.clone(),
                ticket: Ticket::parse(&entry.password).ok()?,
                expires_at_ns: entry.fields.get(EXPIRES_FIELD)?.parse().ok()?,
            })
        })
        .collect())
}

/// Let go of the tickets of `plans` the vault keeps.
async fn forget(workspaces: &Workspaces, plans: &[String]) -> Result<(), String> {
    if plans.is_empty() {
        return Ok(());
    }
    let changes = plans
        .iter()
        .map(|plan| entry_id(plan).map(VaultChange::Forget))
        .collect::<Result<Vec<_>, _>>()?;
    let workspace = person_workspace::workspace_id(workspaces);
    let opened = workspaces.open_vault(&workspace).await?;
    workspaces.write_vault(&opened, changes, None, None).await
}
