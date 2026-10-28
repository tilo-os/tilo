//! The helper itself: who may ask ([`peer`]), what it does ([`plan`]), the
//! socket loop ([`server`]) and the machine it does it on (`linux`).
//!
//! The service keeps one client tunnel, one exit and one block of the
//! person's VPN profiles per machine, each owned by the person whose shell
//! asked for it, and remembers them in a record that outlives the process,
//! so a restarted helper still undoes what an earlier one set up.

pub mod peer;
pub mod plan;
pub mod record;
pub mod server;

#[cfg(target_os = "linux")]
pub mod linux;

use std::os::fd::OwnedFd;

use crate::net::{CLIENT_INTERFACE, EXIT_INTERFACE, MAX_MTU};
use crate::protocol::{
    ClientLink, ClientState, ExitLink, ExitState, ProfileBlock, Request, Response,
};
use plan::Command;
use record::{ClientRecord, ExitRecord, ProfileBlockRecord, Record};

/// A caller the service answers: the compositor of the person at the seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
}

/// What the service needs of the machine.
pub trait Backend {
    /// Create the TUN `name` and hand over its only descriptor.
    fn create_tun(&mut self, name: &str) -> Result<OwnedFd, String>;
    /// Run `command`; a failure of a required one is an error.
    fn run(&mut self, command: &Command) -> Result<(), String>;
    fn interface_exists(&self, name: &str) -> bool;
    /// Whether the person `uid` still has a session.
    fn has_session(&self, uid: u32) -> bool;
    /// Whether IPv4 forwarding is on.
    fn forwarding(&self) -> Result<bool, String>;
    fn set_forwarding(&mut self, on: bool) -> Result<(), String>;
    /// Keep `record` where the next helper finds it.
    fn save(&mut self, record: &Record) -> Result<(), String>;
}

pub struct Service<B> {
    backend: B,
    record: Record,
}

impl<B: Backend> Service<B> {
    /// A service over `backend` that picks up what `record` says an earlier
    /// helper left.
    pub fn new(backend: B, record: Record) -> Self {
        Self { backend, record }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn record(&self) -> &Record {
        &self.record
    }

    /// Whether nothing is open, so the process may exit. A profile block
    /// needs no process: its table stays in the kernel and its record in
    /// the record file until it is released or the machine boots.
    pub fn is_idle(&self) -> bool {
        self.record.client.is_none() && self.record.exit.is_none()
    }

    /// Answer `request` from `caller`, with the TUN descriptor of a tunnel
    /// it opened.
    pub fn handle(&mut self, caller: Caller, request: Request) -> (Response, Option<OwnedFd>) {
        match request {
            Request::Status => (self.status(), None),
            Request::OpenClient { link } => match self.open_client(caller, &link) {
                Ok(fd) => (
                    Response::Opened {
                        interface: CLIENT_INTERFACE.into(),
                        mtu: self.record.client.as_ref().map_or(MAX_MTU, |c| c.link.mtu),
                    },
                    Some(fd),
                ),
                Err(response) => (response, None),
            },
            Request::CloseClient => (self.close_client(caller), None),
            Request::OpenExit { link } => match self.open_exit(caller, &link) {
                Ok(fd) => (
                    Response::Opened {
                        interface: EXIT_INTERFACE.into(),
                        mtu: self.record.exit.as_ref().map_or(MAX_MTU, |e| e.link.mtu),
                    },
                    Some(fd),
                ),
                Err(response) => (response, None),
            },
            Request::CloseExit => (self.close_exit(caller), None),
            Request::HoldProfileBlock { block } => (self.hold_profile_block(caller, block), None),
            Request::ReleaseProfileBlock => (self.release_profile_block(caller), None),
        }
    }

    fn status(&self) -> Response {
        Response::Status {
            client: self.record.client.as_ref().map(|client| ClientState {
                link: client.link.clone(),
                interface_up: self.backend.interface_exists(CLIENT_INTERFACE),
            }),
            exit: self.record.exit.as_ref().map(|exit| ExitState {
                link: exit.link.clone(),
                interface_up: self.backend.interface_exists(EXIT_INTERFACE),
            }),
            profile_block: self
                .record
                .profile_block
                .as_ref()
                .map(|held| held.block.clone()),
        }
    }

    /// Refuse a caller other than the person who owns what is open, while
    /// that person still has a session.
    fn owned_by_another(&self, owner: Option<u32>, caller: Caller) -> Option<Response> {
        owner
            .filter(|owner| *owner != caller.uid && self.backend.has_session(*owner))
            .map(|_| Response::Refused {
                reason: "another person on this computer is using this".into(),
            })
    }

    fn save(&mut self) {
        if let Err(error) = self.backend.save(&self.record) {
            eprintln!("tilo-device-vpn: keeping the record: {error}");
        }
    }

    fn run_all(&mut self, commands: &[Command]) -> Result<(), String> {
        for command in commands {
            self.backend.run(command)?;
        }
        Ok(())
    }

    fn open_client(&mut self, caller: Caller, link: &ClientLink) -> Result<OwnedFd, Response> {
        if let Some(refused) =
            self.owned_by_another(self.record.client.as_ref().map(|c| c.owner), caller)
        {
            return Err(refused);
        }
        link.check()
            .map_err(|reason| Response::Refused { reason })?;
        let previous = self.record.client.clone();
        // A replaced tunnel's TUN may still exist: the new one takes its name.
        if previous.is_some() {
            let _ = self.backend.run(&plan_delete_link(CLIENT_INTERFACE));
        }
        let fd = self
            .backend
            .create_tun(CLIENT_INTERFACE)
            .map_err(|message| Response::Failed { message })?;
        let commands = plan::open_client(
            caller.uid,
            link,
            previous
                .as_ref()
                .map(|previous| (previous.owner, &previous.link)),
        );
        // From here on the record names what may have been set up, so a
        // failure part way is undone by `close_client`.
        let dns_set = previous.as_ref().is_some_and(|previous| previous.dns_set);
        self.record.client = Some(ClientRecord {
            owner: caller.uid,
            link: link.clone(),
            dns_set,
        });
        self.save();
        if let Err(message) = self.run_all(&commands) {
            self.undo_client();
            return Err(Response::Failed { message });
        }
        if link.dns != dns_set {
            if let Err(message) = self.backend.run(&plan::global_dns(link.dns)) {
                self.undo_client();
                return Err(Response::Failed { message });
            }
            if let Some(client) = self.record.client.as_mut() {
                client.dns_set = link.dns;
            }
            self.save();
        }
        Ok(fd)
    }

    fn close_client(&mut self, caller: Caller) -> Response {
        if let Some(refused) =
            self.owned_by_another(self.record.client.as_ref().map(|c| c.owner), caller)
        {
            return refused;
        }
        self.undo_client();
        Response::Done
    }

    /// Take the client tunnel down entirely, whatever state it is in.
    fn undo_client(&mut self) {
        let Some(client) = self.record.client.clone() else {
            return;
        };
        for command in plan::close_client(client.owner, &client.link) {
            let _ = self.backend.run(&command);
        }
        if client.dns_set
            && let Err(error) = self.backend.run(&plan::global_dns(false))
        {
            eprintln!("tilo-device-vpn: putting DNS back: {error}");
        }
        self.record.client = None;
        self.save();
    }

    fn open_exit(&mut self, caller: Caller, link: &ExitLink) -> Result<OwnedFd, Response> {
        if let Some(refused) =
            self.owned_by_another(self.record.exit.as_ref().map(|e| e.owner), caller)
        {
            return Err(refused);
        }
        link.check()
            .map_err(|reason| Response::Refused { reason })?;
        if self.record.exit.is_some() {
            self.undo_exit();
        }
        let fd = self
            .backend
            .create_tun(EXIT_INTERFACE)
            .map_err(|message| Response::Failed { message })?;
        let was_forwarding = self
            .backend
            .forwarding()
            .map_err(|message| Response::Failed { message })?;
        self.record.exit = Some(ExitRecord {
            owner: caller.uid,
            link: link.clone(),
            enabled_forwarding: !was_forwarding,
        });
        self.save();
        let result = self
            .run_all(&plan::open_exit(link, !was_forwarding))
            .and_then(|()| {
                if was_forwarding {
                    Ok(())
                } else {
                    self.backend.set_forwarding(true)
                }
            });
        if let Err(message) = result {
            self.undo_exit();
            return Err(Response::Failed { message });
        }
        Ok(fd)
    }

    fn close_exit(&mut self, caller: Caller) -> Response {
        if let Some(refused) =
            self.owned_by_another(self.record.exit.as_ref().map(|e| e.owner), caller)
        {
            return refused;
        }
        self.undo_exit();
        Response::Done
    }

    fn undo_exit(&mut self) {
        let Some(exit) = self.record.exit.clone() else {
            return;
        };
        for command in plan::close_exit() {
            let _ = self.backend.run(&command);
        }
        if exit.enabled_forwarding
            && let Err(error) = self.backend.set_forwarding(false)
        {
            eprintln!("tilo-device-vpn: turning forwarding back off: {error}");
        }
        self.record.exit = None;
        self.save();
    }

    /// Put up the block of the person's VPN profiles, or replace the one
    /// that holds. A block that fails part way is lifted again.
    fn hold_profile_block(&mut self, caller: Caller, block: ProfileBlock) -> Response {
        if let Some(refused) = self.owned_by_another(
            self.record.profile_block.as_ref().map(|held| held.owner),
            caller,
        ) {
            return refused;
        }
        if let Err(reason) = block.check() {
            return Response::Refused { reason };
        }
        let commands = plan::hold_profile_block(&block);
        // Recorded before it is put up, so a helper that dies part way still
        // knows to lift it.
        self.record.profile_block = Some(ProfileBlockRecord {
            owner: caller.uid,
            block,
        });
        self.save();
        if let Err(message) = self.run_all(&commands) {
            self.undo_profile_block();
            return Response::Failed { message };
        }
        Response::Done
    }

    fn release_profile_block(&mut self, caller: Caller) -> Response {
        if let Some(refused) = self.owned_by_another(
            self.record.profile_block.as_ref().map(|held| held.owner),
            caller,
        ) {
            return refused;
        }
        self.undo_profile_block();
        Response::Done
    }

    fn undo_profile_block(&mut self) {
        if self.record.profile_block.is_none() {
            return;
        }
        for command in plan::release_profile_block() {
            let _ = self.backend.run(&command);
        }
        self.record.profile_block = None;
        self.save();
    }

    /// Undo what lost its reason: a tunnel whose owner's session ended, an
    /// exit whose TUN is gone, a client whose TUN is gone and that has no
    /// kill switch to keep. A profile block stays when its owner's session
    /// ends: a shell that crashed takes the session with it, and the block
    /// exists for exactly that moment. Its owner's next shell adopts it,
    /// and once the owner is logged out the person at the seat may lift it.
    pub fn housekeeping(&mut self) {
        if let Some(client) = &self.record.client {
            let owner_gone = !self.backend.has_session(client.owner);
            let tunnel_gone = !self.backend.interface_exists(CLIENT_INTERFACE);
            if owner_gone || (tunnel_gone && !client.link.kill_switch) {
                self.undo_client();
            }
        }
        if let Some(exit) = &self.record.exit
            && (!self.backend.has_session(exit.owner)
                || !self.backend.interface_exists(EXIT_INTERFACE))
        {
            self.undo_exit();
        }
    }

    /// Undo everything an earlier boot left (`tilo-device-vpn-reset.service`),
    /// when nobody is left to want what it set up.
    pub fn reset(&mut self) {
        self.undo_client();
        self.undo_exit();
        self.undo_profile_block();
    }
}

fn plan_delete_link(name: &str) -> Command {
    Command {
        program: plan::Program::Ip,
        args: vec!["link".into(), "delete".into(), name.into()],
        input: None,
        required: false,
    }
}

#[cfg(test)]
mod tests;
