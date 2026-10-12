use super::{Arc, CallTarget, Command, NativeWorkspaceOwner, Operation, Reply, Ticket, Weak};
use crate::network::actor::{HostIdentity, native_ingress::NOT_CONNECTED};
use crate::shell::{native_workspace_owner::NativeCommittedView, state::RoomId};
use tilo_joined_room::voice::Channel;
use tilo_relay_session::coedit::{RelayCoeditSession, VoiceEvent};

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
                Operation::Open { owner, target } => self
                    .open(owner, &target, identity, sessions)
                    .inspect_err(|error| {
                        // Only fixed classes: never log documents, certificates,
                        // routes or an arbitrary lower layer's error text.
                        let refusal_class = match error.as_str() {
                            "exact admitted Chat missing" => "chat_missing",
                            "exact admitted Chat is encrypted" => "chat_encrypted",
                            "native selected device custody changed" => "identity_changed",
                            "native selected device revoked" => "device_revoked",
                            "no call is active on this channel" => "call_missing",
                            "native owner continuity is fenced" => "owner_fenced",
                            "room has no committed native enrollment" => "enrollment_missing",
                            "invalid media binding" => "binding_invalid",
                            "malformed media policy" => "binding_malformed",
                            NOT_CONNECTED => "session_missing",
                            _ => "other",
                        };
                        tracing::debug!(refusal_class, "native voice open refusal class");
                    }),
                Operation::Heartbeat { ticket } => self
                    .known(&ticket)
                    .and_then(|()| {
                        let session = sessions(ticket.workspace()).ok_or(NOT_CONNECTED)?;
                        ticket.heartbeat(&session, |view| check_identity(view, identity))
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
        let event_class = match event {
            VoiceEvent::Result { result: Ok(_), .. } => "start_result",
            VoiceEvent::Result {
                result: Err(reason),
                ..
            } if reason == "voice channel awaits governor admission" => "channel_pending",
            VoiceEvent::Result { result: Err(_), .. } => "start_refused",
            VoiceEvent::Granted(_) => "grant",
            VoiceEvent::Refused { .. } => "join_refused",
        };
        tracing::debug!(
            event_class,
            live_tickets = tickets.len(),
            "native voice governor event observed"
        );
        for ticket in &tickets {
            let claimed = |id: [u8; 32]| {
                tickets
                    .iter()
                    .any(|other| !Arc::ptr_eq(other, ticket) && other.id() == Some(id))
            };
            if ticket.apply(event, session, claimed) {
                tracing::debug!(event_class, "native voice governor event consumed");
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
        let workspace = owner
            .with_current(room, |view| {
                check_identity(view, identity)?;
                Ok(view.workspace())
            })
            .inspect_err(|_| {
                tracing::debug!(
                    phase = "workspace_identity",
                    "native voice open refusal phase"
                );
            })?;
        let session = sessions(workspace).ok_or(NOT_CONNECTED)?;
        let admitted = owner.with_current(room, |view| {
            check_identity(view, identity)?;
            let now = std::time::SystemTime::now();
            view.peer()
                .permissions(view.device().device_public_key, now)?;
            view.peer().check_document_read(
                view.device().device_public_key,
                &target.app,
                &target.record,
                now,
            )?;
            view.peer()
                .media_binding_if_admitted(&target.app, &target.record, &target.channel)
                .map(|binding| binding.is_some())
        })?;
        if !admitted {
            tracing::debug!("native voice channel awaiting admitted state");
            return Ok(Reply::BindingPending);
        }
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
        )
        .inspect_err(|_| {
            tracing::debug!(phase = "participation", "native voice open refusal phase");
        })?;
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
