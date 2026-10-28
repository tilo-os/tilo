//! A new device of the person asking to be them, approved on this computer.
//!
//! One flow for every device: the person makes a pairing link here ("Pair a
//! new device"), single use and good for ten minutes, and opens it on the new
//! device (a phone scans its QR code, a browser takes the address). That
//! device asks a host both reach, the one whose workspace the link names, to
//! be certified as the person, naming the link's invite, and shows its
//! matching code. This computer, which made the invite, asks that host for
//! the requests waiting for the person and shows the one naming its invite:
//! what is asking (its label, the time, where from when the host can say)
//! and the same code. Approving has the person's root certify that device
//! through the host-side identity store, which also lists it with the
//! person's devices (Settings → Devices), where it can be revoked. Without
//! an approval nothing is admitted.
//!
//! The root never leaves the identity store: approval commands are signed by
//! its pairing approver, which signs nothing else, and the certificate by
//! the store itself, for exactly the request shown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::certify::PendingCertification;
use tilo_user_identity::{CertificationRequest, DeviceCert};

use super::hosted_sites::HostHttp;
use super::{RoomId, ShellNetwork};

/// How long a pairing link admits a new device.
pub(crate) const INVITE_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The person's root, as approving their new devices needs it.
pub(crate) trait RootApproval: Send + Sync {
    /// The person.
    fn user_public_key(&self) -> [u8; 32];
    /// What signs the approval commands a host takes from the person.
    ///
    /// # Errors
    ///
    /// Why the root cannot approve here.
    fn approver(&self) -> Result<Arc<dyn tilo_relay_client::DeviceSigner>, String>;
    /// Certify the device `request` names as one of the person's, and keep
    /// it with their devices.
    ///
    /// # Errors
    ///
    /// Why it was not certified.
    fn certify(&self, request: &CertificationRequest) -> Result<DeviceCert, String>;
}

/// [`RootApproval`] over the session's identity store.
struct SessionRoot {
    user_public_key: [u8; 32],
}

impl RootApproval for SessionRoot {
    fn user_public_key(&self) -> [u8; 32] {
        self.user_public_key
    }

    fn approver(&self) -> Result<Arc<dyn tilo_relay_client::DeviceSigner>, String> {
        let store = crate::shell::identity_admin::fleet_store()?;
        let approver = store
            .pairing_approver(&hex::encode(self.user_public_key))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(approver))
    }

    fn certify(&self, request: &CertificationRequest) -> Result<DeviceCert, String> {
        crate::shell::identity_admin::fleet_store()?
            .certify_paired_device(
                &hex::encode(self.user_public_key),
                request,
                tilo_clock::unix_now_ns(),
            )
            .map_err(|error| error.to_string())
    }
}

/// A pairing link this computer made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PairingInvite {
    /// What the new device's request names.
    pub id: [u8; 16],
    pub link: String,
    /// The host the new device asks, by its origin.
    pub origin: String,
    /// When it stops admitting, Unix-epoch milliseconds.
    pub expires_at_ms: u64,
}

/// A new device asking to be the person, at the host one of this
/// computer's invites named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceAsking {
    /// The request, as the answer names it.
    pub digest: [u8; 32],
    /// What it calls itself (such as "Safari on iPhone").
    pub label: String,
    /// The code it shows.
    pub code: String,
    /// When the host received it, Unix-epoch milliseconds.
    pub requested_at_ms: u64,
    /// Where it came from, when the host can say (ISO 3166-1 alpha-2).
    pub country: Option<String>,
    origin: String,
    request: CertificationRequest,
    invite: [u8; 16],
}

/// The invites this computer made and the devices asking under them.
#[derive(Default)]
pub(super) struct State {
    invites: Mutex<Vec<PairingInvite>>,
    asking: Mutex<Vec<DeviceAsking>>,
    /// The root, when a test stands one in for the identity store.
    root: Mutex<Option<Arc<dyn RootApproval>>>,
}

impl ShellNetwork {
    fn pairing_root(&self) -> Result<Arc<dyn RootApproval>, String> {
        let known = self.device_approvals.root.lock_recover().clone();
        if let Some(root) = known {
            return Ok(root);
        }
        let user_public_key = self
            .host_identity
            .as_ref()
            .ok_or("this computer has no identity yet")?
            .current_native_material(0)?
            .0
            .user_public_key;
        Ok(Arc::new(SessionRoot { user_public_key }))
    }

    /// Stand `root` in for the session's identity store.
    #[cfg(test)]
    pub(crate) fn set_pairing_root(&self, root: Arc<dyn RootApproval>) {
        *self.device_approvals.root.lock_recover() = Some(root);
    }

    /// Let every open pairing link run out now, as ten minutes would.
    #[cfg(test)]
    pub(crate) fn expire_device_invites(&self) {
        for invite in self.device_approvals.invites.lock_recover().iter_mut() {
            invite.expires_at_ms = 0;
        }
    }

    /// Make a pairing link for a new device of the person, at the host of a
    /// workspace this computer holds through the web (the new device must
    /// reach it): single use, good for [`INVITE_LIFETIME`].
    pub(crate) fn invite_device(&self) -> Result<PairingInvite, String> {
        let user = self.pairing_root()?.user_public_key();
        let (room, hosted) = self
            .site_rooms()
            .into_iter()
            .filter(|room| self.holds_room(*room))
            .filter_map(|room| Some((room, self.hosted_sites.hosted(room)?)))
            .find(|(_, hosted)| hosted.site.is_some())
            .ok_or(
                "Join a workspace on the web first: your new device meets this computer there.",
            )?;
        let entry = hosted.site.clone().unwrap_or_default();
        let id: [u8; 16] = {
            let mut id = [0; 16];
            id.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            id
        };
        let link = pairing_link(&entry, &hosted.origin, room, user, id)?;
        let invite = PairingInvite {
            id,
            link,
            origin: hosted.origin,
            expires_at_ms: tilo_clock::unix_now_ms()
                .saturating_add(u64::try_from(INVITE_LIFETIME.as_millis()).unwrap_or(u64::MAX)),
        };
        let mut invites = self.device_approvals.invites.lock_recover();
        let now = tilo_clock::unix_now_ms();
        invites.retain(|open| open.expires_at_ms > now);
        invites.push(invite.clone());
        drop(invites);
        Ok(invite)
    }

    /// Whether an invite this computer made still admits a device.
    #[cfg(test)]
    pub(crate) fn inviting_devices(&self) -> bool {
        let now = tilo_clock::unix_now_ms();
        self.device_approvals
            .invites
            .lock_recover()
            .iter()
            .any(|invite| invite.expires_at_ms > now)
    }

    /// Ask the hosts this computer's open invites named for the devices
    /// asking under them, and answer every one waiting now. Blocks on the
    /// hosts: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why the hosts could not be asked.
    pub(crate) fn devices_asking(&self) -> Result<Vec<DeviceAsking>, String> {
        let now = tilo_clock::unix_now_ms();
        let invites: Vec<PairingInvite> = {
            let mut invites = self.device_approvals.invites.lock_recover();
            invites.retain(|invite| invite.expires_at_ms > now);
            invites.clone()
        };
        if invites.is_empty() {
            self.device_approvals.asking.lock_recover().clear();
            return Ok(Vec::new());
        }
        let root = self.pairing_root()?;
        let approver = root.approver()?;
        let runtime = self.runtime_handle()?;
        let mut origins: Vec<&str> = invites
            .iter()
            .map(|invite| invite.origin.as_str())
            .collect();
        origins.sort_unstable();
        origins.dedup();
        let mut asking = Vec::new();
        for origin in origins {
            let http = HostHttp::new(origin)?;
            let pending = runtime
                .block_on(tilo_browser_peer::pending_pairings(
                    http.as_ref(),
                    approver.as_ref(),
                ))
                .map_err(|error| error.to_string())?;
            asking.extend(
                pending
                    .into_iter()
                    .filter(|entry| {
                        invites.iter().any(|invite| {
                            invite.id == entry.request.invite && invite.origin == origin
                        })
                    })
                    .map(|entry| device_asking(origin, entry)),
            );
        }
        self.device_approvals
            .asking
            .lock_recover()
            .clone_from(&asking);
        Ok(asking)
    }

    /// Approve (`approve`) or decline the device asking under `digest`. An
    /// approved device is certified as the person's and listed with their
    /// devices; either way its invite admits no other.
    pub(crate) fn answer_device(&self, digest: [u8; 32], approve: bool) -> Result<(), String> {
        let asking = self
            .device_approvals
            .asking
            .lock_recover()
            .iter()
            .find(|asking| asking.digest == digest)
            .cloned()
            .ok_or("That device stopped asking. Make a new link and try again.")?;
        let root = self.pairing_root()?;
        let approver = root.approver()?;
        let cert = if approve {
            Some(root.certify(&asking.request)?)
        } else {
            None
        };
        let http = HostHttp::new(&asking.origin)?;
        self.runtime_handle()?
            .block_on(tilo_browser_peer::decide_pairing(
                http.as_ref(),
                approver.as_ref(),
                digest,
                cert,
            ))
            .map_err(|error| error.to_string())?;
        self.device_approvals
            .invites
            .lock_recover()
            .retain(|invite| invite.id != asking.invite);
        self.device_approvals
            .asking
            .lock_recover()
            .retain(|other| other.invite != asking.invite);
        tracing::info!(approve, label = %asking.label, "answered a device asking to be the person");
        Ok(())
    }
}

fn device_asking(origin: &str, entry: PendingCertification) -> DeviceAsking {
    DeviceAsking {
        digest: entry.request_digest,
        label: entry.request.device_label.clone(),
        code: entry.pairing_code,
        requested_at_ms: entry.requested_at_ns / 1_000_000,
        country: entry.country,
        origin: origin.to_owned(),
        invite: entry.request.invite,
        request: entry.request,
    }
}

/// The pairing link: the workspace link of `room` at the host `origin`,
/// under the site's `entry`, with the person and the invite as its `pair`
/// secret (`<person hex><invite hex>`), so the one pairing link format
/// carries which invite a request answers.
fn pairing_link(
    entry: &str,
    origin: &str,
    room: RoomId,
    user: [u8; 32],
    invite: [u8; 16],
) -> Result<String, String> {
    use tilo_reference::link::{
        HostOrigin, Link, LinkSecrets, LinkTarget, Mode, Scope, Secret, WebEntry,
    };
    let entry = WebEntry::new(entry).map_err(|error| error.to_string())?;
    let host = HostOrigin::new(origin).map_err(|error| error.to_string())?;
    let link = Link {
        mode: Mode::Desktop,
        target: LinkTarget::new(Scope::Room {
            host,
            room: room.as_uuid(),
            slug: None,
        }),
        secrets: LinkSecrets {
            pair: Some(
                Secret::new(format!("{}{}", hex::encode(user), hex::encode(invite)))
                    .map_err(|error| error.to_string())?,
            ),
            ..LinkSecrets::default()
        },
    };
    link.to_url(&entry).map_err(|error| error.to_string())
}
