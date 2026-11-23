//! A machine the person prepared but has not installed yet, as its entry in
//! their servers keeps it.
//!
//! The entry exists before the machine does: [`super::Servers::plan_machine`]
//! enters it under the plan's id with the token's nonce, so a medium or a
//! rescue system carrying only the token can be recognised when the machine
//! enrolls ([`super::Servers::begin_install`]), and the rest of its
//! configuration is written before there is a device key to write it under.
//!
//! The plan is secret-free: the nonce identifies the single-use token and the
//! token itself is never kept here, in a log or in an answer.
//!
//! A choice the plan left open, or a disk rule that matched no drive or
//! several on the machine, is never guessed by the machine. It enrolls with
//! the machine's real drives ([`super::Servers::begin_install`]) and the
//! question it cannot settle ([`Asked`]); one of the person's devices answers
//! it here ([`super::Servers::answer_machine_choice`]) and the machine reads
//! its entry back.
//!
//! The entry is the plan's only home: [`super::Server::install_plan`] reads it
//! back as the same document ([`install_profile_model::plan::Plan`]) the
//! machine installed from, so the person's devices, their agents and the
//! machine itself read one copy.

use tilo_resource::{Collection, Model};

/// A machine whose installation is planned, and no disk was written yet.
pub const PLANNED: &str = "planned";
/// A machine that enrolled and is installing.
pub const INSTALLING: &str = "installing";
/// A machine whose installation finished: the entry is the server's from now
/// on, and the plan is what it was installed from.
pub const INSTALLED: &str = "installed";

/// The states a planned machine's entry is in.
pub const STATES: [&str; 4] = ["", PLANNED, INSTALLING, INSTALLED];

/// What a machine at another place should become, and how its installation
/// stands.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Pending {
    /// Its installation's state ([`STATES`]); empty for an entry that is not a
    /// planned machine.
    pub status: String,
    /// The plan's id, as the person's devices and the medium name it.
    pub plan: String,
    /// The ticket's nonce, lowercase hex: single-use, and how the machine is
    /// recognised until it signs in.
    pub nonce: String,
    /// Nanoseconds since the epoch after which its ticket is invalid.
    pub expires_at_ns: u64,
    /// The OS it installs (`tilo-os` or `tilo-server`).
    pub destination: String,
    /// How its network is set up, as the plan said; open when its addressing
    /// is empty.
    pub network: AddressBy,
    /// How it unlocks after a boot (`owner-device`, `tpm` or `passphrase`);
    /// empty when open.
    pub unlock: String,
    /// The updates it follows (`stable` or `dev`); empty when open.
    pub updates: String,
    /// The disk rule (`empty`, `serial:<serial>`, `wwid:<wwn>`, `largest`,
    /// `only`).
    pub disk: String,
    /// The choices the plan leaves open, comma separated, as the person's
    /// devices answer them against the machine's real hardware.
    pub open: String,
    /// How many times the plan changed after a medium was made from it.
    pub revision: u64,
    /// The drives the machine reported when it enrolled, by their identity:
    /// what an answer to its disk question may name, and nothing else.
    pub drives: Collection<ReportedDrive>,
    /// The questions the machine asked and no device answered yet, by the
    /// choice each settles, with the reason the machine could not settle it.
    pub ask: Collection<Asked>,
    /// The workspaces the plan has the machine hold, by workspace id: what a
    /// device governing each names it a holder of once it enrolled. From then
    /// on the workspace's governance says what it holds.
    pub hosting: Collection<Hosting>,
}

/// A workspace a plan has its machine hold, and how.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Hosting {
    /// As an encrypted store or a key holder
    /// ([`install_profile_model::plan::HOLDING_MODES`]).
    pub mode: String,
    /// The workspace's governance names the machine: it was handed over, and
    /// its governance alone says what the machine holds from then on.
    pub handed: bool,
}

/// How the plan said the machine's network is set up, as its entry keeps it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct AddressBy {
    /// `dhcp` or `static`; empty when the plan left the network open.
    pub addressing: String,
    /// `address/prefix` entries, comma separated, for static addressing.
    pub addresses: String,
    /// The default gateways, comma separated: at most one for each address
    /// family; empty for none.
    pub gateway: String,
    /// DNS servers, comma separated; empty for none.
    pub dns: String,
}

/// A drive a machine that enrolled really has, as it reported it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ReportedDrive {
    /// The drive's model, for the person to recognise it by.
    pub model: String,
    /// Its serial or WWN: what an answer names it by.
    pub identity: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Whether it already holds files.
    pub holds_data: bool,
    /// Whether it holds the system the machine booted from.
    pub boots: bool,
}

/// A choice a machine asked the person's devices to settle, and why.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Asked {
    /// Why the machine could not settle the choice itself, for the person to
    /// read: the rule that matched no drive or several, or a choice the plan
    /// left open.
    pub reason: String,
}

impl Pending {
    /// Whether this entry is a machine waiting to be installed.
    #[must_use]
    pub fn planned(&self) -> bool {
        matches!(self.status.as_str(), PLANNED | INSTALLING)
    }

    /// The choices left open, by name.
    #[must_use]
    pub fn open_choices(&self) -> Vec<&str> {
        self.open
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// The drive the machine reported as `identity`, when it reported one.
    #[must_use]
    pub fn drive(&self, identity: &str) -> Option<&ReportedDrive> {
        self.drives.get(&tilo_resource::Id::from_raw(identity))
    }

    /// The question the machine asked about `choice`, when it is unanswered.
    #[must_use]
    pub fn question(&self, choice: &str) -> Option<&Asked> {
        self.ask.get(&tilo_resource::Id::from_raw(choice))
    }

    /// The choices this entry still leaves open, by
    /// [`install_profile_model::plan::OPEN_CHOICES`] name: what the machine
    /// asked and no device answered, or what it settled itself.
    #[must_use]
    pub fn still_open(&self) -> Vec<&str> {
        install_profile_model::plan::OPEN_CHOICES
            .iter()
            .copied()
            .filter(|choice| self.open_choices().contains(choice))
            .collect()
    }

    /// The plan document this entry keeps, as `label` names the machine: the
    /// same document the machine installs from and the person's devices
    /// planned.
    ///
    /// # Errors
    /// The entry does not read back as a plan, because a value it keeps is
    /// not one a plan takes.
    pub fn document(&self, label: &str) -> Result<install_profile_model::plan::Plan, String> {
        use install_profile_model::plan;
        let open: Vec<String> = self
            .still_open()
            .iter()
            .map(|choice| (*choice).to_owned())
            .collect();
        // A plan that left the name open has none; the label is what the
        // person's list shows it by until one of their devices answers.
        let name = if open.iter().any(|choice| choice == "name") {
            String::new()
        } else {
            label.to_owned()
        };
        let document = plan::Plan {
            schema: plan::SCHEMA,
            plan: self.plan.clone(),
            revision: self.revision,
            destination: self.destination.clone(),
            name,
            network: plan::Network {
                addressing: self.network.addressing.clone(),
                addresses: self.network.addresses.clone(),
                gateway: self.network.gateway.clone(),
                dns: self.network.dns.clone(),
            },
            disk: plan::DiskRule::decode(&self.disk).map_err(str::to_owned)?,
            unlock: self.unlock.clone(),
            updates: self.updates.clone(),
            hosting: self
                .hosting
                .iter()
                .map(|(workspace, hosting)| plan::Holding {
                    workspace: workspace.as_str().to_owned(),
                    mode: hosting.mode.clone(),
                })
                .collect(),
            open,
            token: plan::Token {
                nonce: self.nonce.clone(),
                expires_at_ns: self.expires_at_ns,
            },
        };
        // Read back through the one rule the machine reads it with, so an
        // entry that would not install is refused here and not there.
        serde_json::from_value(serde_json::to_value(&document).map_err(|error| error.to_string())?)
            .map_err(|_| "the entry does not read back as a plan".to_owned())
    }
}

impl AddressBy {
    /// The plan's network choice, as its entry keeps it.
    #[must_use]
    pub fn of(network: &install_profile_model::plan::Network) -> Self {
        Self {
            addressing: network.addressing.clone(),
            addresses: network.addresses.clone(),
            gateway: network.gateway.clone(),
            dns: network.dns.clone(),
        }
    }
}
