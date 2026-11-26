//! Where this device of the person's goes: the server it enters their workspaces through.
//!
//! A device that keeps a copy of its person's workspace writes none of it down. The ticket a computer logs in with names that server, which the
//! daemon keeps in memory while it takes the person's own workspace in from there; from then on,
//! and after every later start, the device's copy of that workspace names it: the host that keeps
//! the workspace's keys and is not this device. A device placed for records of one workspace keeps
//! no copy: its host is the one its ticket's login names on this start ([`super::confined`]). The device and its
//! person are its certificate's,
//! and so is the agent that runs on it, which its label names first ([`agent_of`]).

use std::sync::{Arc, Mutex};

use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_device_identity::login::{Device, ServerAddress};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::workspaces::{ServiceWorkspaces, block_on_owned};

/// The site a device names whose server names none of its own.
const SITE: &str = "https://tilo.sh/";

/// The server this device enters its person's workspaces through, once it knows one.
pub(crate) struct Home {
    workspaces: Arc<ServiceWorkspaces>,
    cert: DeviceCert,
    /// The device the ticket this computer logged in with made of it, while this daemon runs.
    entered: Option<Device>,
    found: Mutex<Option<Device>>,
    /// Set once this device wrote the person's list from what it knows of their workspaces.
    listed: tokio::sync::OnceCell<()>,
}

impl Home {
    /// The home of the device `cert` certifies, kept in `workspaces`, entered through `entered`
    /// when this computer just logged in with a ticket.
    pub(crate) fn new(
        workspaces: Arc<ServiceWorkspaces>,
        cert: DeviceCert,
        entered: Option<Device>,
    ) -> Self {
        let entered = entered.filter(|device| {
            device.cert.device_public_key == cert.device_public_key
                && device.cert.user_public_key == cert.user_public_key
        });
        Self {
            workspaces,
            cert,
            entered,
            found: Mutex::new(None),
            listed: tokio::sync::OnceCell::new(),
        }
    }

    /// The device this computer is, as it enters its person's workspaces: through the server
    /// the ticket named while this daemon runs, else through the one its copy of the person's
    /// workspace names. `None` while neither names one: asked again, it is there once the copy
    /// arrives. Awaited: the copy is read on the runtime this device's programs run on, so a
    /// program of the person's asking through the daemon waits here without waiting on itself.
    pub(crate) async fn get(&self) -> Option<Device> {
        if let Some(found) = self.known() {
            return Some(found);
        }
        let found = match &self.entered {
            Some(entered) => entered.clone(),
            None => self.named().await?,
        };
        let known = self.found.lock_recover().get_or_insert(found).clone();
        self.list_here(&known);
        Some(known)
    }

    /// Write the person's list of their workspaces here, once, when this device keeps their root
    /// and so is the one that writes it: the workspace its login named, as the server keeps it,
    /// and every workspace its copy records them in. A server that keeps their own workspace
    /// cannot write the list, so their other devices would read none.
    ///
    /// The list is kept in the person's own workspace, and only a device holding a copy of it
    /// writes it: one is taken in from the server the login names first, where the server keeps
    /// it. That is a workspace's copy, so it runs beside the answer this home gives, as the copy
    /// of any workspace a login enters is taken in: a caller waiting on this device is never held
    /// behind it.
    fn list_here(&self, device: &Device) {
        if !self.cert.capabilities.can_admit_devices || self.listed.set(()).is_err() {
            return;
        }
        let workspaces = Arc::clone(&self.workspaces);
        let cert = self.cert.clone();
        let device = device.clone();
        tokio::spawn(async move { list_the_persons_workspaces(workspaces, cert, device).await });
    }

    /// [`Self::get`] for a thread that does not drive the app runtime: it waits on the copy's
    /// read there instead of yielding to it, which a task of that runtime cannot do.
    pub(crate) fn get_blocking(&self) -> Option<Device> {
        if let Some(found) = self.known() {
            return Some(found);
        }
        if let Ok(app) = tilo_rt::try_app() {
            debug_assert!(
                !app.is_runtime_thread(),
                "a task of the app runtime asks Home::get instead: this read waits on it"
            );
        }
        let found = self.entered.clone().or_else(|| self.in_copy())?;
        Some(self.found.lock_recover().get_or_insert(found).clone())
    }

    /// Enter through `device` from now on: the server the person's workspace names now
    /// ([`Self::moved`]).
    pub(crate) fn enter_through(&self, device: Device) {
        *self.found.lock_recover() = Some(device);
    }

    /// The device this home knows it enters through, without reading the copy for it.
    pub(crate) fn known(&self) -> Option<Device> {
        self.found.lock_recover().clone()
    }

    /// A server the copy names that this home does not enter through yet, once the copy names
    /// one: the device as it enters through it. Waits on the copy's governance changes, so a
    /// server named after this daemon started is entered through as soon as it keeps the
    /// person's workspace: one this device is still handing it to is not gone to yet.
    pub(crate) async fn moved(&self) -> Device {
        let person = self.person();
        let mut governance = self.workspaces.copies().governance_changes();
        // What this home enters through as it starts following the copy: what it entered through
        // before, else what the copy named then, else none.
        let found = self.found.lock_recover().clone();
        let current = match found {
            Some(found) => Some(found),
            None => self.entered.clone().or(self.named().await),
        };
        let entering = current.map(|current| current.server_cert.device_public_key);
        loop {
            if let Some(named) = self.named().await
                && Some(named.server_cert.device_public_key) != entering
            {
                return named;
            }
            loop {
                match governance.recv().await {
                    Ok(changed) if changed != person => {}
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return std::future::pending().await;
                    }
                }
            }
        }
    }

    /// The person's own workspace, where their servers are named.
    fn person(&self) -> String {
        tilo_server_protocol::servers::person_workspace_id(&self.cert.user_public_key)
    }

    /// The device as this device's copy of the person's workspace names its server.
    fn in_copy(&self) -> Option<Device> {
        let own = Arc::clone(&self.workspaces);
        let cert = self.cert.clone();
        block_on_owned(async move { named_in(&own, &cert).await })
    }

    /// [`Self::in_copy`], asked from a task.
    async fn named(&self) -> Option<Device> {
        named_in(&self.workspaces, &self.cert).await
    }
}

/// Write the person's list of their workspaces here, once this device holds a copy of their own
/// workspace: the workspace its login named, as the server keeps it, and every workspace its copy
/// records them in.
async fn list_the_persons_workspaces(
    workspaces: Arc<ServiceWorkspaces>,
    cert: DeviceCert,
    device: Device,
) {
    let copies = workspaces.copies();
    let person = cert.user_public_key;
    let own = tilo_server_protocol::servers::person_workspace_id(&person);
    if copies.state(&own).await.is_none() && !taken_in(&workspaces, &device, &own).await {
        return;
    }
    let name = device
        .workspace_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    if let Some(name) = name {
        copies.list_for(&person, &device.workspace, name).await;
    }
    copies.list_known_for(&person).await;
}

/// Take the person's own workspace `own` in from the server `device` names, where this device
/// holds no copy of it: the one that keeps their root writes their list there, and a server that
/// keeps the workspace without that root writes none of it itself. The copy is a member copy,
/// synced with the server as the copy of any workspace its login entered is. Whether it is held
/// here now.
async fn taken_in(workspaces: &Arc<ServiceWorkspaces>, device: &Device, own: &str) -> bool {
    let entered = match tilo_api::daemon::enter::connect_signed(
        workspaces.signer(),
        device,
        Some(workspaces.lent()),
    )
    .await
    {
        Ok(entered) => entered,
        Err(error) => {
            tracing::warn!(
                %error,
                workspace = %own,
                "the person's own workspace is not taken in from their server"
            );
            return false;
        }
    };
    // Entered by its id at the device the login names, which admits this device as its person's:
    // the address its host keeps it at is what the member copy is taken in by.
    if let Err(error) = entered.join(own).await {
        tracing::warn!(
            %error,
            workspace = %own,
            "the person's own workspace did not let this device in"
        );
    }
    workspaces.hold(&entered).await;
    entered.close();
    workspaces.copies().state(own).await.is_some()
}

/// The device `cert` certifies as `own`'s copy of the person's workspace names its server: a
/// host that keeps the workspace's keys and is not this device.
async fn named_in(own: &ServiceWorkspaces, cert: &DeviceCert) -> Option<Device> {
    let person = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
    // This device's workspace service serves as the device's own key, which is a host of the
    // workspace too, and never this device's server. A server this device is handing the
    // workspace to is named before it keeps it, and named no more if the hand-over fails: it is
    // gone to once it says it keeps all of it.
    let copies = own.copies();
    let service = copies.device_cert().device_public_key;
    let hosts: Vec<_> = copies
        .hosts(&person)
        .await
        .ok()?
        .into_iter()
        .filter(|hint| hint.host.device_public_key != service)
        .filter(|hint| !copies.handing_to(&person, &hint.host.device_public_key))
        .collect();
    let server = server_among(&hosts, &cert.device_public_key)?;
    let relay_url = server
        .endpoint_addr
        .relay_urls()
        .next()
        .map(ToString::to_string);
    // The site the server answers at is the one its signed address names; one reached over
    // iroh alone names none.
    let origin = server.origin.clone();
    Some(Device {
        site: origin
            .as_ref()
            .map_or_else(|| SITE.to_owned(), |origin| format!("{origin}/")),
        host_origin: origin.unwrap_or_else(|| {
            format!(
                "{}{}",
                tilo_api::daemon::enter::IROH_ONLY,
                server.endpoint_addr.id
            )
        }),
        workspace: person,
        workspace_name: None,
        agent: agent_of(&cert.device_label).to_owned(),
        cert: cert.clone(),
        server_cert: server.device_certificate,
        server_address: Some(ServerAddress {
            relay_url,
            endpoint_addr: server.endpoint_addr,
            web: None,
        }),
    })
}

/// The server among the signed addresses `hosts` of the person's workspace, for the device
/// `this`: a host that keeps the workspace's keys and is not this device, one that runs the
/// workspace's work first.
pub(crate) fn server_among(
    hosts: &[tilo_peer_state::join::TransportHint],
    this: &[u8; 32],
) -> Option<WorkspaceDeviceHintV1> {
    hosts
        .iter()
        .filter_map(|hint| WorkspaceDeviceHintV1::from_transport_hint(hint).ok())
        .filter(|host| host.holds_keys && host.device_certificate.device_public_key != *this)
        .max_by_key(|host| host.runs_work)
}

/// The agent that runs on a device labelled `label`: what a login labels it first, as
/// `"Reviewer on build-box"` names the agent Reviewer, else the whole label.
#[must_use]
pub(crate) fn agent_of(label: &str) -> &str {
    tilo_device_identity::login::agent_of(label)
}
