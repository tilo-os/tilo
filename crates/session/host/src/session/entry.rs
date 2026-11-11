//! A session's entry through a workspace's door ([`tilo_relay_protocol::entering`]), and what
//! a failed connection says about this device: the host's answer to the entry, kept in the
//! session's status, the failure each refusal stops the session with, and the wait before the
//! next attempt.

use tilo_relay_client::LinkError;

use std::time::Duration;

use tokio::sync::mpsc;

use super::{Command, Exit, MAX_BACKOFF, NOT_ADMITTED, PeerHandle, Task, TransportPhase};

/// Called once with the host's answer to another entry, see
/// [`PeerHandle::enter_then`].
pub type EntryCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::entering::Entered, LinkError>) + Send>;
impl PeerHandle {
    /// Enter another workspace as `entering` says, on a stream of its own on
    /// this session's connection ([`tilo_relay_protocol::entering`]): a
    /// device in a session with its host redeems an invitation this way.
    /// `done` gets the host's answer, or why there was none: the session is
    /// not connected, or the host did not answer the entry.
    pub fn enter_then(
        &self,
        entering: tilo_relay_protocol::entering::Entering,
        done: EntryCompletion,
    ) -> Result<(), String> {
        self.send(Command::Enter(entering, done))
    }
}

/// What a failed connection says about this device.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The host was not reached, or something in front of it answered:
    /// retried with backoff, never counted against the device.
    Reachability,
    /// The host turned the device away without saying why: counted, and
    /// taken as [`NOT_ADMITTED`] after
    /// [`REFUSALS_BEFORE_FAILING`](super::REFUSALS_BEFORE_FAILING) in a row.
    Unexplained,
    /// The host said why: the session stops with this failure code and copy
    /// until the page acts on it.
    Final(&'static str, String),
}

pub(super) fn refusal(error: &LinkError) -> Refusal {
    match error {
        LinkError::HostChanged => Refusal::Final(
            "host-changed",
            "This workspace’s host was set up again.".to_owned(),
        ),
        LinkError::Unavailable(_) | LinkError::Malformed(_) | LinkError::Sign(_) => {
            Refusal::Reachability
        }
        // A host that turns a device away closes its handshake without a
        // word, so its own reason never reaches the page.
        LinkError::Refused(_) => Refusal::Unexplained,
        // Another peer protocol version: whichever side is behind updates
        // (docs/architecture/compatibility.md). This page is the host's
        // build, so a newer host means a reload brings it up to date.
        LinkError::PeerVersion { theirs }
            if tilo_relay_protocol::auth::this_device_is_behind(*theirs) =>
        {
            Refusal::Final(
                "update-required",
                "This page is out of date. Reload it to keep working together.".to_owned(),
            )
        }
        LinkError::PeerVersion { .. } => Refusal::Final(
            "host-update-required",
            "This workspace’s host needs to update to keep working together.".to_owned(),
        ),
    }
}

/// The failure code of an entry's refusal: the door's own code, as its page
/// reads it, or `not-admitted` when the workspace refused the device the door
/// let in.
pub(super) const fn door_code(
    refusal: &tilo_relay_protocol::entering::EnterRefusal,
) -> &'static str {
    use tilo_relay_protocol::directory::DoorRefusalCode;
    use tilo_relay_protocol::entering::EnterRefusal;
    match refusal {
        EnterRefusal::Workspace(_) => NOT_ADMITTED.0,
        EnterRefusal::Door { refused, .. } => match refused.code {
            DoorRefusalCode::Banned => "banned",
            DoorRefusalCode::Removed => "removed",
            DoorRefusalCode::InvitationOnly => "invitation-only",
            DoorRefusalCode::Full => "full",
            DoorRefusalCode::Refused => "refused",
        },
    }
}

/// The host's answer to a session's entry through a workspace's door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The workspace admitted this device.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its relay workspace.
        workspace_id: u64,
        /// Its name.
        name: String,
        /// What the person may do there.
        role: tilo_relay_protocol::workspaces::WorkspaceRole,
    },
    /// The door or the workspace did not let it in: the door's `code`
    /// (`banned`, `removed`, `invitation-only`, `full`, `refused`), or
    /// `not-admitted` when the workspace refused the device the door let in;
    /// what was said, and how long the door asked to wait before the next try.
    Refused {
        code: &'static str,
        message: String,
        retry_after_secs: Option<u64>,
        /// The door's own refusal as it answered it: its code, and what it
        /// says about a wait list the workspace keeps, so the page offers to
        /// leave an address there.
        door: Option<tilo_relay_protocol::directory::DoorRefused>,
    },
}

impl Entry {
    pub(super) fn refused(refusal: &tilo_relay_protocol::entering::EnterRefusal) -> Self {
        use tilo_relay_protocol::entering::EnterRefusal;
        Self::Refused {
            code: door_code(refusal),
            message: refusal.reason().to_owned(),
            retry_after_secs: match refusal {
                EnterRefusal::Door {
                    retry_after_secs, ..
                } => *retry_after_secs,
                EnterRefusal::Workspace(_) => None,
            },
            door: match refusal {
                EnterRefusal::Door { refused, .. } => Some(refused.clone()),
                EnterRefusal::Workspace(_) => None,
            },
        }
    }
}

impl Task {
    /// Take the host's answer to this session's entry: in, the session is a
    /// member's from here and enters no more; refused, it stops.
    pub(super) fn entered(
        &mut self,
        entered: tilo_relay_protocol::entering::Entered,
    ) -> Option<Exit> {
        use tilo_relay_protocol::entering::Entered;
        match entered {
            Entered::Admitted {
                workspace,
                workspace_id,
                name,
                role,
                desktop_scope,
            } => {
                self.config.entering = None;
                // The desktop opens with the keys of the group the host named
                // before the governance that decides it arrives.
                if let (Some(scope), Some(id)) = (
                    desktop_scope,
                    tilo_desktop_model::WorkspaceId::parse(&workspace),
                ) {
                    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
                    self.channels.scope_told(
                        id,
                        &collab_types::ChannelRoute::new(DESKTOP_APP, DESKTOP_RECORD),
                        scope,
                    );
                    self.pump_channels(true);
                }
                self.update(|status| {
                    status.entry = Some(Entry::Admitted {
                        workspace,
                        workspace_id,
                        name,
                        role,
                    });
                });
                None
            }
            Entered::Refused(refused) => {
                let entry = Entry::refused(&refused);
                self.update(|status| status.entry = Some(entry));
                Some(Exit::Refused(
                    door_code(&refused),
                    refused.reason().to_owned(),
                ))
            }
        }
    }

    /// This device's request for the keys of the channel group of the
    /// workspace its session enters ([`PeerConfig::entering`]), which its
    /// proof carries: prepared before anything of the workspace's governance
    /// is here, so its Welcome comes first in its download.
    ///
    /// [`PeerConfig::entering`]: super::PeerConfig::entering
    pub(super) async fn entry_access(&mut self) -> Option<collab_types::MlsAccessRequestV1> {
        let workspace = self.config.entering.as_ref()?.workspace.clone();
        let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)?;
        self.channels
            .ask_on_entry(
                workspace,
                tilo_clock::Instant::now(),
                tilo_clock::unix_now(),
            )
            .await
    }

    /// Make another entry on `client`'s connection, off the session's loop:
    /// `done` gets the host's answer.
    pub(super) fn enter(
        &self,
        entering: tilo_relay_protocol::entering::Entering,
        done: EntryCompletion,
        client: Option<&tilo_relay_session::RelayClient>,
    ) {
        let Some(entrance) = client.and_then(tilo_relay_session::RelayClient::entrance) else {
            done(Err(LinkError::Unavailable(
                "not connected to the host".to_owned(),
            )));
            return;
        };
        let (cert, signer) = (self.config.cert.clone(), self.signer.clone());
        let revocations = self.config.revocations.clone();
        n0_future::task::spawn(async move {
            let answer = n0_future::time::timeout(
                tilo_relay_protocol::entering::ENTERING_DEADLINE,
                entrance.enter(&cert, signer.as_ref(), revocations, entering),
            )
            .await
            .map_err(|_| {
                LinkError::Unavailable("the host did not answer the entry in time".to_owned())
            })
            .and_then(|answer| answer.map_err(super::connect::iroh_link_error));
            done(answer);
        });
    }

    /// The session is offline after its `attempt`th failed one in a row:
    /// how long it waits before the next, which its status says.
    pub(super) fn offline(&self, attempt: u32) -> Duration {
        let delay = MAX_BACKOFF.min(Duration::from_millis(
            250u64.saturating_mul(1u64 << attempt.min(7)),
        ));
        let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.update(|status| {
            status.phase = TransportPhase::Offline;
            status.authenticated = false;
            status.attempt = attempt;
            status.next_attempt_at_ms = Some(tilo_clock::unix_now_ms().saturating_add(delay_ms));
        });
        delay
    }

    /// The session stops, failed with `code` and `message`, until the page
    /// asks it to connect again (`true`) or closes it (`false`).
    pub(super) async fn stop_until_asked(
        &mut self,
        code: &'static str,
        message: String,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        self.fail(code, message);
        self.update(|status| {
            status.phase = TransportPhase::Failed;
            status.authenticated = false;
        });
        self.wait_for_reconnect(commands).await
    }
}
