//! How a device that runs apps for the people who call them signs: as itself, the device that
//! ran the app's code, and with every request decided as the caller who made the call it serves.
//!
//! A server runs a workspace's apps for its members and agents, and has no person of its own in
//! the workspace. So an app's request for a signature or for keys is decided as whoever made the
//! call the app serves: they must still be on the workspace's list of devices, and the workspace
//! allows what it allows them, never more. A request that names no call is decided as nobody and
//! refused; the device only ever signs as the one that ran the code.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::persist::{self, IdentityPaths};
use tilo_history::FluentArgs;
use tilo_identity::broker::{Presented, SignerSource, Undecided, WorkspaceMembership};
use tilo_user_identity::DeviceCert;

use crate::apps::service_workspaces::block_on;
use crate::workspaces::Workspaces;

/// Whoever made a call, as the connection it arrived on proved them: their device and the person
/// who certified it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedCaller {
    pub device_public_key: [u8; 32],
    pub user_public_key: [u8; 32],
}

/// Who made each call an app serves on this device, while it runs.
pub trait Callers: Send + Sync {
    /// Whoever made the call `invocation` that `app` serves in `workspace`, while the call runs.
    fn caller(
        &self,
        workspace: &WorkspaceId,
        app: &str,
        invocation: [u8; 16],
    ) -> Option<VerifiedCaller>;
}

/// The signer of a device that runs apps for the people who call them.
pub struct CallerSigner {
    workspaces: Workspaces,
    runtime: tokio::runtime::Handle,
    /// Where this device's own key and certificate are kept.
    paths: IdentityPaths,
    /// Whose device this is: the person its certificate names.
    person: [u8; 32],
    callers: Arc<dyn Callers>,
}

impl CallerSigner {
    /// The signer of the device whose key and certificate `paths` keeps, certified by `person`,
    /// deciding each request in the workspaces `workspaces` keeps as the caller `callers` names.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        runtime: tokio::runtime::Handle,
        paths: IdentityPaths,
        person: [u8; 32],
        callers: Arc<dyn Callers>,
    ) -> Self {
        Self {
            workspaces,
            runtime,
            paths,
            person,
            callers,
        }
    }
}

/// What `workspace`, as `workspaces` keeps it, allows `caller` now: their device is on its list
/// of devices, and nothing beyond what its policy gives them.
///
/// # Errors
/// The device keeps no such workspace, or the caller's device is not on its list.
pub fn allowed(
    workspaces: &Workspaces,
    runtime: &tokio::runtime::Handle,
    workspace: WorkspaceId,
    caller: &VerifiedCaller,
) -> Result<tilo_action_policy::MemberAccess, String> {
    let workspaces = workspaces.clone();
    let device = caller.device_public_key;
    block_on(runtime, async move {
        let (_, peer) = workspaces
            .governed_copy(workspace)
            .await
            .ok_or_else(|| format!("this device keeps no workspace {workspace}"))?;
        peer.permissions(device, tilo_clock::wall())
            .map_err(|_| "the caller may no longer act in this workspace".to_owned())
    })
}

/// What a person who asks this device to change `app` is told: it changes nothing for anyone
/// yet, and where they can.
#[must_use]
pub fn editing_unavailable(app: &str) -> String {
    let mut args = FluentArgs::new();
    args.set("app", app.to_owned());
    crate::i18n::message(None, "server-apps-editing-unavailable", &args)
}

impl SignerSource for CallerSigner {
    /// This device as itself: it joins a workspace's collaboration as the device it is, and is
    /// no person of its own.
    fn presented(&self, _: &dyn WorkspaceMembership, _: &WorkspaceId) -> Result<Presented, String> {
        Ok(Presented {
            person: self.person,
            // Only a join is presented so, and decides nothing: every decision is a caller's
            // ([`Self::decided_as`]).
            access: None,
            their_device: false,
            profile: None,
        })
    }

    fn decided_as(
        &self,
        _: &dyn WorkspaceMembership,
        workspace: &WorkspaceId,
        app: &str,
        call: Option<[u8; 16]>,
    ) -> Result<Presented, Undecided> {
        let caller = call
            .and_then(|invocation| self.callers.caller(workspace, app, invocation))
            .ok_or_else(|| Undecided::Refused(editing_unavailable(app)))?;
        let access = allowed(&self.workspaces, &self.runtime, *workspace, &caller)
            .map_err(Undecided::Refused)?;
        Ok(Presented {
            person: caller.user_public_key,
            access: Some(access),
            their_device: false,
            profile: None,
        })
    }

    fn device(&self, _: &Presented) -> std::io::Result<(ed25519_dalek::SigningKey, DeviceCert)> {
        let key = persist::load_device_key(&self.paths)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let cert = persist::read_device_cert(&self.paths.device_cert)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        tilo_user_identity::verify_device_cert(&cert)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if cert.device_public_key != key.verifying_key().to_bytes()
            || cert.user_public_key != self.person
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "this device's certificate is not its key's",
            ));
        }
        Ok((key, cert))
    }

    /// The keys reach as far as the caller's reach: the workspace's policy decides the read as
    /// them, which [`Self::decided_as`] already checked they may still make.
    fn hands_keys(&self, _: &Presented, _: &str) -> Result<(), String> {
        Ok(())
    }
}
