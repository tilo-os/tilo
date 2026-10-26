//! What the host asks Remote to do, and what Remote answers: the effects an
//! open request, a confirmation or a row invokes — connect, forget a
//! credential, trust a host key — and the outcome reported back for each.

use crate::model::ConnectionId;
use crate::session::{SessionStatus, TransportReadiness, connection_readiness};
use crate::view::Destination;

use super::{Action, LiveSession, PendingResourceOperation, RemoteApp};

impl RemoteApp {
    /// The open session, published as a live row beside the saved
    /// connections: disconnect, send text, and the SSH host-key decisions.
    pub(crate) fn live_connection_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows: Vec<_> = self
            .workspace
            .connections()
            .iter()
            .map(crate::actions::connection_row)
            .collect();
        if let Some(id) = self.session_for
            && let Some(connection) = self.workspace.get(id)
        {
            let host_key_issue = self.ssh_host_key_issue(id);
            rows.push(crate::actions::session_row(
                connection,
                host_key_issue.as_ref(),
            ));
        }
        rows
    }

    /// Answer `effect`. Opening a session and sending text answer once the
    /// transport says how it went ([`Self::settle_pending_effect`]).
    pub(crate) fn answer_effect(
        &mut self,
        effect: crate::actions::RemoteEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::Remote, Self>,
    ) -> ui::framework::EffectAnswer {
        use crate::actions::RemoteEffect;
        let done = || Ok(ui::framework::Effected::done());
        match effect {
            RemoteEffect::ShowConnections => {
                self.apply(Action::Show(Destination::Connections));
                done()
            }
            RemoteEffect::New => {
                self.apply(Action::NewConnection);
                done()
            }
            RemoteEffect::EditConnection { connection } => {
                if self.workspace.get(connection).is_none() {
                    return Err("saved connection no longer exists".into());
                }
                self.apply(Action::EditConnection(connection));
                done()
            }
            RemoteEffect::Connect { connection } => {
                if self.pending_resource_operation.is_some() {
                    return Err("another remote operation is still running".into());
                }
                let Some(saved) = self.workspace.get(connection) else {
                    return Err("saved connection no longer exists".into());
                };
                let readiness = connection_readiness(saved);
                if !readiness.can_connect() {
                    let reason = if readiness == TransportReadiness::InvalidProfile {
                        saved
                            .profile_issue()
                            .unwrap_or_else(|| readiness.detail(saved.kind))
                    } else {
                        readiness.detail(saved.kind)
                    };
                    return Err(reason.to_string());
                }
                self.open_session(connection);
                let status = self.session.borrow().status();
                match status {
                    Some(SessionStatus::Connecting) => {
                        self.pending_resource_operation = Some(PendingResourceOperation::Connect {
                            reply: Some(cx.reply()),
                            connection_id: connection,
                            session_generation: self.session_generation,
                        });
                        Ok(ui::framework::Effected::Pending)
                    }
                    Some(SessionStatus::Connected) => done(),
                    Some(SessionStatus::Failed(message) | SessionStatus::Unavailable(message)) => {
                        Err(message)
                    }
                    Some(SessionStatus::Closed) | None => {
                        Err("remote transport did not start".into())
                    }
                }
            }
            RemoteEffect::SendText { connection, text } => {
                if self.pending_resource_operation.is_some() {
                    return Err("another remote operation is still running".into());
                }
                if self.session_for != Some(connection) {
                    return Err("that remote session is no longer active".into());
                }
                let acknowledgement = match &self.session.borrow().live {
                    Some(LiveSession::Ssh(session))
                        if session.status() == SessionStatus::Connected =>
                    {
                        session.send_input_ack(text.into_bytes())?
                    }
                    _ => return Err("text input requires a connected SSH terminal".into()),
                };
                self.pending_resource_operation = Some(PendingResourceOperation::SshInput {
                    reply: Some(cx.reply()),
                    connection_id: connection,
                    session_generation: self.session_generation,
                    acknowledgement,
                });
                Ok(ui::framework::Effected::Pending)
            }
            RemoteEffect::Disconnect { connection } => {
                if self.session_for != Some(connection) {
                    return Err("that remote session is no longer active".to_string());
                }
                self.disconnect_session();
                done()
            }
            RemoteEffect::TrustHostKey {
                connection,
                observed,
            } => self
                .trust_host_key(connection, &observed)
                .and_then(|()| done()),
            RemoteEffect::ForgetHostKey { connection } => {
                self.forget_host_key(connection).and_then(|()| done())
            }
        }
    }

    /// Trust the SSH host key `observed` on first contact with `connection_id`,
    /// then connect.
    fn trust_host_key(
        &mut self,
        connection_id: ConnectionId,
        observed: &crate::ssh::HostKeyIdentity,
    ) -> Result<(), String> {
        let connection = self
            .workspace
            .get(connection_id)
            .ok_or_else(|| "saved connection no longer exists".to_string())?;
        if !matches!(self.ssh_host_key_issue(connection_id), Some(crate::ssh::HostKeyIssue::FirstContact { observed: ref current }) if current == observed)
        {
            return Err(
                "the observed SSH host key is stale; reconnect to inspect the current key".into(),
            );
        }
        crate::ssh::trust_host_key(
            &self.known_hosts,
            &connection.host.0,
            connection.port,
            observed,
        )
        .map_err(|error| format!("could not save SSH host-key trust: {error}"))?;
        self.open_session(connection_id);
        Ok(())
    }

    /// Forget the changed SSH host key of `connection_id`, and disconnect.
    fn forget_host_key(&mut self, connection_id: ConnectionId) -> Result<(), String> {
        let connection = self
            .workspace
            .get(connection_id)
            .ok_or_else(|| "saved connection no longer exists".to_string())?;
        if !matches!(
            self.ssh_host_key_issue(connection_id),
            Some(crate::ssh::HostKeyIssue::Changed { .. })
        ) {
            return Err("only the currently observed changed SSH host key can be forgotten".into());
        }
        crate::ssh::forget_host_key(&self.known_hosts, &connection.host.0, connection.port)
            .map_err(|error| format!("could not forget SSH host-key trust: {error}"))?;
        self.disconnect_session();
        Ok(())
    }

    /// Answer the session effect still waiting, once its transport settled.
    pub(crate) fn settle_pending_effect(&mut self) {
        let cancelled = match &self.pending_resource_operation {
            Some(
                PendingResourceOperation::Connect { reply, .. }
                | PendingResourceOperation::SshInput { reply, .. },
            ) => reply
                .as_ref()
                .is_some_and(ui::framework::EffectReply::is_cancelled),
            None => return,
        };
        let Some(outcome) = self.pending_outcome(cancelled) else {
            return;
        };
        if let Some(
            PendingResourceOperation::Connect {
                reply: Some(reply), ..
            }
            | PendingResourceOperation::SshInput {
                reply: Some(reply), ..
            },
        ) = self.pending_resource_operation.take()
        {
            reply.finish(outcome.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// How the session effect still waiting ended, once it has; `cancelled`
    /// when its caller gave up on it.
    pub(crate) fn pending_outcome(&mut self, cancelled: bool) -> Option<Result<(), String>> {
        let pending = self.pending_resource_operation.as_ref()?;
        if cancelled
            && let PendingResourceOperation::Connect {
                connection_id,
                session_generation,
                ..
            } = pending
        {
            // Connect has not completed yet, so dropping its exact generation
            // cancels the socket/relay attempt. Do not let a cancellation for
            // an old attempt tear down a newer reconnect of the same saved
            // profile.
            if self.session_for == Some(*connection_id)
                && self.session_generation == *session_generation
            {
                self.disconnect_session();
            }
            return Some(Err("remote connection attempt cancelled".into()));
        }
        Some(match pending {
            PendingResourceOperation::Connect {
                connection_id,
                session_generation,
                ..
            } => {
                if self.session_for != Some(*connection_id)
                    || self.session_generation != *session_generation
                {
                    Err("the connection attempt was superseded by a newer session".into())
                } else {
                    match self.session.borrow().status() {
                        Some(SessionStatus::Connecting) => return None,
                        Some(SessionStatus::Connected) => Ok(()),
                        Some(
                            SessionStatus::Failed(message) | SessionStatus::Unavailable(message),
                        ) => Err(message),
                        Some(SessionStatus::Closed) | None => {
                            Err("remote connection closed before it became ready".into())
                        }
                    }
                }
            }
            PendingResourceOperation::SshInput {
                connection_id,
                session_generation,
                acknowledgement,
                ..
            } => {
                if self.session_for != Some(*connection_id)
                    || self.session_generation != *session_generation
                {
                    Err("the SSH session ended before input delivery was acknowledged".into())
                } else {
                    match acknowledgement.try_recv() {
                        Ok(result) => result,
                        Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            Err("remote input worker stopped before acknowledgement".into())
                        }
                    }
                }
            }
        })
    }
}
