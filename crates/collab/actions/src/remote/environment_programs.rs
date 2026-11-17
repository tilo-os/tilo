//! A program started in a workspace's machine and served by the workspace's server, on the
//! member device that takes the workspace's work (`docs/app-model/environments.md`,
//! "Members that take work").
//!
//! A coding agent's program needs what only the server may hold: its person's model and the
//! workspace's actions. Where the workspace's machine is on another computer than the
//! server, the server asks that computer's member device to start the program there, and
//! carries its frames both ways over the device action service, as the device it is:
//!
//! - [`serve_target`] starts one: the argument is a [`ServeProgram`]. The invocation lasts as
//!   long as the program does. Each [`InvocationUpdate::Item`](crate::InvocationUpdate) is a
//!   [`Frames`]: every frame the program sent that the server has not taken yet, numbered, so
//!   a follower that missed an update loses nothing. It succeeds with a [`ProgramEnded`],
//!   however the program ended. A cancel of the invocation ends the program at once.
//! - [`tell_target`] hands the program its next frames ([`TellProgram`]) and says how many of
//!   the program's own frames the server has taken, which the member then lets go of.
//!
//! A frame is one message of the guest agent's serving mode, in that protocol's own bytes
//! written as hex: this module names no frame, so only the two ends agree on them.
//!
//! Only the workspace's server may ask, as a device of the operator the member's own identity
//! belongs to, and only for the workspace it asks as: a program of one workspace is told,
//! followed and ended by nobody who asks as another. A member that hears nothing of a
//! program's server any more ends the program.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::server_account::Refusal;
use crate::InvocationId;
use crate::api::ActionAddress;

/// The provider the member answers a workspace's programs under: the one its commands are.
pub const PROVIDER: &str = super::environment_commands::PROVIDER;
/// The resource a served program is an action of: the workspace's environment.
pub const RESOURCE: &str = super::environment_commands::RESOURCE;
/// Start a program in the workspace's machine and serve it.
pub const SERVE: &str = "serve-program";
/// Hand a served program its next frames.
pub const TELL: &str = "tell-program";

/// What [`SERVE`] is invoked on.
#[must_use]
pub fn serve_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, SERVE),
    }
}

/// What [`TELL`] is invoked on.
#[must_use]
pub fn tell_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, TELL),
    }
}

/// A program to start in the machine of the workspace asked in, served by its server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServeProgram {
    /// The program and its arguments, as the machine finds them.
    pub argv: Vec<String>,
    /// Variables beside the ones every program in the machine starts with.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Where it starts: a folder of the workspace's files when relative, a path of the
    /// machine's own when absolute.
    pub directory: String,
    /// The endpoints the program asks its server at, by name.
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// How long the program is given to end once asked, in milliseconds.
    pub stop_grace_ms: u64,
    /// The agent program in the machine that launches it.
    pub agent_program: String,
    /// Who pays for the workspace's machine, as the server knows it.
    pub account: String,
    /// The size of the workspace's machine (`small`, `default`, `large`, `xlarge`).
    pub size: String,
}

/// Frames in order, each as hex of its own bytes, the first numbered `first`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frames {
    /// The number of the first frame here, counted from the program's first.
    pub first: u64,
    pub frames: Vec<String>,
}

/// How a served program ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramEnded {
    /// What it sent that its server had not taken yet.
    pub last: Frames,
    /// The launcher's exit status, as a shell reads one.
    pub exit: i32,
    /// Why the machine was not given, typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// What was said beside the frames.
    #[serde(default)]
    pub words: String,
}

/// The next frames for a served program, and how far its server has read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TellProgram {
    /// The invocation that started the program.
    pub program: InvocationId,
    /// The number of the first frame here, counted from the first the server ever sent it: a
    /// frame the program was handed already is not handed to it twice.
    pub first: u64,
    #[serde(default)]
    pub frames: Vec<String>,
    /// How many of the program's own frames the server has taken.
    pub taken: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_member_is_asked_reads_back_as_it_was_written() {
        let asked = TellProgram {
            program: InvocationId::new(),
            first: 3,
            frames: vec!["00ff".into()],
            taken: 9,
        };
        let written = serde_json::to_value(&asked).unwrap();
        assert_eq!(
            serde_json::from_value::<TellProgram>(written).unwrap(),
            asked
        );
        assert_eq!(serve_target().address, ActionAddress::new(RESOURCE, SERVE));
        assert_eq!(tell_target().address, ActionAddress::new(RESOURCE, TELL));
    }
}
