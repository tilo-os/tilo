//! The servers of a person, configured in their workspace.
//!
//! The person's workspace is an ordinary workspace whose only member is the
//! person, and it holds what is theirs alone; their servers are one part of
//! it, and it is still made with the name "Servers". Each server is one of their devices and takes part in it, and so
//! does each of their other devices. Its [`SERVERS`] document says, per server,
//! what that server should be: its name, how much it offers
//! (storage, compute, machines), which updates it follows and when it may
//! restart for maintenance, how it unlocks after a boot, whether it was
//! retired, and the machine itself: its hostname, the drives it uses for what
//! and how its network interfaces are set up ([`device`]). Every change is a
//! typed action on that document ([`Servers`]), so it
//! has history and an author, and a device, an agent or a script change it the
//! same way. A server applies what its entry says; nothing about it lives in a
//! file of its own.
//!
//! Each entry also carries what the person asked that server to do
//! ([`Asked`]): its health, a service's journal, a restart, a reboot, an
//! update, a pause or resumption of updates, a rollback, a drain or making
//! Tilo its default start. Asking is
//! an action on the document, and so is answering; the server its entry names
//! carries the request out and keeps what it found or did, and every device
//! reads both. The action that asks (`server.status` and the others, served by
//! [`super::actions`]) waits for that answer and returns it.
//!
//! The workspace's id follows from the person's root
//! ([`person_workspace_id`]), so each of their devices finds the same one
//! without being told. One device founds it, as any workspace is founded, when
//! it is first needed ([`founded`]): the first machine planned, or the first
//! server entered. That device is its founder. Its one member is the person,
//! so it admits every device their root certified, as every workspace admits a
//! member's devices, with no server asked to let it in. A server is entered in
//! the document when the ticket it came in with is approved ([`admit`]), and
//! the devices that have the workspace then hand it the workspace as they hand
//! it every server the document names. A device that founds it again
//! anyway (one restored from the recovery words, say, before it heard of the
//! first) founds another history under the same id, which every host of the
//! first refuses: it replaces none of the person's servers. Once it holds the
//! workspace, a server keeps its own entry's address current ([`enroll`]).
//!
//! Why the person's servers are a workspace, and not part of the person's own
//! store beside their devices and workspace list: the servers restart into an
//! update one at a time under a lease of this workspace (`tilo_server::rollout`),
//! exactly-once work that only a governed workspace orders, and a server takes
//! no part in the person's store, whose link knows a device by its endpoint id
//! while a server's endpoint runs on a key of its own.
//!
//! Which workspaces a server holds, and how, is each workspace's governance
//! alone: its holders list names the server, as a store or a key holder, and
//! the workspace's own actions add, move and remove it. The document keeps no
//! copy of it. What it keeps is what the governance cannot name yet: the
//! workspaces a planned machine is to hold once it exists, which a device of
//! the person governing each hands it once, when it enrolled ([`attach`]).
//! Each server applies its own entry's offer to what it answers devices, its
//! update policy to what it follows, and a retired server lets go of
//! everything it holds.

pub use install_profile_model::plan::WaitingPlan;
use serde::Deserialize;
use tilo_resource::{Collection, Id, Invoke, Model, Refusal, resource};
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::transport::{OperationEntry, ServerOperation};
use tilo_user_identity::{DeviceCert, RevocationEntry, verify_revocation_entry};

use super::Workspaces;
use super::person_workspace::{NAME as PERSON_WORKSPACE_NAME, holds_person_workspace};

pub mod attach;
pub mod device;
pub mod machine;
pub mod plan;
mod refound;

/// The Servers workspace's document of the person's servers.
pub const SERVERS: (&str, &str) = ("so.tilo.servers", "servers");
/// The ways a server unlocks after a boot: an owner's device approves it, its
/// TPM unseals its keys, or someone types a passphrase at it.
pub const UNLOCK_POLICIES: [&str; 3] = ["owner-device", "tpm", "passphrase"];
/// The updates a server follows: the releases (`stable`), or the images built
/// from the development branch (`dev`).
pub const UPDATES: [&str; 2] = ["stable", "dev"];
/// How many answers to operations one server keeps.
pub const HISTORY_KEPT: usize = 20;
pub const MINUTES_PER_DAY: u32 = 24 * 60;
const MAX_REQUEST: usize = 64;

/// The person's servers, by device key (lowercase hex): each server's name,
/// what it holds and offers, the updates it follows, its maintenance window
/// and how it unlocks.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Servers {
    pub servers: Collection<Server>,
}

/// One server's desired configuration, and what was asked of it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Server {
    /// The name people see for it.
    pub label: String,
    /// Where it is dialled, as its own endpoint advertises it (JSON), kept so
    /// a device of its person can hand it a workspace. Empty until it
    /// enrolled one.
    pub endpoint: String,
    /// Gigabytes of storage it offers its workspaces.
    pub storage_gb: u64,
    /// Whether it does its workspaces' actions and jobs.
    pub compute: bool,
    /// How many Linux machines it serves its workspaces at once.
    pub machines: u32,
    /// The updates it follows ([`UPDATES`]); empty for the ones its image
    /// came with.
    pub updates: String,
    /// When its daily maintenance window starts, in minutes after midnight UTC.
    pub maintenance_start: u32,
    /// How long the window lasts in minutes; 0 for no window.
    pub maintenance_minutes: u32,
    /// How it unlocks after a boot ([`UNLOCK_POLICIES`]); empty for its image's
    /// default.
    pub unlock: String,
    /// Retired: the person took it out of service, and it serves nothing.
    pub retired: bool,
    /// The revocation of its device key the person's root signed when they
    /// asked it to retire (a [`RevocationEntry`] as JSON): once it is retired,
    /// every holder of the workspace, the server among them, refuses that key.
    pub revocation: String,
    /// The public half (PEM) of the key its TPM's rollback floor takes
    /// approvals from, as the server says; empty until it said.
    pub floor_key: String,
    /// The machine itself: its hostname, the drives it uses and how its
    /// network interfaces are set up ([`device::DeviceConfig`]).
    pub device: device::DeviceConfig,
    /// What an installation plans this machine to be, before it exists, and
    /// from what it was installed afterwards ([`plan::Pending`]).
    pub plan: plan::Pending,
    /// What the person asked of it, newest last: the operation and, once it
    /// ran, what it answered.
    pub asked: Collection<Asked>,
}

/// One operation the person asked of a server, and what it answered.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Asked {
    /// The operation, one word ([`ServerOperation::NAMES`]).
    pub operation: String,
    /// The systemd unit a `restart` or `logs` names; empty for the rest.
    pub service: String,
    /// How many journal lines a `logs` reads; 0 for the rest.
    pub lines: u32,
    /// When it was asked, nanoseconds since the epoch.
    pub at_ns: u64,
    /// What the server answered, its [`ServerOperationResult`] as JSON: empty
    /// until it answered.
    ///
    /// [`ServerOperationResult`]: tilo_server_protocol::transport::ServerOperationResult
    pub answer: String,
}

impl Asked {
    /// The operation that was asked, unless its name is no operation this
    /// build knows.
    #[must_use]
    pub fn operation(&self) -> Option<ServerOperation> {
        ServerOperation::named(&self.operation, &self.service, self.lines)
    }

    /// What the server answered, once it did.
    ///
    /// # Errors
    /// It answered nothing yet, or what it answered does not read.
    pub fn result(&self) -> Option<tilo_server_protocol::transport::ServerOperationResult> {
        tilo_failure::ignored!(
            "an answer this build cannot read reads as none",
            serde_json::from_str(&self.answer).ok()
        )
    }
}

impl Servers {
    /// The entry of the server whose device key is `device`, unless it was
    /// retired.
    #[must_use]
    pub fn active(&self, device: &[u8; 32]) -> Option<&Server> {
        self.servers
            .get(&device_id(device))
            .filter(|server| !server.retired)
    }

    /// The planned machine whose single-use token is named by `nonce`.
    #[must_use]
    pub fn planned_by_nonce(&self, nonce: &str) -> Option<(&Id, &Server)> {
        self.servers
            .iter()
            .find(|(_, server)| server.plan.planned() && server.plan.nonce == nonce)
    }

    fn server(&mut self, device: &Id) -> Result<&mut Server, Refusal> {
        self.servers
            .get_mut(device)
            .filter(|server| !server.retired)
            .ok_or_else(|| Refusal::new("no such server"))
    }

    /// The machines still to install, by the plan's id: the person's devices
    /// read what each waits for, and the machine reads its own plan.
    pub fn waiting(&self) -> impl Iterator<Item = (&Id, &Server)> {
        self.servers
            .iter()
            .filter(|(_, server)| server.plan.planned())
    }

    /// The machines the person planned and has not installed, each as its
    /// devices and the installer read it: the plan document, the drives its
    /// machine reported since, and the questions it still asks.
    ///
    /// # Errors
    /// An entry does not read back as a plan, which only a value kept outside
    /// the plan's own rules makes.
    pub fn waiting_plans(&self) -> Result<Vec<WaitingPlan>, String> {
        let mut waiting = Vec::new();
        for (_, server) in self.waiting() {
            waiting.push(as_plan(server)?);
        }
        waiting.sort_by(|left, right| left.plan.plan.cmp(&right.plan.plan));
        Ok(waiting)
    }

    /// The planned machine whose plan is `plan`, as [`Self::waiting_plans`]
    /// tells it.
    ///
    /// # Errors
    /// No machine is planned under that id, or its entry does not read back as
    /// a plan.
    pub fn waiting_plan(&self, plan: &str) -> Result<WaitingPlan, String> {
        self.waiting()
            .find(|(id, server)| id.as_str() == plan || server.plan.plan == plan)
            .ok_or_else(|| "no planned machine is named by that id".to_owned())
            .and_then(|(_, server)| as_plan(server))
    }
}

/// `server`'s waiting entry as the plan its devices and machine read.
fn as_plan(server: &Server) -> Result<WaitingPlan, String> {
    let drives = server
        .plan
        .drives
        .iter()
        .map(|(_, drive)| install_profile_model::plan::Drive {
            model: drive.model.clone(),
            identity: drive.identity.clone(),
            bytes: drive.bytes,
            holds_data: drive.holds_data,
            boots: drive.boots,
        })
        .collect();
    let asks = server
        .plan
        .ask
        .iter()
        .map(|(choice, asked)| install_profile_model::plan::Question {
            choice: choice.as_str().to_owned(),
            reason: asked.reason.clone(),
        })
        .collect();
    Ok(WaitingPlan {
        plan: server.install_plan()?,
        drives,
        asks,
    })
}

impl Server {
    /// The operations still to do here, oldest first: what the person asked
    /// and this server did not answer yet.
    pub fn pending(&self) -> impl Iterator<Item = (&Id, &Asked)> {
        self.asked
            .iter()
            .filter(|(_, asked)| asked.answer.is_empty())
    }

    /// The plan this entry keeps, as the document the machine installs from:
    /// its name, everything it was planned with, what its person's devices
    /// answered since, and what it still asks. The entry is the plan's only
    /// home, so this is the same document for the person and for the machine.
    ///
    /// It is `Err` when the entry does not read back as a plan, which only a
    /// value kept outside the plan's own rules makes.
    ///
    /// # Errors
    /// The entry does not read back as a plan.
    pub fn install_plan(&self) -> Result<install_profile_model::plan::Plan, String> {
        self.plan.document(&self.label)
    }

    /// What was asked of it and answered, newest first.
    #[must_use]
    pub fn history(&self) -> Vec<OperationEntry> {
        let mut entries: Vec<OperationEntry> = self
            .asked
            .iter()
            .filter(|(_, asked)| !asked.answer.is_empty())
            .map(|(_, asked)| OperationEntry {
                at_ns: asked.at_ns,
                operation: asked.operation.clone(),
                summary: asked
                    .result()
                    .map_or_else(String::new, |result| result.summary),
                done: asked.result().is_some_and(|result| result.done),
            })
            .collect();
        entries.reverse();
        entries
    }
}

/// The Servers workspace's [`SERVERS`] document as every device founds it: the
/// empty document made as one change every device makes alike.
///
/// Two of the person's devices that first need the workspace before they can
/// see each other each write the document's first change. The genesis here is
/// made at time zero, under an actor derived from the document's identity alone
/// ([`tilo_resource::genesis::actor`]), so both write the same change, and the
/// entries each device writes afterwards go into the one `servers` map the
/// genesis carries instead of into a map of their own.
///
/// # Errors
/// The document's initializer fails, which is a bug in it.
pub fn genesis(workspace: &str) -> Result<automerge::AutoCommit, String> {
    super::person_workspace::genesis::<Servers>(workspace)
}

#[resource(kind = "tilo.servers", read_content = tilo_resource::read::fields)]
impl Servers {
    /// Plan a machine that is not installed yet: enter the secret-free plan
    /// document `plan` under its own id ([`plan`]), with the nonce of the
    /// single-use token its medium or rescue command carries, so the machine is
    /// recognised when it enrolls and the person's devices see it waiting. It
    /// is one document, checked by the same rules the machine checks it with.
    /// A plan already there keeps what it holds and takes the new document and
    /// revision.
    #[action(external)]
    pub fn plan_machine(&mut self, plan: String, label: String) -> Result<(), Refusal> {
        let document: install_profile_model::plan::Plan = serde_json::from_str(&plan)
            .map_err(|_| Refusal::new("a machine's plan is the document it installs from"))?;
        let label = validate_label(&label)?;
        let id = Id::from_raw(document.plan.clone());
        let PendingPlan {
            nonce,
            expires_at_ns,
            destination,
            network,
            unlock,
            updates,
            disk,
            open,
            hosting,
        } = PendingPlan::of(&document);
        match self.servers.get_mut(&id) {
            Some(server) => {
                server.label = label;
                server.plan.destination = destination;
                server.plan.network = network;
                server.plan.unlock = unlock;
                server.plan.updates = updates;
                server.plan.disk = disk;
                server.plan.open = open;
                server.plan.revision = server.plan.revision.saturating_add(1);
                server.plan.nonce = nonce;
                server.plan.expires_at_ns = expires_at_ns;
                server.plan.hosting = hosting;
                plan::PLANNED.clone_into(&mut server.plan.status);
            }
            None => {
                self.servers.push(
                    id,
                    Server {
                        label,
                        plan: plan::Pending {
                            status: plan::PLANNED.to_owned(),
                            plan: document.plan,
                            nonce,
                            expires_at_ns,
                            destination,
                            network,
                            unlock,
                            updates,
                            disk,
                            open,
                            hosting,
                            revision: 1,
                            ..plan::Pending::default()
                        },
                        ..Server::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// The machine that enrolled with the token `nonce` is installing: its
    /// plan becomes its entry, under its device key, keeping the name and the
    /// machine configuration the plan was written with.
    ///
    /// It brings the drives it really has, so an answer can only name one of
    /// them, and the questions it cannot settle on its own: a choice the plan
    /// left open, or a disk rule that matched no drive or several. Nothing is
    /// guessed here; one of the person's devices answers
    /// ([`Self::answer_machine_choice`]).
    #[action(external)]
    pub fn begin_install(
        &mut self,
        device: String,
        nonce: String,
        drives: String,
        ask: String,
    ) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_device(&device)?;
        let nonce = validate_hex32(&nonce, "a token is named by its 16-byte nonce")?;
        let drives = validated_drives(&drives)?;
        let ask = validated_questions(&ask)?;
        let plan = self
            .planned_by_nonce(&nonce)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| Refusal::new("no planned machine answers that token"))?;
        if device != plan {
            if self.servers.contains(&device) {
                return Err(Refusal::new("that device already has an entry"));
            }
            let mut entry = self
                .servers
                .remove(&plan)
                .ok_or_else(|| Refusal::new("no planned machine answers that token"))?;
            plan::INSTALLING.clone_into(&mut entry.plan.status);
            entry.plan.drives = drives;
            entry.plan.ask = ask;
            self.servers.push(device, entry);
            return Ok(());
        }
        // The machine enrolled and took its entry when it was approved; this
        // is its own report of the drives it has and what it cannot settle,
        // arriving once it holds the person's Servers workspace.
        let entry = self
            .servers
            .get_mut(&device)
            .filter(|entry| entry.plan.planned())
            .ok_or_else(|| Refusal::new("no planned machine answers that token"))?;
        entry.plan.drives = drives;
        entry.plan.ask = ask;
        entry.plan.revision = entry.plan.revision.saturating_add(1);
        Ok(())
    }

    /// Answer the question a machine asked about `choice` with `value`, on one
    /// of the person's devices. The answer is checked against the machine's
    /// real report: a disk answer names a drive the machine reported, never
    /// one it did not.
    ///
    /// A choice the machine did not ask about, or one another answer settles
    /// (a network is set up on its entry, the workspaces it holds in each
    /// workspace), is
    /// refused with what answers it instead.
    #[action(external)]
    pub fn answer_machine_choice(
        &mut self,
        device: Id,
        choice: String,
        value: String,
    ) -> Result<(), Refusal> {
        let choice = choice.trim().to_owned();
        if !install_profile_model::plan::OPEN_CHOICES.contains(&choice.as_str()) {
            return Err(Refusal::new("a plan answers one of its own choices"));
        }
        let server = self.server(&device)?;
        if server.plan.status != plan::INSTALLING {
            return Err(Refusal::new("that machine is not installing"));
        }
        let value = value.trim().to_owned();
        match choice.as_str() {
            "disk" => {
                let rule =
                    install_profile_model::plan::DiskRule::decode(&value).map_err(Refusal::new)?;
                if !install_profile_model::plan::IDENTIFIED_RULES.contains(&rule.kind.as_str()) {
                    return Err(Refusal::new(
                        "an answered disk names the drive by its serial or WWN",
                    ));
                }
                if server.plan.drive(&rule.identity).is_none() {
                    return Err(Refusal::new(
                        "that drive is not one the machine reported; answer with one it did",
                    ));
                }
                server.plan.disk = rule.encode();
            }
            "name" => server.label = validate_label(&value)?,
            "destination" => {
                if !install_profile_model::plan::DESTINATIONS.contains(&value.as_str()) {
                    return Err(Refusal::new("a plan installs Tilo OS or Tilo Server"));
                }
                server.plan.destination = value;
            }
            "unlock" => {
                if !install_profile_model::plan::UNLOCK.contains(&value.as_str()) {
                    return Err(Refusal::new("a machine unlocks by one of its three ways"));
                }
                server.plan.unlock = value;
            }
            "updates" => {
                if !install_profile_model::plan::UPDATES.contains(&value.as_str()) {
                    return Err(Refusal::new("a machine follows stable or dev updates"));
                }
                server.plan.updates = value;
            }
            "network" => {
                return Err(Refusal::new(
                    "a machine's network is set up on its entry, not answered here",
                ));
            }
            _ => {
                return Err(Refusal::new(
                    "the workspaces a machine holds are set in each workspace, not answered here",
                ));
            }
        }
        server.plan.open = without_choice(&server.plan.open, &choice);
        server
            .plan
            .ask
            .remove(&Id::from_raw(choice.as_str().to_owned()));
        server.plan.revision = server.plan.revision.saturating_add(1);
        Ok(())
    }

    /// The machine finished installing: its entry is the server's from now on,
    /// and its plan says what it was installed from.
    #[action(external)]
    pub fn finish_install(&mut self, device: Id) -> Result<(), Refusal> {
        let server = self.server(&device)?;
        if server.plan.status != plan::INSTALLING {
            return Err(Refusal::new("that server is not installing"));
        }
        if !server.plan.ask.is_empty() {
            return Err(Refusal::new(
                "the machine is still waiting for an answer on one of the person's devices",
            ));
        }
        plan::INSTALLED.clone_into(&mut server.plan.status);
        Ok(())
    }

    /// Take a server into the person's servers, named `label` and dialled at
    /// `endpoint` (its own address, JSON). A server already there keeps its
    /// configuration and takes its new address, which changes as it is
    /// reached.
    #[action(external)]
    pub fn enroll_server(
        &mut self,
        device: String,
        label: String,
        endpoint: String,
    ) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_device(&device)?;
        let label = validate_label(&label)?;
        let endpoint = validate_endpoint(&endpoint)?;
        match self.servers.get_mut(&device) {
            Some(server) => server.endpoint = endpoint,
            None => {
                self.servers.push(
                    device,
                    Server {
                        label,
                        endpoint,
                        ..Server::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// Rename a server.
    #[action(external)]
    pub fn rename_server(&mut self, device: Id, label: String) -> Result<(), Refusal> {
        let label = validate_label(&label)?;
        self.server(&device)?.label = label;
        Ok(())
    }

    /// The workspace `workspace` its plan has a server hold was handed to it:
    /// the workspace's governance names it from then on, and says alone what
    /// it holds, so a holder taken out there later stays out.
    #[action(housekeeping)]
    pub fn hand_planned_workspace(&mut self, device: Id, workspace: String) -> Result<(), Refusal> {
        let planned = self
            .server(&device)?
            .plan
            .hosting
            .get_mut(&Id::from_raw(workspace))
            .ok_or_else(|| Refusal::new("its plan has it hold no such workspace"))?;
        planned.handed = true;
        Ok(())
    }

    /// Set how much a server offers its workspaces.
    #[action(external)]
    pub fn set_appetite(
        &mut self,
        device: Id,
        storage_gb: u64,
        compute: bool,
        machines: u32,
    ) -> Result<(), Refusal> {
        let server = self.server(&device)?;
        server.storage_gb = storage_gb;
        server.compute = compute;
        server.machines = machines;
        Ok(())
    }

    /// Set the updates a server follows.
    #[action(external)]
    pub fn set_updates(&mut self, device: Id, updates: String) -> Result<(), Refusal> {
        if !UPDATES.contains(&updates.as_str()) {
            return Err(Refusal::new("unknown updates a server follows"));
        }
        self.server(&device)?.updates = updates;
        Ok(())
    }

    /// Set a server's daily maintenance window: `minutes` from `start`
    /// minutes after midnight UTC; 0 minutes for none.
    #[action(external)]
    pub fn set_maintenance_window(
        &mut self,
        device: Id,
        start: u32,
        minutes: u32,
    ) -> Result<(), Refusal> {
        if start >= MINUTES_PER_DAY || minutes > MINUTES_PER_DAY {
            return Err(Refusal::new(
                "a window starts within a day and lasts at most one",
            ));
        }
        let server = self.server(&device)?;
        server.maintenance_start = start;
        server.maintenance_minutes = minutes;
        Ok(())
    }

    /// Set how a server unlocks after a boot.
    #[action(external)]
    pub fn set_unlock_policy(&mut self, device: Id, policy: String) -> Result<(), Refusal> {
        if !UNLOCK_POLICIES.contains(&policy.as_str()) {
            return Err(Refusal::new("unknown unlock policy"));
        }
        self.server(&device)?.unlock = policy;
        Ok(())
    }

    /// Say which key (public half, PEM) a server's rollback floor takes
    /// approvals from.
    #[action(external)]
    pub fn set_floor_key(&mut self, device: Id, key: String) -> Result<(), Refusal> {
        self.server(&device)?.floor_key = key;
        Ok(())
    }

    /// Ask a server to do an operation, under the id `request` names it by.
    /// `operation` is one of [`ServerOperation::NAMES`]; a `restart` or `logs`
    /// names the systemd unit in `service`, and a `logs` reads `lines` lines
    /// of it.
    #[action(external)]
    pub fn ask_operation(
        &mut self,
        device: Id,
        request: String,
        operation: String,
        service: String,
        lines: u32,
    ) -> Result<(), Refusal> {
        if !ServerOperation::NAMES.contains(&operation.as_str()) {
            return Err(Refusal::new("no such operation"));
        }
        if request.is_empty() || request.len() > MAX_REQUEST || hex::decode(&request).is_err() {
            return Err(Refusal::new("a request is named by its id"));
        }
        let server = self.server(&device)?;
        server.asked.push(
            Id::from_raw(request),
            Asked {
                operation,
                service,
                lines,
                at_ns: tilo_clock::unix_now_ns(),
                answer: String::new(),
            },
        );
        Ok(())
    }

    /// Keep what a server answered the request `request` asked it: one
    /// [`ServerOperationResult`](tilo_server_protocol::transport::ServerOperationResult)
    /// as JSON. A request answered already keeps the answer it had.
    #[action(external)]
    pub fn answer_operation(
        &mut self,
        device: Id,
        request: String,
        answer: String,
    ) -> Result<(), Refusal> {
        serde_json::from_str::<tilo_server_protocol::transport::ServerOperationResult>(&answer)
            .map_err(|_| Refusal::new("an answer is an operation's result"))?;
        let server = self.server(&device)?;
        let request = Id::from_raw(request);
        match server.asked.get_mut(&request) {
            Some(asked) if asked.answer.is_empty() => asked.answer = answer,
            Some(_) => return Ok(()),
            None => return Err(Refusal::new("no such request")),
        }
        // What was asked long ago reads from the newest answers alone; the
        // requests still to do stay.
        let oldest: Vec<Id> = server
            .asked
            .iter()
            .filter(|(_, asked)| !asked.answer.is_empty())
            .map(|(id, _)| id.clone())
            .take(server.asked.len().saturating_sub(HISTORY_KEPT))
            .collect();
        for id in oldest {
            server.asked.remove(&id);
        }
        Ok(())
    }

    /// Set the machine's hostname.
    #[action(external)]
    pub fn set_hostname(&mut self, device: Id, hostname: String) -> Result<(), Refusal> {
        let hostname = device::valid_hostname(&hostname)?;
        self.server(&device)?.device.hostname = hostname;
        Ok(())
    }

    /// Use the drive `identity` (its serial or WWN) as `role`
    /// ([`device::DISK_ROLES`]).
    ///
    /// `confirm` names the drive again to confirm erasing what it holds: a
    /// drive used for something else already is erased to become this one, so
    /// changing its role needs it. A machine has one system disk, and the disk
    /// boots from is never repurposed.
    #[action(external)]
    pub fn use_disk(
        &mut self,
        device: Id,
        identity: String,
        role: String,
        confirm: String,
    ) -> Result<(), Refusal> {
        let identity = device::valid_identity(&identity)?;
        device::valid_role(&role)?;
        let confirm = confirm.trim().to_owned();
        if !confirm.is_empty() && confirm != identity {
            return Err(Refusal::new(
                "confirming a drive is naming its own serial or WWN",
            ));
        }
        let erase = confirm == identity;
        let disk = Id::from_raw(identity);
        let server = self.server(&device)?;
        if let Some(known) = server.device.disks.get(&disk)
            && known.role != role
        {
            if known.role == device::SYSTEM {
                return Err(Refusal::new(
                    "a machine does not repurpose the disk it boots from",
                ));
            }
            if !erase {
                return Err(Refusal::new(
                    "another role erases the drive; confirm it by naming its serial or WWN",
                ));
            }
        }
        if role == device::SYSTEM
            && server
                .device
                .disks
                .iter()
                .any(|(known, held)| held.role == device::SYSTEM && known != &disk)
        {
            return Err(Refusal::new(
                "a machine has one system disk; release the other first",
            ));
        }
        match server.device.disks.get_mut(&disk) {
            Some(held) => {
                held.role = role;
                held.erase = erase;
            }
            None => {
                server.device.disks.push(disk, device::Disk { role, erase });
            }
        }
        Ok(())
    }

    /// Stop using the drive `identity`; the disk boots from stays.
    #[action(external)]
    pub fn release_disk(&mut self, device: Id, identity: String) -> Result<(), Refusal> {
        let disk = Id::from_raw(identity);
        let server = self.server(&device)?;
        if server
            .device
            .disks
            .get(&disk)
            .is_some_and(|held| held.role == device::SYSTEM)
        {
            return Err(Refusal::new("a machine keeps the disk it boots from"));
        }
        server.device.disks.remove(&disk);
        Ok(())
    }

    /// Set up the interface `name`: addressed by the `addresses` it names
    /// (`address/prefix`, comma separated), or by DHCP when it names none;
    /// reached through `gateway`; using `dns` servers (comma separated); a
    /// member of `bond` (empty for none; a member takes its addressing from the
    /// bond). A name ending in `.<vlan>` tags the interface with it.
    #[action(external)]
    pub fn set_interface(
        &mut self,
        device: Id,
        name: String,
        addressing: String,
        addresses: String,
        gateway: String,
        dns: String,
        bond: String,
    ) -> Result<(), Refusal> {
        let name = name.trim().to_owned();
        let interface =
            device::valid_interface(&name, &addressing, &addresses, &gateway, &dns, &bond)?;
        let interface_id = Id::from_raw(name);
        let server = self.server(&device)?;
        if !interface.bond.is_empty()
            && server
                .device
                .network
                .get(&Id::from_raw(interface.bond.clone()))
                .is_none()
        {
            return Err(Refusal::new(
                "a bond member names the interface its bond is set up on",
            ));
        }
        if !interface.bond.is_empty()
            && server
                .device
                .network
                .iter()
                .any(|(known, held)| known != &interface_id && held.bond == interface_id.as_str())
        {
            return Err(Refusal::new(
                "an interface others are bonded to is no member",
            ));
        }
        match server.device.network.get_mut(&interface_id) {
            Some(held) => *held = interface,
            None => {
                server.device.network.push(interface_id, interface);
            }
        }
        Ok(())
    }

    /// Stop configuring the interface `name`; the interfaces bonded to it stay
    /// until they are let go.
    #[action(external)]
    pub fn release_interface(&mut self, device: Id, name: String) -> Result<(), Refusal> {
        let name = name.trim().to_owned();
        let interface = Id::from_raw(name);
        let server = self.server(&device)?;
        if server
            .device
            .network
            .iter()
            .any(|(known, held)| known != &interface && held.bond == interface.as_str())
        {
            return Err(Refusal::new(
                "interfaces are bonded to it; let them go first",
            ));
        }
        server.device.network.remove(&interface);
        Ok(())
    }

    /// Say what a server's machine reported applying its configuration: the
    /// `revision` of the request, an [`REPORT_STATES`](device::REPORT_STATES)
    /// state, and what it applied or why not.
    #[action(external)]
    pub fn report_device_config(
        &mut self,
        device: Id,
        revision: String,
        state: String,
        detail: String,
    ) -> Result<(), Refusal> {
        let status = device::valid_status(&revision, &state, &detail)?;
        self.server(&device)?.device.status = status;
        Ok(())
    }

    /// Ask a server to retire with `revocation`, the revocation of its device
    /// key its person's root signed (JSON): the server takes it, retires its
    /// own entry and lets go of everything it holds.
    #[action(external)]
    pub fn ask_retirement(&mut self, device: Id, revocation: String) -> Result<(), Refusal> {
        let entry = serde_json::from_str::<RevocationEntry>(&revocation)
            .ok()
            .filter(|entry| {
                verify_revocation_entry(entry).is_ok()
                    && device_id(&entry.revoked_device_public_key) == device
            })
            .ok_or_else(|| Refusal::new("a server is retired with its root-signed revocation"))?;
        self.server(&device)?.revocation =
            serde_json::to_string(&entry).map_err(|error| Refusal::new(error.to_string()))?;
        Ok(())
    }

    /// Retire a server asked to retire: it serves nothing from then on, and
    /// every holder refuses its key. The server does this itself once it took
    /// its revocation, or the device that asked does when it is away. Its entry
    /// stays, so its device is never taken in again as a new server.
    #[action(external)]
    pub fn retire_server(&mut self, device: Id) -> Result<(), Refusal> {
        let server = self.server(&device)?;
        if server.revocation.is_empty() {
            return Err(Refusal::new(
                "a server is retired with its root-signed revocation",
            ));
        }
        server.retired = true;
        Ok(())
    }
}

/// A server's id in [`Servers`]: its device key in lowercase hex.
#[must_use]
pub fn device_id(device: &[u8; 32]) -> Id {
    Id::from_raw(hex::encode(device))
}

fn validate_device(device: &Id) -> Result<(), Refusal> {
    let mut key = [0; 32];
    hex::decode_to_slice(device.as_str(), &mut key)
        .ok()
        .filter(|()| device.as_str() == hex::encode(key))
        .ok_or_else(|| Refusal::new("a server is named by its device key"))
}

/// `text` as a 16-byte id in lowercase hex, or `wrong`.
fn validate_hex32(text: &str, wrong: &'static str) -> Result<String, Refusal> {
    (text.len() == 32
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| text.to_owned())
    .ok_or_else(|| Refusal::new(wrong))
}

/// The choices a plan document makes, as the person's servers keep them for a
/// machine that does not exist yet.
struct PendingPlan {
    nonce: String,
    expires_at_ns: u64,
    destination: String,
    network: plan::AddressBy,
    unlock: String,
    updates: String,
    disk: String,
    open: String,
    hosting: Collection<plan::Hosting>,
}

impl PendingPlan {
    fn of(document: &install_profile_model::plan::Plan) -> Self {
        Self {
            nonce: document.token.nonce.clone(),
            expires_at_ns: document.token.expires_at_ns,
            destination: document.destination.clone(),
            network: plan::AddressBy::of(&document.network),
            unlock: document.unlock.clone(),
            updates: document.updates.clone(),
            disk: document.disk.encode(),
            open: document.open_choices().join(","),
            hosting: document
                .hosting
                .iter()
                .map(|held| {
                    (
                        Id::from_raw(held.workspace.clone()),
                        plan::Hosting {
                            mode: held.mode.clone(),
                            handed: false,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// `text` without the choice `name`, comma separated as the entry keeps the
/// list.
fn without_choice(text: &str, name: &str) -> String {
    text.split(',')
        .map(str::trim)
        .filter(|choice| !choice.is_empty() && *choice != name)
        .collect::<Vec<_>>()
        .join(",")
}

/// The drives a machine reported, as its entry keeps them: each named by its
/// identity, bounded and cleaned, so an answer can only name one of them.
fn validated_drives(text: &str) -> Result<Collection<plan::ReportedDrive>, Refusal> {
    let wrong = || Refusal::new("a machine's drives are the ones it reported");
    if text.len() > 64 * 1024 {
        return Err(wrong());
    }
    let reported: Vec<RawDrive> = serde_json::from_str(text).map_err(|_| wrong())?;
    if reported.len() > 64 {
        return Err(wrong());
    }
    let mut drives = Collection::default();
    for drive in reported {
        if drive.identity.is_empty()
            || drive.identity.len() > 128
            || !drive
                .identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._:".contains(&byte))
            || drive.model.len() > 128
            || drive.model.chars().any(char::is_control)
            || drive.bytes == 0
            || drives.contains(&Id::from_raw(drive.identity.clone()))
        {
            return Err(wrong());
        }
        drives.push(
            Id::from_raw(drive.identity.clone()),
            plan::ReportedDrive {
                model: drive.model,
                identity: drive.identity,
                bytes: drive.bytes,
                holds_data: drive.holds_data,
                boots: drive.boots,
            },
        );
    }
    Ok(drives)
}

/// A drive as the machine reports it on enrollment.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDrive {
    #[serde(default)]
    model: String,
    identity: String,
    bytes: u64,
    #[serde(default)]
    holds_data: bool,
    #[serde(default)]
    boots: bool,
}

/// The questions a machine asked, as its entry keeps them, by the choice each
/// settles: a JSON object of choice to the reason the machine could not
/// settle it.
fn validated_questions(text: &str) -> Result<Collection<plan::Asked>, Refusal> {
    let wrong = || Refusal::new("a machine asks about one of its plan's own choices");
    if text.trim().is_empty() {
        return Ok(Collection::default());
    }
    if text.len() > 16 * 1024 {
        return Err(wrong());
    }
    let asked: std::collections::BTreeMap<String, String> =
        serde_json::from_str(text).map_err(|_| wrong())?;
    let mut questions = Collection::default();
    for (choice, reason) in asked {
        if !install_profile_model::plan::OPEN_CHOICES.contains(&choice.as_str())
            || reason.trim().is_empty()
            || reason.len() > 512
            || reason.chars().any(char::is_control)
        {
            return Err(wrong());
        }
        questions.push(Id::from_raw(choice), plan::Asked { reason });
    }
    Ok(questions)
}

/// `endpoint` as a server's address, spelled as its own endpoint advertises
/// it: parsed here so nothing later reads a dial address out of arbitrary
/// text.
fn validate_endpoint(endpoint: &str) -> Result<String, Refusal> {
    let wrong = || Refusal::new("a server's endpoint is its own dial address");
    let address = serde_json::from_str::<iroh_base::EndpointAddr>(endpoint).map_err(|_| wrong())?;
    serde_json::to_string(&address).map_err(|_| wrong())
}

fn validate_label(label: &str) -> Result<String, Refusal> {
    if !tilo_server_protocol::valid_device_label(label) {
        return Err(Refusal::new("a server's name is one line of 1 to 80 bytes"));
    }
    Ok(label.trim().to_owned())
}

/// Apply `action` to the Servers workspace's [`SERVERS`] document as this device,
/// which governs the workspace.
///
/// # Errors
/// The action was refused, or the document could not be written.
pub async fn apply(workspaces: &Workspaces, action: Invoke<Servers>) -> Result<(), String> {
    let admin = person_workspace_id(&workspaces.serving_certificate().user_public_key);
    apply_on(workspaces, &admin, action).await
}

/// [`apply`] on the workspace `workspace`, which is this device's person's
/// Servers workspace.
///
/// # Errors
/// As [`apply`].
pub async fn apply_on(
    workspaces: &Workspaces,
    workspace: &str,
    action: Invoke<Servers>,
) -> Result<(), String> {
    super::person_workspace::apply_on(workspaces, workspace, SERVERS, action).await
}

/// Take the machine whose device key is `device` in as the one the plan `plan` describes: the
/// plan's entry becomes the machine's, under its key, as when it begins its install. What the
/// device that served the machine's ticket does once the person's root certified it.
///
/// # Errors
/// No machine is planned under `plan`, or the document refused the change or could not be
/// written.
pub async fn admit_planned(
    workspaces: &Workspaces,
    plan: &str,
    device: &[u8; 32],
) -> Result<(), String> {
    let nonce = read(workspaces).await?.waiting_plan(plan)?.plan.token.nonce;
    apply(
        workspaces,
        Servers::BEGIN_INSTALL.with((hex::encode(device), nonce, "[]".to_owned(), String::new())),
    )
    .await
}

/// The Servers workspace's [`SERVERS`] document as this device reads it.
///
/// # Errors
/// The document could not be read.
pub async fn read(workspaces: &Workspaces) -> Result<Servers, String> {
    let admin = person_workspace_id(&workspaces.serving_certificate().user_public_key);
    read_on(workspaces, &admin).await
}

/// [`read`] of the workspace `workspace`.
///
/// # Errors
/// As [`read`].
pub async fn read_on(workspaces: &Workspaces, workspace: &str) -> Result<Servers, String> {
    super::person_workspace::read_on::<Servers>(workspaces, workspace, SERVERS).await
}

/// The name the Servers workspace gives the server `device`: what the
/// workspaces it holds show it by. `None` where its entry names it nowhere,
/// and the keeper goes by its device's label.
pub async fn name(workspaces: &Workspaces, device: &[u8; 32]) -> Option<String> {
    read(workspaces)
        .await
        .ok()?
        .active(device)
        .map(|server| server.label.clone())
        .filter(|label| !label.is_empty())
}

/// Refuse, on this device, the key of every server `servers` says was retired:
/// the revocation its person's root signed when it was retired. Every holder of
/// the Servers workspace does this as it applies the document, so the server
/// itself, and every device of its person, refuses it.
///
/// # Errors
/// A revocation could not be kept.
pub async fn refuse_retired(workspaces: &Workspaces, servers: &Servers) -> Result<(), String> {
    let live = workspaces.revocations();
    let entries = servers
        .servers
        .iter()
        .filter(|(_, server)| server.retired)
        .filter_map(|(_, server)| {
            tilo_failure::ignored!(
                "a server retired before its entry carried its revocation has none to refuse",
                serde_json::from_str::<RevocationEntry>(&server.revocation).ok()
            )
        })
        .filter(|entry| {
            !live.is_revoked_at(
                &entry.user_public_key,
                &entry.revoked_device_public_key,
                entry.revoked_at_ns,
            )
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(());
    }
    workspaces.take_revocations(entries).await
}

/// Keep this device's own entry among its person's servers current: its name
/// and `endpoint`, what a server that holds the Servers workspace does on each
/// start, as the address it is reached at may have changed.
///
/// # Errors
/// This device holds no Servers workspace, or the entry could not be written.
pub async fn enroll(
    workspaces: &Workspaces,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    if !holds_person_workspace(workspaces).await {
        return Err("this server holds no Servers workspace yet".into());
    }
    let cert = workspaces.serving_certificate().clone();
    enter(workspaces, &cert, endpoint).await
}

/// Enter the server `server`, dialled at `endpoint`, among this device's
/// person's servers: the devices that have the Servers workspace then hand it
/// the workspace as they hand it every server the document names. A retired
/// server stays retired.
///
/// # Errors
/// `server` is not a device of this device's person that may hold workspaces,
/// this device holds no Servers workspace, or the entry could not be written.
pub async fn admit(
    workspaces: &Workspaces,
    server: &DeviceCert,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    if server.user_public_key != workspaces.serving_certificate().user_public_key
        || !server.capabilities.can_own_workspaces
    {
        return Err("only a server of this person is taken in among their servers".into());
    }
    if !holds_person_workspace(workspaces).await {
        return Err("this server holds no Servers workspace yet".into());
    }
    enter(workspaces, server, endpoint).await
}

/// Hand the device `device`, another of this device's person's devices, the
/// Servers workspace to keep as the servers do, dialled at `endpoint`. A device
/// that keeps it already is handed it afresh, which is how its address is
/// brought up to date.
///
/// # Errors
/// `device` is not a device of this device's person, this device holds no Servers
/// workspace, the device could not be reached, or it refused.
pub async fn take_in(
    workspaces: &Workspaces,
    device: &DeviceCert,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    if device.user_public_key != workspaces.serving_certificate().user_public_key
        || device.capabilities.agent.is_some()
        || device.capabilities.stand_in
    {
        return Err("only a device of this person holds their Servers workspace".into());
    }
    if !holds_person_workspace(workspaces).await {
        return Err("this device holds no Servers workspace yet".into());
    }
    let admin = person_workspace_id(&device.user_public_key);
    workspaces
        .attach_server(
            &admin,
            device.device_public_key,
            endpoint.clone(),
            tilo_peer_state::CopyRole::KeyHolder,
        )
        .await
        .map(drop)
}

/// Enter the device `cert` among the person's servers at `endpoint`; an entry
/// that says so already writes no history.
async fn enter(
    workspaces: &Workspaces,
    cert: &DeviceCert,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    let endpoint = serde_json::to_string(endpoint).map_err(|error| error.to_string())?;
    if read(workspaces)
        .await?
        .servers
        .get(&device_id(&cert.device_public_key))
        .is_some_and(|server| server.endpoint == endpoint)
    {
        return Ok(());
    }
    let action = Servers::ENROLL_SERVER.with((
        hex::encode(cert.device_public_key),
        cert.device_label.clone(),
        endpoint,
    ));
    apply(workspaces, action).await
}

impl super::Workspaces {
    /// Tell whoever follows the workspace's records that this device wrote the
    /// record `record` of `app` in `workspace` itself, as a member's write
    /// tells them ([`super::ResourceWrite`]): a follower that keeps the record
    /// current hears its own changes too.
    ///
    /// This is the one notice an admitted frame and a local write both raise,
    /// from the single point each reaches, so a watcher hears a write once.
    pub(crate) fn wrote(&self, workspace: &str, app: &str, record: &str) {
        let _ = self.inner.document_writes.send(super::ResourceWrite {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests;
