//! The person's workspaces as one host lists them, followed on a device's session with it: what
//! every device of theirs (a page, a desktop, a computer the CLI logged in) reads to learn the
//! workspaces it may enter.
//!
//! The host sends the list whenever it changes (a workspace made, joined, renamed, left or
//! deleted), and a session that has heard none yet asks once ([`LIST_PERSONAL`]): so the list a
//! device reads is never older than the workspaces it may enter, and nothing asks on a timer.
//!
//! [`LIST_PERSONAL`]: tilo_server_protocol::workspaces::LIST_PERSONAL

use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::{PersonalList, WorkspaceLabels};

use crate::device::{Device, DeviceRefusal};
use crate::{Event, Events, Link};

/// The person's workspace list one host keeps, as a session with it hears it.
pub struct PersonalWorkspaces {
    /// The lists the host sent this session.
    sent: Events,
    /// The newest list this session heard or asked for.
    newest: Mutex<Option<PersonalList>>,
}

impl PersonalWorkspaces {
    /// Follow the lists `link`'s host sends from now on.
    #[must_use]
    pub fn follow(link: &Link) -> Self {
        Self {
            sent: link.events_where(|event| matches!(event, Event::Personal(_))),
            newest: Mutex::new(None),
        }
    }

    /// The queue the host's lists arrive on: what a reader that wants waking for each one listens
    /// to ([`Link::on_event`]).
    #[must_use]
    pub const fn sent(&self) -> &Events {
        &self.sent
    }

    /// The newest list the host sent or answered, taking in what it sent since the last read;
    /// `None` before it sent or answered any.
    #[must_use]
    pub fn heard(&self) -> Option<PersonalList> {
        let mut newest = self.newest.lock_recover();
        for event in self.sent.drain() {
            if let Event::Personal(list) = event {
                *newest = Some(*list);
            }
        }
        newest.clone()
    }

    /// The person's list as the host keeps it now: the newest it sent, else what `device` (whose
    /// session this follows) is answered when it asks.
    ///
    /// # Errors
    /// The host had sent none, and did not answer or refused.
    pub async fn read(&self, device: &Device) -> Result<PersonalList, DeviceRefusal> {
        self.read_in(device, device.workspace_id()).await
    }

    /// [`Self::read`], asking in `entered`, a workspace `device` is in on the host.
    ///
    /// # Errors
    /// The host had sent none, and did not answer or refused.
    pub async fn read_in(
        &self,
        device: &Device,
        entered: WorkspaceId,
    ) -> Result<PersonalList, DeviceRefusal> {
        if let Some(list) = self.heard() {
            return Ok(list);
        }
        let asked = device
            .personal_list_in(entered, &WorkspaceLabels::new(), None)
            .await?;
        let mut newest = self.newest.lock_recover();
        // A list the host sent while the ask was out is newer than its answer.
        for event in self.sent.drain() {
            if let Event::Personal(list) = event {
                *newest = Some(*list);
            }
        }
        Ok(newest.get_or_insert(asked).clone())
    }
}
