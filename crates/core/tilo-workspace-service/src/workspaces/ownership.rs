//! A workspace this host makes for a person is theirs.
//!
//! The host founds it, as it founds every workspace, and hands it to the
//! person in the same step ([`Workspaces::create_for`]): from then on its
//! governance names them as its owner on every device, and this host stays
//! in it as one of its administrators, the grant every member reads and its
//! owner may withdraw. That grant is what the host serves the workspace with:
//! it admits whom invitations name, names its own address, signs the
//! workspace's checkpoints and relays what its members write.

use super::{HostedWorkspace, Workspaces};

impl Workspaces {
    /// Hand `hosted`, a workspace this host owns, to `person`: one
    /// governance change, which leaves this host its administrator, and the
    /// name `name` for them when one is given, until they rename themselves.
    /// Nothing when it is theirs already, under that name, or `person` is
    /// this host's own.
    pub(super) async fn give_ownership(
        &self,
        hosted: &mut HostedWorkspace,
        person: [u8; 32],
        name: Option<&str>,
    ) -> Result<(), String> {
        if person == self.inner.host.cert.user_public_key {
            return Ok(());
        }
        let key = &self.inner.host.key;
        let now = tilo_clock::wall();
        let mut candidate = hosted.peer().clone();
        let handed = candidate
            .governance_mut()
            .transfer_ownership(person, key, now)?;
        let named = match name {
            Some(name) => candidate
                .governance_mut()
                .name_person(person, name, key, now)?,
            None => false,
        };
        if !(handed || named) {
            return Ok(());
        }
        let state = hosted.collaboration().clone();
        hosted.keep(candidate, key, now, |_, _| Ok(state))?;
        self.settle(hosted).await
    }
}
