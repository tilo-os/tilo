//! A hosted workspace's Terminal session on a terminal, carried to the person's device.
//!
//! The person's device asks the session's `open-terminal`
//! (`tilo_agent_session::workspace_actions`, under the admission `run` has); the host's shells
//! ([`HostShells`]) start the session's shell on a terminal and hand its stream to that
//! device. The device then dials this host on [`ALPN`](ush::environment_terminals::ALPN) as
//! itself and presents the stream's one-use id ([`wire::Hello`]): a stream handed to another
//! device is refused there.
//!
//! - Shells that run here ([`ShellsAt::Here`]) carry the stream themselves.
//! - Shells on the member device that holds the workspace's VM ([`ShellsAt::Placed`]): the
//!   member opens the shell ([`environment_terminals`]) and hands its stream to this host's
//!   own device, which dials the member's terminal streams and splices the two. The person's
//!   device never reaches the member, and the member serves nobody but this host.
//!
//! Each command line the session's shell runs is recorded on the session by this host
//! (`Recorder`), wherever the shell runs: shells here tell it as each run ends; a member hands
//! this host the session's runs on a stream of their own, which this host follows from the
//! open that started the shell until the session ends, whether a device is attached or not.

use std::io;
use std::sync::{Arc, OnceLock};

use iroh::EndpointAddr;
use tilo_actions::remote::environment_commands::VmSecret;
use tilo_actions::remote::environment_terminals::{self, OpenTerminal, TerminalOpened};
use tilo_agent_session::workspace_actions::{Opened, Opening, Recorder, ShellRan};
use ush::environment_terminals::quic::{Receiving, Route, Sending, dial};
use ush::environment_terminals::{HANDED_FOR, Handed, NOT_HANDED, Ran, wire};

use crate::agents::{HostShells, ShellsAt};
use crate::placed_shells::{PlacedShells, member_address};

/// The shells whose terminal streams a host serves on [`ALPN`](ush::environment_terminals::ALPN), once they run.
#[derive(Clone, Default)]
pub struct TerminalShells(Arc<OnceLock<Arc<HostShells>>>);

impl TerminalShells {
    /// Serve the terminal streams `shells` hand out.
    pub fn serve_for(&self, shells: Arc<HostShells>) {
        let _ = self.0.set(shells);
    }

    /// What answers [`ALPN`](ush::environment_terminals::ALPN) on the host's endpoint.
    #[must_use]
    pub fn route(&self) -> Route {
        let shells = Arc::clone(&self.0);
        Route::new(move |device, from, to| {
            let served = match shells.get() {
                Some(shells) => serve(shells, device, from, to),
                None => Err(wire::refuse(to, "this host runs no workspace shells")),
            };
            if let Err(error) = served {
                tracing::debug!(%error, "a terminal stream ended in error");
            }
        })
    }
}

/// A stream the member holding a workspace's VM handed this host's own device, handed on
/// to the person's device that opened it.
pub struct OnMember {
    address: EndpointAddr,
    stream: String,
}

/// The streams a host hands out for the shells its members run.
pub type MemberStreams = Handed<OnMember>;

/// The streams a host hands out for the shells its members run, each waiting
/// [`HANDED_FOR`] for its device.
#[must_use]
pub fn member_streams() -> MemberStreams {
    Handed::new(HANDED_FOR)
}

/// Carry the stream the device `device` presents on `from`: one `shells` handed it, which
/// they carry here or splice to the member that runs the shell.
fn serve(
    shells: &HostShells,
    device: [u8; 32],
    from: Receiving,
    to: &mut Sending,
) -> io::Result<()> {
    match shells.shells() {
        ShellsAt::Here(terminals, _) => terminals.serve_handed(device, from, to).map(drop),
        ShellsAt::Placed(placed) => splice(placed, device, from, to),
    }
}

/// Splice the stream `device` presents to the member that holds it, as this host's device.
fn splice(
    placed: &PlacedShells,
    device: [u8; 32],
    mut from: Receiving,
    to: &mut Sending,
) -> io::Result<()> {
    let Some(hello) = wire::read_hello(&mut from)? else {
        return Ok(());
    };
    let Some(on_member) = placed.terminals().take(&hello.stream, &device) else {
        return Err(wire::refuse(to, NOT_HANDED));
    };
    let (_, endpoint) = placed.asking().map_err(|error| wire::refuse(to, &error))?;
    let runtime = tokio::runtime::Handle::current();
    let (connection, mut member_to, mut member_from) = dial(endpoint, on_member.address, &runtime)
        .map_err(|error| wire::refuse(to, &format!("the VM host is not reached: {error}")))?;
    wire::write_hello(
        &mut member_to,
        &wire::Hello {
            stream: on_member.stream,
        },
    )?;
    // What the person types goes on to the member until they leave; what the member says,
    // its answer to the hello first, comes back until the shell is over there.
    std::thread::Builder::new()
        .name("terminal-splice".into())
        .spawn(move || {
            let _ = io::copy(&mut from, &mut member_to);
            member_to.finish();
        })?;
    let carried = io::copy(&mut member_from, to);
    drop(connection);
    carried.map(drop)
}

impl PlacedShells {
    /// Open `opening`'s session on a terminal on the member that holds its workspace's VM,
    /// as this host's device, every program its shell starts in the VM given `secrets`, and
    /// hand the stream on to the device that asked.
    ///
    /// # Errors
    ///
    /// The placer or the member could not be asked, or the member refused.
    pub fn open_terminal(
        &self,
        opening: &Opening,
        secrets: Vec<VmSecret>,
        record: Recorder,
    ) -> Result<Opened, String> {
        // A host not running yet asks its placer nothing.
        self.asking()?;
        let mut waited = None;
        let host = self
            .placed(&opening.workspace, &mut waited, None)?
            .ok_or_else(|| "the workspace's VM was not placed".to_owned())?;
        let member = self.members().get(&host).ok_or_else(|| {
            format!("the placer named {host}, which is no VM host this host knows")
        })?;
        let address = member_address(member)?;
        let device = self.member_device(&address, &opening.workspace)?;
        let asked = OpenTerminal {
            session: opening.session.clone(),
            cols: opening.cols,
            rows: opening.rows,
            attach: opening.attach.clone(),
            person: opening.person.clone(),
            account: self.account().to_owned(),
            size: self.size().as_str().to_owned(),
            secrets,
        };
        let opened: TerminalOpened =
            crate::placed_machines::follow(&device, environment_terminals::target(), &asked)?
                .and_then(|value| serde_json::from_value(value).ok())
                .ok_or_else(|| "the VM host answered no terminal".to_owned())?;
        if let Some(runs) = opened.runs {
            self.follow_runs(address.clone(), runs, record)?;
        }
        let stream = self
            .terminals()
            .hand(
                opening.device,
                OnMember {
                    address,
                    stream: opened.stream,
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(Opened {
            stream,
            session: opened.session,
        })
    }

    /// Follow the runs the member at `address` handed this host as `runs`, recording each
    /// with `record`, until the session ends there.
    fn follow_runs(
        &self,
        address: EndpointAddr,
        runs: String,
        record: Recorder,
    ) -> Result<(), String> {
        let (_, endpoint) = self.asking()?;
        let (endpoint, runtime) = (endpoint.clone(), self.runtime()?.clone());
        std::thread::Builder::new()
            .name("terminal-runs".into())
            .spawn(move || {
                let followed = (|| {
                    let (_connection, mut to, mut from) = dial(&endpoint, address, &runtime)?;
                    wire::write_hello(&mut to, &wire::Hello { stream: runs })?;
                    match wire::read_welcome(&mut from)? {
                        Some(wire::Welcome::Opened { .. }) => {}
                        other => return Err(io::Error::other(format!("{other:?}"))),
                    }
                    while let Some(ran) = wire::read_ran(&mut from)? {
                        record(shell_ran(ran));
                    }
                    Ok::<_, io::Error>(())
                })();
                if let Err(error) = followed {
                    tracing::warn!(%error, "a terminal's runs on the VM host stopped being followed");
                }
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// A run a session's shell told of, as a command the host records.
#[must_use]
pub fn shell_ran(ran: Ran) -> ShellRan {
    ShellRan {
        command: ran.command,
        cwd: ran.cwd,
        exit: ran.exit,
        output: ran.output,
        truncated: ran.truncated,
        cancelled: false,
        started_ms: ran.started_ms,
        ended_ms: ran.ended_ms,
        refusal: None,
    }
}
