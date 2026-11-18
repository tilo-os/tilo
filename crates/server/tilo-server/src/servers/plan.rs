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

use tilo_resource::Model;

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
    /// The enrollment token's nonce, lowercase hex: single-use, and how the
    /// machine is recognised until it enrolls.
    pub nonce: String,
    /// Nanoseconds since the epoch after which its token is invalid.
    pub expires_at_ns: u64,
    /// The OS it installs (`tilo-os` or `tilo-server`).
    pub destination: String,
    /// The disk rule (`empty`, `serial:<serial>`, `wwid:<wwn>`, `largest`,
    /// `only`).
    pub disk: String,
    /// The choices the plan leaves open, comma separated, as the person's
    /// devices answer them against the machine's real hardware.
    pub open: String,
    /// How many times the plan changed after a medium was made from it.
    pub revision: u64,
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
}
