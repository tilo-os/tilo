//! A live session's life: what asks the person to confirm a connect, a delete
//! or an untrusted host key, what opening a session starts, the frames it
//! pumps while it is open, and how it is closed.

use std::rc::Rc;

use iroh_base::EndpointId;

use ui::TextRole;
use ui::widget::{Column, ModalDialog, Text};

use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind, Presence, Screen};
use crate::session::{SessionStatus, TransportReadiness, connection_readiness};
use crate::ssh::{SshConfig, SshSession};
use crate::view::Destination;
use crate::vnc::{VncConfig, VncSession};

use super::secrets::{VNC_PASSWORD, secret_purpose};
use super::{Action, LiveSession, PendingConfirmation, RemoteApp, TiloSession, owner};

impl RemoteApp {
    pub(super) fn request_connect(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notify("That saved connection no longer exists.");
            return;
        };
        if !connection_readiness(connection).can_connect() {
            self.open_session(id);
            return;
        }
        self.pending_confirmation = Some(PendingConfirmation::Connect {
            id,
            target: Self::confirmation_target(connection),
        });
        self.show_confirmation();
    }

    pub(super) fn request_delete(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            self.notify("That saved connection no longer exists.");
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::Delete {
            id,
            target: Self::confirmation_target(connection),
        });
        self.show_confirmation();
    }

    pub(super) fn ssh_host_key_issue(&self, id: ConnectionId) -> Option<crate::ssh::HostKeyIssue> {
        match (&self.session.borrow().live, self.session_for) {
            (Some(LiveSession::Ssh(session)), Some(session_id)) if session_id == id => {
                session.host_key_issue()
            }
            _ => None,
        }
    }

    pub(super) fn request_trust_ssh_host_key(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            return;
        };
        let target = Self::confirmation_target(connection);
        let Some(crate::ssh::HostKeyIssue::FirstContact { observed }) = self.ssh_host_key_issue(id)
        else {
            self.notify("No new SSH host key is waiting for trust. Forget a changed key before you review it again.");
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::TrustSshHostKey {
            id,
            target,
            observed,
        });
        self.show_confirmation();
    }

    pub(super) fn request_forget_ssh_host_key(&mut self, id: ConnectionId) {
        let Some(connection) = self.workspace.get(id) else {
            return;
        };
        let target = Self::confirmation_target(connection);
        if !matches!(
            self.ssh_host_key_issue(id),
            Some(crate::ssh::HostKeyIssue::Changed { .. })
        ) {
            self.notify("Only a changed SSH host key can be forgotten. Reconnect to check this machine again.");
            return;
        }
        self.pending_confirmation = Some(PendingConfirmation::ForgetSshHostKey { id, target });
        self.show_confirmation();
    }

    /// Build the dialog that asks the pending confirmation. It states the
    /// scope in plain language; cancel is the safe default.
    pub(super) fn show_confirmation(&mut self) {
        let Some(pending) = self.pending_confirmation.clone() else {
            self.dialog = None;
            return;
        };
        let name = self
            .workspace
            .get(pending.id())
            .map_or_else(|| "this connection".to_owned(), |c| c.name.clone());
        let mut key = None;
        let (title, detail, primary, destructive) = match &pending {
            PendingConfirmation::Disconnect { .. } => (
                format!("Close the session to {name}?"),
                "The live session ends and its scrollback goes with it. The saved connection stays.".to_owned(),
                "Close session",
                false,
            ),
            PendingConfirmation::Delete { .. } => (
                format!("Delete {name}?"),
                "The saved connection is removed for everyone you share it with. The machine itself is not changed.".to_owned(),
                "Delete connection",
                true,
            ),
            PendingConfirmation::Connect { .. } => (
                format!("Connect to {name}?"),
                "This opens a live session. Remote checks the saved address and access again before anything is sent.".to_owned(),
                "Connect",
                false,
            ),
            PendingConfirmation::TrustSshHostKey { observed, .. } => {
                key = Some(format!("{} {}", observed.algorithm, observed.fingerprint));
                (
                    format!("Trust the SSH key of {name}?"),
                    "Check this key with the machine's owner before you trust it. Only this device remembers it.".to_owned(),
                    "Trust key",
                    false,
                )
            }
            PendingConfirmation::ForgetSshHostKey { .. } => (
                format!("Forget the SSH key of {name}?"),
                "Only this device's saved key is removed. Reconnect to check and trust the new key.".to_owned(),
                "Forget key",
                true,
            ),
        };
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(&detail).with_role(TextRole::Body)));
        if let Some(key) = key {
            body = body.push(Box::new(
                Text::new(&key)
                    .with_role(TextRole::Code)
                    .with_kind(ui::TextKind::Secret),
            ));
        }
        let confirm = Rc::clone(&self.requests);
        let cancel = Rc::clone(&self.requests);
        let dismiss = Rc::clone(&self.requests);
        self.dialog = Some(
            ModalDialog::new(title, body)
                .with_test_id("remote.confirmation")
                .primary_action(primary, move || {
                    confirm.borrow_mut().push(Action::ConfirmPending);
                })
                .secondary_action("Cancel", move || {
                    cancel.borrow_mut().push(Action::CancelPending);
                })
                .on_dismiss(move || dismiss.borrow_mut().push(Action::CancelPending))
                .with_destructive(destructive)
                .open(true),
        );
        self.dirty = true;
    }

    /// Revalidate against the current shared projection immediately before a
    /// network attempt or deletion. A late collaborator edit changes neither
    /// the target nor the document unexpectedly; the user must review it.
    pub(super) fn confirm_pending(&mut self) {
        let Some(pending) = self.pending_confirmation.take() else {
            return;
        };
        let id = pending.id();
        let Some(connection) = self.workspace.get(id).cloned() else {
            self.notify("The saved connection was removed before you confirmed.");
            return;
        };
        if Self::confirmation_target(&connection) != pending.target() {
            self.notify("This connection changed while you were asked. Review it, then try again.");
            return;
        }
        match pending {
            PendingConfirmation::Connect { .. } => {
                // `open_session` repeats transport/profile readiness at the I/O
                // boundary, so an expired capability cannot race confirmation.
                self.open_session(id);
            }
            PendingConfirmation::Disconnect { .. } => self.disconnect_session(),
            PendingConfirmation::Delete { .. } => self.delete_connection(id),
            PendingConfirmation::TrustSshHostKey { observed, .. } => {
                let current = self.ssh_host_key_issue(id);
                if !matches!(current, Some(crate::ssh::HostKeyIssue::FirstContact { observed: ref current }) if current == &observed)
                {
                    self.notify("The SSH host key changed before it was trusted. Review it again.");
                    return;
                }
                match crate::ssh::trust_host_key(
                    &self.known_hosts,
                    &connection.host.0,
                    connection.port,
                    &observed,
                ) {
                    Ok(()) => self.open_session(id),
                    Err(error) => self.notify(format!("Couldn't save the SSH key: {error}")),
                }
            }
            PendingConfirmation::ForgetSshHostKey { .. } => {
                if !matches!(
                    self.ssh_host_key_issue(id),
                    Some(crate::ssh::HostKeyIssue::Changed { .. })
                ) {
                    self.notify("The SSH host key is no longer the one you chose to forget. Reconnect to review it again.");
                    return;
                }
                match crate::ssh::forget_host_key(
                    &self.known_hosts,
                    &connection.host.0,
                    connection.port,
                ) {
                    Ok(true) => {
                        self.notify("Forgot the saved SSH key. Reconnect to check the new one before you trust it.");
                        self.disconnect_session();
                    }
                    Ok(false) => self.notify("This device had no saved SSH key for the machine."),
                    Err(error) => self.notify(format!("Couldn't forget the SSH key: {error}")),
                }
            }
        }
    }

    /// Open the session surface for a connection. Tilo always uses its real
    /// shell-broker transport; SSH/VNC require the explicit live-session
    /// capability. A missing capability or RDP backend gets a first-class
    /// recovery surface — no socket is opened, and it is not misreported as a
    /// connection failure.
    pub(super) fn open_session(&mut self, id: ConnectionId) {
        let Some(c) = self.workspace.get(id).cloned() else {
            return;
        };
        self.selected = Some(id);
        self.destination = Destination::Session;
        let Some(next_generation) = self.session_generation.checked_add(1) else {
            // Never recycle a generation: pending actions use it as a
            // capability to a particular transport attempt. A process that
            // exhausted u64 attempts must restart rather than aliasing one.
            self.session_for = Some(id);
            self.session.borrow_mut().live = Some(LiveSession::Error(
                "Remote has run out of session identities. Restart it to connect again.".into(),
            ));
            self.screen = Screen::Session { id };
            return;
        };
        self.session_generation = next_generation;
        self.session_for = Some(id);
        self.set_fullscreen(false);
        {
            let mut session = self.session.borrow_mut();
            session.fb_image = None;
            session.fb_canvas = None;
            session.vnc_buttons = 0;
            session.held_keys.clear();
            session.vnc_wheel = (0.0, 0.0);
            session.live = None;
        }
        let readiness = connection_readiness(&c);
        let live = if readiness.can_connect() && self.capture {
            // A capture reaches nothing outside the process.
            LiveSession::Preview(SessionStatus::Connecting)
        } else if readiness.can_connect() {
            let live = self.start_live(&c);
            // When a machine was last used follows the person to their other
            // devices; whether it answered is this device's observation, made
            // once the transport reports (`pump_session`).
            let now = self.now_ms();
            if let Some(connection) = self.workspace.get_mut(id) {
                connection.last_used_ms = Some(now);
            }
            self.submit(Ok(crate::model::Remote::MARK_USED.with(
                crate::resource::remote::MarkUsed {
                    connection: crate::model::Remote::item(id),
                    at_ms: now,
                },
            )));
            live
        } else {
            LiveSession::Unavailable {
                readiness,
                kind: c.kind,
            }
        };
        self.session.borrow_mut().live = Some(live);
        self.screen = Screen::Session { id };
        if readiness.can_connect() && !self.capture {
            self.connect_started = Some(ui::time::now());
            self.connect_job = Some((
                id,
                ui::jobs::start(
                    ui::jobs::Spec::new(format!("Connecting to {}", self.target_name(id)))
                        .subject(
                            ui::jobs::Subject::new(owner(), self.target_name(id))
                                .opens(id.to_string()),
                        )
                        .unit(ui::jobs::Unit::Steps)
                        .phase("Connecting"),
                ),
            ));
        }
    }

    pub(super) fn disconnect_session(&mut self) {
        // Dropping the live session closes the transport (the worker thread
        // sees its channel close and disconnects).
        {
            let mut session = self.session.borrow_mut();
            if let Some(LiveSession::Tilo(tilo)) = session.live.as_ref() {
                let session_id = tilo.id;
                session
                    .collab_outbox
                    .push(ui::collaboration::CollabRequest::CloseWorkspaceControl { session_id });
            }
            session.live = None;
            session.fb_image = None;
            session.fb_canvas = None;
        }
        self.session_for = None;
        // At the identity boundary opening another transport is rejected, so
        // retaining MAX here cannot allow a stale operation to alias a new
        // session.
        if let Some(next_generation) = self.session_generation.checked_add(1) {
            self.session_generation = next_generation;
        }
        self.set_fullscreen(false);
        self.screen = Screen::List;
        if self.destination == Destination::Session {
            self.destination = Destination::Connections;
        }
    }

    /// Build the real transport for a connection from its kind + auth and
    /// the credential saved on this device (secrets are never stored in the
    /// document).
    fn start_live(&mut self, c: &Connection) -> LiveSession {
        match c.kind {
            ConnectionKind::Ssh => {
                let prefer_key = matches!(c.auth, AuthMethod::PublicKey);
                let secret = self.saved_secret(secret_purpose(c.kind, c.auth), c.id);
                let auth = match crate::ssh::resolve_auth(
                    prefer_key,
                    secret,
                    &crate::ssh::default_key_paths(),
                ) {
                    Ok(a) => a,
                    Err(e) => return LiveSession::Error(e.to_string()),
                };
                let user = if c.user.0.is_empty() {
                    tilo_paths::session::user_name().unwrap_or_else(|| "root".into())
                } else {
                    c.user.0.clone()
                };
                match SshSession::connect(SshConfig {
                    host: c.host.0.clone(),
                    port: c.port,
                    user,
                    auth,
                    cols: 80,
                    rows: 24,
                    known_hosts: self.known_hosts.clone(),
                }) {
                    Ok(s) => LiveSession::Ssh(Box::new(s)),
                    Err(e) => LiveSession::Error(e.to_string()),
                }
            }
            ConnectionKind::Vnc => match VncSession::connect(VncConfig {
                host: c.host.0.clone(),
                port: c.port,
                password: self.saved_secret(VNC_PASSWORD, c.id),
            }) {
                Ok(s) => LiveSession::Vnc(s),
                Err(e) => LiveSession::Error(e.to_string()),
            },
            ConnectionKind::Tilo => {
                let target = match c.host.0.parse::<EndpointId>() {
                    Ok(target) => *target.as_bytes(),
                    Err(_) => {
                        return LiveSession::Error(
                            "Tilo host must be an exact certified device endpoint id".into(),
                        );
                    }
                };
                let id = self.next_tilo_session_id;
                self.next_tilo_session_id = self.next_tilo_session_id.saturating_add(1);
                self.session.borrow_mut().collab_outbox.push(
                    ui::collaboration::CollabRequest::OpenWorkspaceControl {
                        session_id: id,
                        target_device: target,
                    },
                );
                match TiloSession::new(id) {
                    Ok(session) => LiveSession::Tilo(session),
                    Err(error) => LiveSession::Error(error),
                }
            }
            // `open_session` preflights RDP as `BackendUnavailable` before
            // reaching this constructor. Keep the defensive branch explicit
            // anyway, so a future caller cannot turn a persisted legacy RDP
            // profile into a generic, misleading transport failure.
            ConnectionKind::Rdp => LiveSession::Unavailable {
                readiness: TransportReadiness::BackendUnavailable,
                kind: ConnectionKind::Rdp,
            },
        }
    }

    /// Drain live-session I/O and note what this device observed of the
    /// machine: a connection that came up marks it reachable, one that
    /// failed marks it unreachable. The observation stays on this device.
    pub(super) fn pump_session(&mut self) {
        self.expire_connect();
        let (changed, status) = {
            let mut session = self.session.borrow_mut();
            let changed = session.pump() || std::mem::take(&mut session.changed);
            (changed, session.status())
        };
        if changed {
            self.dirty = true;
        }
        let Some(id) = self.session_for else {
            return;
        };
        let presence = match status {
            Some(SessionStatus::Connected) => Presence::Online,
            Some(SessionStatus::Failed(_)) => Presence::Offline,
            _ => return,
        };
        // A preview has no machine behind it to observe.
        if matches!(
            self.session.borrow().live,
            Some(LiveSession::Preview(_) | LiveSession::Sample(_))
        ) {
            return;
        }
        if self.workspace.presence_of(id) != presence {
            self.workspace.presence.insert(id, presence);
            self.dirty = true;
        }
    }
}
