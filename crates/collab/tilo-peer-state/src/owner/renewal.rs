//! A renewed device certificate in the workspaces this device joined. A
//! certificate that expires is renewed by its person's root before it runs
//! out (`tilo_identity_host`); the device proves its membership again with
//! the renewed one, the workspace's governor puts it in place of the one the
//! workspace held, and the workspace's record here follows. The device stays the same
//! member, with its role and everything it wrote.
use crate::join::JoinBootstrap;
use crate::owner::{NativeWorkspaceOwner, RoomId};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Whether `renewed` is a later certificate of `held`'s device from the same
/// person's root. The certificates' signatures are checked where each is
/// taken in: the device's custody and the workspace's governance.
#[must_use]
pub fn renews(renewed: &DeviceCert, held: &DeviceCert) -> bool {
    renewed.device_public_key == held.device_public_key
        && renewed.user_public_key == held.user_public_key
        && renewed.issued_at_ns > held.issued_at_ns
}

impl NativeWorkspaceOwner {
    /// Take `bootstrap`, the answer of `workspace`'s governor to this device
    /// proving its membership with `renewed`, into the workspace: its
    /// governance, which now holds `renewed`, and the workspace's record of this
    /// device. Published before it returns.
    ///
    /// # Errors
    /// `renewed` does not renew the certificate the workspace's record holds, the answer is not the workspace's or its
    /// governance does not hold `renewed`, or the record cannot be
    /// published.
    pub fn renew_membership(
        &self,
        workspace: RoomId,
        renewed: &DeviceCert,
        bootstrap: &JoinBootstrap,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(workspace, |view| Ok(view.clone()))?;
        if &candidate.record.device == renewed {
            return Ok(());
        }
        if !renews(renewed, &candidate.record.device) {
            return Err("the certificate does not renew this device's in the workspace".into());
        }
        if bootstrap.device_cert != *renewed
            || bootstrap.room != workspace.to_string()
            || bootstrap.host.user_public_key != candidate.owner()
        {
            return Err("the answer is not this workspace's renewal".into());
        }
        crate::owner::enrollment::install_governance(&mut candidate.peer, bootstrap, now)?;
        if !candidate.peer.certificates().contains(renewed) {
            return Err("the workspace's governance does not hold the renewed certificate".into());
        }
        candidate.record.device = renewed.clone();
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_workspace(workspace, candidate, None)
    }
}
