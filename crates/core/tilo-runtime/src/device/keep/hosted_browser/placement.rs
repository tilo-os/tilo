//! How a workspace's VM joins: with a join request, never a key.
//!
//! Placing a workspace's VM serves one ticket for it on this host's device action service
//! ([`tilo_peer_state::new_device::Tickets`], the one every device serves its tickets through),
//! and hands that ticket to the machine keeper in the [`Material`] it starts the VM's daemon
//! with. The daemon makes the VM's one device key and calls the ticket's certify action here.
//! This host certifies that key at once, with its own device key, for the VM's two records alone
//! ([`vm_placed_records`]): as one of the person's devices, when their root signed this host's
//! certificate, or under the placement grant they gave this host for those records, read from
//! their devices record ([`Placer::issuer`]). The certificate is placed in this host's register
//! ([`tilo_workspace_service::environment::EnvironmentRegistry::confine`]), and the daemon is answered
//! `CertifyAnswer { cert, enter }`: the workspace, entered through this host.
//!
//! A host that is neither refuses to place, in words a person can act on
//! ([`NOT_ALLOWED_TO_PLACE`]), before it starts the VM. The certificate lasts at most
//! [`tilo_user_identity::MAX_PLACED_DEVICE_LIFETIME_NS`]: while the placement is up, the host
//! certifies the device again before it ends ([`HostedBrowsers::renew_placed`]), as long as it
//! still may, and hands the VM the new certificate over the link its daemon dialled.

use std::sync::Arc;

use ed25519_dalek::{Signer as _, SigningKey};
use iroh::EndpointAddr;
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Declined, Direction, Entry, Reached, TICKET_LIFETIME_NS,
    Ticket,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::ActionService;
use tilo_peer_state::new_device::ServedTicket;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, MAX_PLACED_DEVICE_LIFETIME_NS, PlacedRecords, RevocationList,
};
use tilo_workspace_service::environment::vm_placed_records;
use tilo_workspace_service::workspaces::person_workspace::devices;
use tokio::sync::watch;

use super::{HostedBrowsers, Material};
use tilo_server_protocol::WorkspaceId;

/// What a host that is none of the person's devices, and holds no placement grant of theirs for
/// a workspace's computer, answers an open with.
pub const NOT_ALLOWED_TO_PLACE: &str = "This server isn't one of your devices, and you haven't \
     allowed it to run this workspace's computer. Allow it from one of your devices, then open \
     Browser again.";

/// What a host the person took back answers an open with.
pub const TAKEN_BACK: &str = "You removed this server from your devices, so it no longer runs \
     your workspace's computer.";

/// What a placement that stopped answers.
const STOPPED: &str = "The workspace's computer was stopped.";

/// This host as it places a workspace's VM: its own certificate and key, which sign the VM's
/// certificate, and the person's devices record, where a placement grant they gave it is read.
#[derive(Clone)]
pub(super) struct Placer {
    person: [u8; 32],
    host: DeviceCert,
    host_key: SigningKey,
    workspaces: Option<tilo_workspace_service::workspaces::Workspaces>,
}

impl Placer {
    /// What certifies a device placed for `records` at `now_ns`: this host's own certificate when
    /// the person's root signed it as one of their devices and has not taken it back, else the
    /// person's live, unrevoked placement grant to this host for those records.
    ///
    /// # Errors
    ///
    /// Neither holds: the words to refuse the placement with.
    async fn issuer(&self, records: &PlacedRecords, now_ns: u64) -> Result<DeviceCert, String> {
        let record = match &self.workspaces {
            Some(workspaces) => Some(devices::of(workspaces, &self.person).await),
            None => None,
        };
        if self.host.user_public_key == self.person && self.host.is_root_signed_device() {
            let taken_back = record.as_ref().is_some_and(|record| {
                record
                    .revocations(&self.person)
                    .iter()
                    .any(|entry| entry.revoked_device_public_key == self.host.device_public_key)
            });
            return if taken_back {
                Err(TAKEN_BACK.to_owned())
            } else {
                Ok(self.host.clone())
            };
        }
        record
            .and_then(|record| {
                record.placing_grant(&self.person, &self.host.device_public_key, records, now_ns)
            })
            .ok_or_else(|| NOT_ALLOWED_TO_PLACE.to_owned())
    }

    /// The certificate of the device `key`, named `label`, placed for `records` from
    /// `issued_at_ns` for as long as a placed device lasts and `issuer` does, signed with this
    /// host's key.
    ///
    /// # Errors
    ///
    /// The certificate would be none a verifier takes: `issuer` ended already.
    fn certify(
        &self,
        issuer: &DeviceCert,
        key: [u8; 32],
        records: PlacedRecords,
        label: &str,
        issued_at_ns: u64,
    ) -> Result<DeviceCert, String> {
        // No earlier than the issuer: a grant given within the second a ticket's certificate is
        // dated back.
        let issued_at_ns = issued_at_ns.max(issuer.issued_at_ns);
        let expires_at_ns = issued_at_ns
            .saturating_add(MAX_PLACED_DEVICE_LIFETIME_NS)
            .min(issuer.expires_at_ns.unwrap_or(u64::MAX));
        let mut cert = tilo_user_identity::prepare_placed_device_cert(
            issuer,
            key,
            records,
            label,
            issued_at_ns,
            expires_at_ns,
        );
        tilo_user_identity::sign_device_cert(&mut cert, &self.host_key).map_err(|error| {
            format!("this server could not certify the workspace's computer: {error}")
        })?;
        Ok(cert)
    }
}

/// One workspace's VM placement: the ticket its daemon logs in with, the certificate it holds
/// once renewed, why it ended when it was refused, and the switch that stops it.
pub(super) struct Placing {
    pub(super) served: ServedTicket,
    placer: Placer,
    /// The certificate this host renewed the VM's device with last, once it renewed it.
    renewed: Option<DeviceCert>,
    /// Whether this host renews the VM's certificate already ([`HostedBrowsers::keep_renewing`]).
    renewing: bool,
    /// Why the placement was refused, in words for whoever opened it, once it was.
    refused: watch::Receiver<Option<String>>,
    stop: watch::Sender<bool>,
    actions: ActionService,
}

impl Placing {
    /// Stop the placement: the ticket is served no more.
    fn stop(&self) {
        self.stop.send_replace(true);
        self.actions.tickets().withdraw(&self.served);
    }

    /// The certificate the VM's device holds now: the one renewed last, else the one its ticket
    /// gave.
    fn device(&self) -> Option<DeviceCert> {
        self.renewed
            .clone()
            .or_else(|| self.served.certified().borrow().clone())
    }
}

/// The approver of a workspace's VM device: this host certifies its key for the VM's records at
/// once, places the certificate in its register, which confines the device, and signs the
/// approval with its own key.
struct PlacedDevice {
    placer: Placer,
    workspace_id: WorkspaceId,
    environments: tilo_workspace_service::environment::EnvironmentRegistry,
    refused: watch::Sender<Option<String>>,
    stop: watch::Receiver<bool>,
}

impl Approver for PlacedDevice {
    fn approve(&self, asked: Asked) -> Approval {
        let placer = self.placer.clone();
        let environments = self.environments.clone();
        let (refused, stopped) = (self.refused.clone(), self.stop.clone());
        let records =
            vm_placed_records(self.workspace_id).map_err(|error| Declined(error.to_string()));
        Box::pin(async move {
            if *stopped.borrow() {
                return Err(Declined(STOPPED.to_owned()));
            }
            let records = records?;
            let certified = async {
                let issuer = placer.issuer(&records, tilo_clock::unix_now_ns()).await?;
                let cert = placer.certify(
                    &issuer,
                    asked.device_public_key,
                    records,
                    &asked.label,
                    asked.cert.issued_at_ns,
                )?;
                confined(&environments, &cert)?;
                Ok::<_, String>((issuer, cert))
            }
            .await;
            let (issuer, cert) = certified.map_err(|why| {
                refused.send_replace(Some(why.clone()));
                Declined(why)
            })?;
            Ok(Approved {
                signature: placer.host_key.sign(&asked.digest).to_bytes(),
                cert,
                approver: issuer,
            })
        })
    }
}

/// Place `cert`, which this host just certified for a workspace VM's records, in this host's
/// register, which confines its device to that workspace's Browser record. It is none of the
/// person's devices, and their devices record never lists it.
fn confined(
    environments: &tilo_workspace_service::environment::EnvironmentRegistry,
    cert: &DeviceCert,
) -> Result<(), String> {
    environments
        .confine(cert)?
        .map(|_| ())
        .ok_or_else(|| STOPPED.to_owned())
}

/// Where `addr` is reached, as a ticket names it.
fn reached_of(addr: &EndpointAddr) -> Reached {
    Reached {
        endpoint: *addr.id.as_bytes(),
        relay: addr.relay_urls().next().map(ToString::to_string),
        direct: addr.ip_addrs().copied().collect(),
    }
}

impl HostedBrowsers {
    /// What `workspace_id`'s VM is started with, opened on `record`: the ticket served for its
    /// placement, made and served the first time it is asked for, once this host may place the
    /// VM. The placement it is.
    ///
    /// # Errors
    ///
    /// This host is still starting, or may not place the VM: it is neither one of the person's
    /// devices nor holds their placement grant for the VM's records.
    pub(super) fn placement(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        record: &str,
    ) -> Result<(Material, ServedTicket), String> {
        let mut placements = self.placements.lock_recover();
        let material = |ticket: &Ticket, user_public_key| Material {
            workspace: workspace.to_owned(),
            workspace_id,
            record: record.to_owned(),
            ticket: ticket.clone(),
            user_public_key,
            account: self.account.clone(),
        };
        if let Some(placing) = placements.get(&workspace_id) {
            return Ok((
                material(placing.served.ticket(), self.person()?),
                placing.served.clone(),
            ));
        }
        let started = self.authority.lock_recover();
        let authority = started
            .as_ref()
            .ok_or("Browser is still starting. Try again in a minute.")?;
        let placer = Placer {
            person: authority.user_public_key,
            host: authority.host.clone(),
            host_key: authority.host_key.clone(),
            workspaces: authority
                .workspaces
                .as_ref()
                .and_then(tilo_workspace_service::workspaces::WeakWorkspaces::upgrade),
        };
        let records = vm_placed_records(workspace_id).map_err(|error| error.to_string())?;
        // Refused before the VM starts: a host that may not place it starts nothing.
        let may_place = {
            let placer = placer.clone();
            let records = records.clone();
            super::waited_on_its_own_thread(&authority.runtime, async move {
                placer.issuer(&records, tilo_clock::unix_now_ns()).await
            })
            .ok_or("Browser is still starting. Try again in a minute.")?
        };
        may_place?;
        let reached = reached_of(&authority.server);
        let ticket = Ticket::in_direction(Direction::Login, reached.clone());
        let (refused, refusal) = watch::channel(None);
        let (stop, stopped) = watch::channel(false);
        let approver = Arc::new(PlacedDevice {
            placer: placer.clone(),
            workspace_id,
            environments: authority.environments.clone(),
            refused,
            stop: stopped,
        });
        let served = {
            let _entered = authority.runtime.enter();
            ServedTicket::new(
                ticket.clone(),
                authority.user_public_key,
                Entry {
                    workspace: workspace.to_owned(),
                    workspace_name: None,
                    server_cert: authority.host.clone(),
                    server: reached,
                    site: String::new(),
                    host_origin: String::new(),
                },
                DeviceCapabilities::placed(records),
                approver,
                RevocationList::new(),
                tilo_clock::unix_now_ns().saturating_add(TICKET_LIFETIME_NS),
            )
        };
        authority.actions.tickets().serve(served.clone());
        placements.insert(
            workspace_id,
            Placing {
                served: served.clone(),
                placer,
                renewed: None,
                renewing: false,
                refused: refusal,
                stop,
                actions: authority.actions.clone(),
            },
        );
        drop(placements);
        let person = authority.user_public_key;
        drop(started);
        Ok((material(&ticket, person), served))
    }

    /// The person the workspaces here belong to.
    fn person(&self) -> Result<[u8; 32], String> {
        self.authority
            .lock_recover()
            .as_ref()
            .map(|authority| authority.user_public_key)
            .ok_or_else(|| "Browser is still starting. Try again in a minute.".to_owned())
    }

    /// The certificate `served`'s daemon was given, waited for until its daemon joins, this host
    /// refuses it, or the placement stops: the device every window of the VM runs as.
    pub(super) fn certified_device(
        &self,
        workspace_id: WorkspaceId,
        served: &ServedTicket,
    ) -> Result<DeviceCert, String> {
        let runtime = self
            .runtime()
            .ok_or("Browser is still starting. Try again in a minute.")?;
        let (stopped, refused) = self
            .placements
            .lock_recover()
            .get(&workspace_id)
            .map(|placing| (placing.stop.subscribe(), placing.refused.clone()))
            .ok_or(STOPPED)?;
        // A VM up already holds the certificate renewed last.
        let renewed = self
            .placements
            .lock_recover()
            .get(&workspace_id)
            .and_then(|placing| placing.renewed.clone());
        if let Some(renewed) = renewed {
            return Ok(renewed);
        }
        let mut certified = served.certified();
        super::waited_on_its_own_thread(&runtime, async move {
            let (mut stopped, mut refused) = (stopped, refused);
            tokio::select! {
                cert = certified.wait_for(Option::is_some) => {
                    cert.ok().and_then(|cert| cert.clone()).ok_or_else(|| STOPPED.to_owned())
                }
                why = refused.wait_for(Option::is_some) => {
                    Err(why.ok().and_then(|why| why.clone()).unwrap_or_else(|| STOPPED.to_owned()))
                }
                _ = stopped.wait_for(|stopped| *stopped) => {
                    Err("The workspace's computer was stopped before it joined.".to_owned())
                }
            }
        })
        .unwrap_or_else(|| Err(STOPPED.to_owned()))
    }

    /// Certify `workspace_id`'s VM device again, before its certificate ends: for a placement
    /// still up, by this host while it still may place the VM (one of the person's devices, or
    /// holding their unrevoked placement grant), for the same records from now on. The new
    /// certificate is placed in this host's register and handed to the VM over the link its
    /// daemon dialled. Waits on the runtime from a thread of its own, so it is asked from one
    /// that may block.
    ///
    /// # Errors
    ///
    /// The placement stopped or never joined, or this host may no longer place the VM: the
    /// device keeps its certificate until it ends, and is certified no more.
    pub fn renew_placed(&self, workspace_id: WorkspaceId) -> Result<DeviceCert, String> {
        let (placer, current) = self
            .placements
            .lock_recover()
            .get(&workspace_id)
            .map(|placing| (placing.placer.clone(), placing.device()))
            .ok_or(STOPPED)?;
        let current = current.ok_or("The workspace's computer hasn't joined yet.")?;
        let (runtime, environments) = self
            .authority
            .lock_recover()
            .as_ref()
            .map(|authority| (authority.runtime.clone(), authority.environments.clone()))
            .ok_or("Browser is still starting. Try again in a minute.")?;
        let records = vm_placed_records(workspace_id).map_err(|error| error.to_string())?;
        let now = tilo_clock::unix_now_ns();
        let issuer = {
            let (placer, records) = (placer.clone(), records.clone());
            super::waited_on_its_own_thread(
                &runtime,
                async move { placer.issuer(&records, now).await },
            )
            .ok_or(STOPPED)??
        };
        let cert = placer.certify(
            &issuer,
            current.device_public_key,
            records,
            &current.device_label,
            now,
        )?;
        let mut placements = self.placements.lock_recover();
        // Stopped meanwhile: nothing is certified for it again.
        let placing = placements.get_mut(&workspace_id).ok_or(STOPPED)?;
        confined(&environments, &cert)?;
        placing.renewed = Some(cert.clone());
        drop(placements);
        for open in [&self.open, &self.apps] {
            if let Some(up) = open.lock_recover().get_mut(&workspace_id)
                && up.device.device_public_key == cert.device_public_key
            {
                up.device = cert.clone();
            }
        }
        let workspace = [&self.open, &self.apps]
            .into_iter()
            .find_map(|open| Some(open.lock_recover().get(&workspace_id)?.workspace.clone()));
        if let Some(workspace) = workspace {
            self.sessions.renewed(&workspace, &cert)?;
        }
        Ok(cert)
    }

    /// Renew `workspace_id`'s VM device each time three quarters of its certificate's life has
    /// passed, while its placement is up and this host may place it ([`Self::renew_placed`]).
    pub(super) fn keep_renewing(&self, workspace_id: WorkspaceId, mut current: DeviceCert) {
        let (Some(browsers), Some(runtime)) = (self.me(), self.runtime()) else {
            return;
        };
        match self.placements.lock_recover().get_mut(&workspace_id) {
            Some(placing) if !placing.renewing => placing.renewing = true,
            Some(_) | None => return,
        }
        runtime.spawn(async move {
            loop {
                let ends = current.expires_at_ns.unwrap_or(u64::MAX);
                let life = ends.saturating_sub(current.issued_at_ns);
                let due = current
                    .issued_at_ns
                    .saturating_add(life.saturating_sub(life / 4));
                let wait = due.saturating_sub(tilo_clock::unix_now_ns());
                tokio::time::sleep(std::time::Duration::from_nanos(wait)).await;
                let renewing = Arc::clone(&browsers);
                let renewed = tokio::task::spawn_blocking(move || {
                    renewing.renew_placed(workspace_id)
                })
                .await;
                match renewed {
                    Ok(Ok(cert)) => current = cert,
                    Ok(Err(why)) => {
                        tracing::info!(%workspace_id, %why, "the workspace's computer is certified no more");
                        return;
                    }
                    Err(_) => return,
                }
            }
        });
    }

    /// Stop `workspace_id`'s placement, when one is under way or up: its ticket is served no
    /// more, and its device is certified no more.
    pub(super) fn unplace(&self, workspace_id: WorkspaceId) {
        let placing = self.placements.lock_recover().remove(&workspace_id);
        if let Some(placing) = placing {
            placing.stop();
        }
    }
}
