//! The machine's own side of a planned installation: what it reports when it
//! enrolls, what it waits for, and when it installs.
//!
//! A machine that enrolled with a plan's token is one of the person's devices,
//! and so holds their Servers workspace. It reads its plan from its own entry
//! ([`super::Server::install_plan`]) and reports the drives it really has and
//! the choices it cannot settle ([`Report`]); one of the person's other
//! devices answers them in that entry, and the machine installs with what the
//! entry says ([`Step::Install`]) and says it finished
//! ([`super::Servers::finish_install`]).
//!
//! Nothing here guesses a choice the plan left to the person. A disk rule that
//! matches exactly one of the machine's drives is settled here, because that
//! is what the rule is for; every other open choice is asked.

use install_profile_model::plan::{self, Choice, Drive, Plan};

use super::{Server, Servers};

/// What a machine reports when it enrolls: the drives it has, as
/// [`servers::begin_install`](super::Servers::begin_install) takes them, and
/// the questions it cannot settle, choice to the reason for the person to
/// read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The machine's real drives, as JSON.
    pub drives: String,
    /// The choices it asks about, as JSON; empty when it settles all of them.
    pub ask: String,
}

/// What a machine does next, read from its waiting entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// It has not enrolled yet: it reports `report` with the token `nonce`
    /// ([`super::Servers::begin_install`]).
    Enlist {
        /// The token's nonce, as its entry keeps it.
        nonce: String,
        /// What it reports along with enrolling.
        report: Report,
    },
    /// The person's devices have not answered these choices yet; it reads the
    /// entry again after a while.
    Wait(Vec<String>),
    /// Every choice is answered: it installs with this plan, then says it
    /// finished.
    Install(Box<Plan>),
    /// It installed; the entry is its server's from now on.
    Finished,
}

/// The machine whose token is named by `nonce`, as this device's copy of the
/// person's Servers workspace keeps it.
#[must_use]
pub fn waiting<'a>(servers: &'a Servers, nonce: &str) -> Option<&'a Server> {
    servers.planned_by_nonce(nonce).map(|(_, server)| server)
}

/// The choices a machine asks about: each is one of the person's devices
/// answering in its entry ([`super::Servers::answer_machine_choice`]). Its
/// network is set up on its entry instead, and the workspaces it holds in each
/// workspace, so a
/// machine with those open installs with what its entry says and asks
/// nothing.
const ASKABLE: [&str; 5] = ["destination", "name", "disk", "unlock", "updates"];

/// What the machine with `drives` reports when it enrolls: its real drives,
/// and the choices `server`'s plan leaves it unable to settle.
///
/// # Errors
/// The machine cannot write out its drives or its questions.
pub fn report(server: &Server, drives: &[Drive]) -> Result<Report, String> {
    let mut asked = std::collections::BTreeMap::new();
    if let Choice::Open { choice, reason } = plan::choose(&encoded_rule(server), drives) {
        asked.insert(choice, reason);
    }
    for choice in server.plan.still_open() {
        // A disk rule that settled exactly one drive answers the disk here;
        // every other askable choice is the person's.
        if !ASKABLE.contains(&choice) || (choice == "disk" && !asked.contains_key("disk")) {
            continue;
        }
        asked
            .entry(choice)
            .or_insert_with(|| reason_left_open(choice).to_owned());
    }
    let drives = serde_json::to_string(drives)
        .map_err(|error| format!("this machine cannot report its drives: {error}"))?;
    let ask = if asked.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&asked)
            .map_err(|error| format!("this machine cannot ask its questions: {error}"))?
    };
    Ok(Report { drives, ask })
}

/// What `server`'s machine does next, with `drives` being what it really has.
///
/// # Errors
/// The entry is no machine waiting to install.
pub fn step(server: &Server, drives: &[Drive]) -> Result<Step, String> {
    if server.plan.status == super::plan::PLANNED {
        return Ok(Step::Enlist {
            nonce: server.plan.nonce.clone(),
            report: report(server, drives)?,
        });
    }
    if server.plan.status == super::plan::INSTALLED {
        return Ok(Step::Finished);
    }
    if server.plan.status != super::plan::INSTALLING {
        return Err("that entry is no machine waiting to install".to_owned());
    }
    let asked: Vec<String> = server
        .plan
        .ask
        .iter()
        .map(|(choice, _)| choice.as_str().to_owned())
        .collect();
    if !asked.is_empty() {
        return Ok(Step::Wait(asked));
    }
    server.install_plan().map(Box::new).map(Step::Install)
}

/// Why the plan leaves `choice` to the person, as its machine tells them.
fn reason_left_open(choice: &str) -> &'static str {
    match choice {
        "destination" => "the plan leaves the OS it installs open",
        "name" => "the plan leaves the machine's name open",
        "disk" => "the plan leaves the machine's disk open",
        "unlock" => "the plan leaves how the machine unlocks open",
        "updates" => "the plan leaves the updates it follows open",
        _ => "the plan leaves this choice open",
    }
}

/// The disk rule `server`'s plan was made with, as a rule that matches nothing
/// when the plan names none or names one this build cannot read: the machine
/// asks either way, rather than picking a drive.
fn encoded_rule(server: &Server) -> plan::DiskRule {
    tilo_failure::ignored!(
        "a disk rule this build cannot read matches no drive, so the machine asks",
        plan::DiskRule::decode(&server.plan.disk).unwrap_or_default()
    )
}
