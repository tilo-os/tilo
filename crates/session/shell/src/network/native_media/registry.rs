use super::{Arc, CallTarget, Command, NativeWorkspaceOwner, Operation, Reply, Ticket, Weak};
use crate::network::actor::{HostIdentity, native_ingress::NOT_CONNECTED};
use crate::shell::{native_workspace_owner::NativeCommittedView, state::RoomId};
use tilo_joined_room::voice::Channel;
use tilo_relay::coedit::{RelayCoeditSession, VoiceEvent};

/// The relay session of a joined room's workspace, when it is connected.
pub(crate) type Sessions<'a> = &'a dyn Fn(u64) -> Option<Arc<RelayCoeditSession>>;

#[derive(Default)]
pub(crate) struct Registry {
    tickets: Vec<Weak<Ticket>>,
}
impl Registry {
    pub(crate) fn handle(
        &mut self,
        command: Command,
        identity: Option<&HostIdentity>,
        sessions: Sessions<'_>,
    ) {
        if command.reply.is_closed() {
            return;
        }
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        let result = match identity {
            None => Err("native device identity unavailable".into()),
            Some(identity) => match command.operation {
                Operation::Open { owner, target } => self.open(owner, &target, identity, sessions),
                Operation::Heartbeat { ticket } => self
                    .known(&ticket)
                    .and_then(|()| {
                        ticket.heartbeat(
                            &sessions(ticket.workspace()).ok_or(NOT_CONNECTED)?,
                            |view| check_identity(view, identity),
                        )
                    })
                    .map(|()| Reply::Sent)
                    .inspect_err(|_| ticket.stop()),
                Operation::End { ticket } => self.known_pointer(&ticket).map(|()| {
                    ticket.leave(sessions(ticket.workspace()).as_deref());
                    Reply::Left
                }),
            },
        };
        // Dropping a successful Open reply also drops the only strong ticket.
        let _ = command.reply.send(result);
    }
    /// Apply what the governor of workspace `ws` said about its calls.
    pub(crate) fn voice(
        &mut self,
        ws: u64,
        event: &VoiceEvent,
        session: Option<&RelayCoeditSession>,
    ) {
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        let tickets: Vec<_> = self
            .tickets
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|ticket| ticket.workspace() == ws)
            .collect();
        for ticket in &tickets {
            let claimed = |id: [u8; 32]| {
                tickets
                    .iter()
                    .any(|other| !Arc::ptr_eq(other, ticket) && other.id() == Some(id))
            };
            if ticket.apply(event, session, claimed) {
                break;
            }
        }
    }
    /// The room on `ws` was dialled again: its calls join over `session`.
    pub(crate) fn reconnected(&mut self, ws: u64, session: &RelayCoeditSession) {
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        for ticket in self.tickets.iter().filter_map(Weak::upgrade) {
            if ticket.workspace() == ws
                && let Err(error) = ticket.rejoin(session)
            {
                tracing::debug!(%error, ws, "native call could not rejoin");
            }
        }
    }
    fn known(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        ticket.live()?;
        if ticket.leaving() {
            return Err("native participation is leaving".into());
        }
        self.known_pointer(ticket)
    }
    fn known_pointer(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        if !self
            .tickets
            .iter()
            .any(|known| known.ptr_eq(&Arc::downgrade(ticket)))
        {
            return Err("foreign native media ticket".into());
        }
        Ok(())
    }
    fn open(
        &mut self,
        owner: NativeWorkspaceOwner,
        target: &CallTarget,
        identity: &HostIdentity,
        sessions: Sessions<'_>,
    ) -> Result<Reply, String> {
        if self.tickets.len() >= 16 {
            return Err("native media capacity".into());
        }
        let room = RoomId::parse(&target.workspace).ok_or("invalid native room route")?;
        let workspace = owner.with_current(room, |view| {
            check_identity(view, identity)?;
            Ok(view.workspace())
        })?;
        let session = sessions(workspace).ok_or(NOT_CONNECTED)?;
        let ticket = Ticket::open(
            owner,
            room,
            &Channel {
                app: target.app.clone(),
                record: target.record.clone(),
                channel: target.channel.clone(),
            },
            &session,
            |view| check_identity(view, identity),
        )?;
        self.tickets.push(Arc::downgrade(&ticket));
        Ok(Reply::Opened(ticket))
    }
}

fn check_identity(view: &NativeCommittedView, identity: &HostIdentity) -> Result<(), String> {
    let now = tilo_clock::try_unix_now_ns().ok_or("native clock precedes epoch")?;
    let (cert, entries) = identity.current_native_material(view.workspace())?;
    if &cert != view.device()
        || identity.device_signing_key.verifying_key().to_bytes() != cert.device_public_key
    {
        return Err("native selected device custody changed".into());
    }
    let mut revoked = tilo_user_identity::RevocationList::new();
    for entry in entries {
        revoked.insert(entry).map_err(|error| error.to_string())?;
    }
    if revoked.is_revoked_at(&cert.device_public_key, now) {
        return Err("native selected device revoked".into());
    }
    Ok(())
}
