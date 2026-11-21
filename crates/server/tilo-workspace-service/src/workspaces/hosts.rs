//! The holders of a workspace held here: this device and each one its owners
//! asked to hold the workspace as well, every one alike ([`super::holding`]):
//! another device of the owner's person, or another person's server whose
//! operator takes that work.
//!
//! Each holder signs its own address, and the address says how it holds the
//! workspace ([`CopyRole`]): as a key holder, which reads it and takes its work,
//! or as a store, which keeps and forwards its ciphertext alone. The
//! workspace's governance names its holders
//! ([`tilo_peer_state::WORKSPACE_KEEPING_DEVICES_KEY`]), so every member learns
//! of each, and how it holds the workspace, as it learns any change to the
//! workspace, and the workspace's join answers carry them all: a member
//! reaches the workspace wherever it is held. Being named there is what admits
//! a holder everywhere the workspace is held: its device holds the workspace,
//! and its person may reach the holders that do
//! ([`tilo_peer_state::GovernedAccess`]). Being named a key holder is what gives
//! it the workspace's keys ([`tilo_peer_state::PeerGovernance::key_holders`]),
//! never a part among its members: a key holder of another person reads the
//! workspace without changing who is in it. A device of the owner's own
//! person is also admitted as one of the owner's devices before it is handed
//! the workspace, whatever role it holds it in.

use std::sync::Arc;
use std::time::Duration;

use iroh_base::EndpointAddr;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_net::proxy::host::{Destinations, check_destination, guarded_client, system_lookup};
use tilo_peer_state::CopyRole;
use tilo_peer_state::join::{Role, TransportHint};
use tilo_server_client::{
    LinkError,
    https::{HttpClient, HttpFuture},
};
use tilo_server_protocol::workspaces::WorkspaceHolder;
use tilo_server_session::client::ServerClient;
use tilo_server_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE, HostedWorkspace,
    UNHOST_WORKSPACE, ask_host,
};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, GovernedWorkspace, Workspaces};
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

/// The device a signed host address names.
const fn host_device(hint: &TransportHint) -> [u8; 32] {
    hint.host.device_public_key
}

impl Workspaces {
    /// Every holder of `workspace`, each its own signed address, by device
    /// key: the holders are equal, so nothing here comes first. A host the
    /// workspace's governance does not name yet (one founded here before it
    /// named itself) is listed at its own signed address all the same.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or this host's address cannot be
    /// signed.
    pub async fn hosts(&self, workspace: &str) -> Result<Vec<TransportHint>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        let own = self.transport_hint(&governed)?;
        let mut hosts = governed.peer().governance().keeping_devices();
        if !hosts
            .iter()
            .any(|hint| hint.host.device_public_key == own.host.device_public_key)
        {
            hosts.push(own);
        }
        drop(governed);
        hosts.sort_by_key(|hint| hint.host.device_public_key);
        Ok(hosts)
    }

    /// The devices that hold `workspace`'s keys now, as every device reads it
    /// from its governance ([`tilo_peer_state::PeerGovernance::key_holders`]):
    /// who can read it.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, or its governance does not read.
    pub async fn key_holders(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeSet<[u8; 32]>, String> {
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let governed = governed.lock().await;
        governed.peer().governance().key_holders(tilo_clock::wall())
    }

    /// Whether `governed`'s governance names this device a store of the
    /// workspace's ciphertext: it then asks for none of its keys and compacts
    /// none of what it cannot read.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub(super) fn holds_as_store(&self, governed: &GovernedWorkspace) -> bool {
        governed
            .peer()
            .governance()
            .holder_role(&self.inner.host.cert.device_public_key)
            == Some(CopyRole::Store)
    }

    /// Whether `workspace`'s governance names this device a store of its
    /// ciphertext, which reads none of it; `false` for one not governed here.
    pub async fn stores_only(&self, workspace: &str) -> bool {
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        self.holds_as_store(&governed)
    }

    /// Whether `workspace`'s governance names the device `device` among its
    /// hosts.
    pub(crate) async fn names_server(&self, workspace: &str, device: [u8; 32]) -> bool {
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        governed
            .peer()
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| host_device(hint) == device)
    }

    /// Name this host's own signed address among the workspace's hosts, so
    /// every member reads where the workspace is served from the replica it
    /// already holds. Naming it as it is named changes nothing. A host
    /// keeping another person's workspace, as neither its owner nor one of
    /// its administrators, names nothing: its holders are
    /// that person's to name, and the address it signed is the one they
    /// named. Nor does a holder its owners have not named yet, other than the
    /// device the workspace was founded on: whoever hands it the workspace
    /// names it, in the role they chose for it.
    pub(super) async fn name_own_host(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let own = host.cert.device_public_key;
        let (others, unnamed) = {
            let peer = governed.peer();
            (
                !peer.governs_history(&host.cert.user_public_key),
                peer.governance().holder_role(&own).is_none()
                    && peer.governance().founder() != Some(own),
            )
        };
        if others || unnamed {
            return Ok(());
        }
        let hint = self
            .transport_hint(governed)
            .map_err(|error| error.to_string())?;
        let now = tilo_clock::wall();
        let named = governed.change_held(
            &host.key,
            now,
            |_, candidate| {
                let named = candidate
                    .governance_mut()
                    .author_keeping_device(&hint, &host.key, now)?;
                Ok((named, named))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if named {
            self.settle(governed).await?;
        }
        Ok(())
    }

    /// Name this host's own signed address in every workspace it governs,
    /// after where it answers changed: what their governance names is where
    /// members and invitees dial this host.
    pub(super) async fn name_own_hosts(&self) {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            if let Err(error) = self.name_own_host(&mut workspace).await {
                tracing::error!(
                    workspace = %workspace.workspace(),
                    %error,
                    "the host's address is not named in the workspace's governance"
                );
            }
        }
    }

    /// Follow the relays this host answers through: each change of home relay
    /// is named in the workspaces it governs, so an invitation always carries
    /// an address a stranger can dial. A host that has lost its relays names
    /// nothing: the address it named stays what a workspace says, and a member
    /// reaches it again through the server once it comes back.
    pub(super) fn follow_own_address(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let workspaces = Arc::downgrade(&self.inner);
        let following =
            tilo_endpoint_reachability::follow(&self.inner.host.endpoint, &runtime, move |_| {
                let workspaces = workspaces.clone();
                async move {
                    let Some(inner) = workspaces.upgrade() else {
                        return;
                    };
                    Self { inner }.name_own_hosts().await;
                }
            });
        let _ = self.inner.own_address.set(following);
    }

    /// Change which hosts `workspace`'s governance names with `change`, as its
    /// governor.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed and settled"
    )]
    pub(super) async fn name_host(
        &self,
        workspace: &str,
        change: impl FnOnce(
            tilo_peer_state::PeerGovernanceMut<'_>,
            &ed25519_dalek::SigningKey,
            std::time::SystemTime,
        ) -> Result<bool, String>,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let now = tilo_clock::wall();
        let changed = governed.change_held(
            &host.key,
            now,
            |_, candidate| {
                let changed = change(candidate.governance_mut(), &host.key, now)?;
                Ok((changed, changed))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if changed {
            self.settle(&mut governed).await?;
        }
        Ok(())
    }

    /// Have the server `device`, at `addr`, hold `workspace` as well, as
    /// `role`, as its owners chose ([`Workspaces::chooses_servers`]): hand the
    /// workspace over with [`HOST_WORKSPACE`] and name the address it answers,
    /// which says that role, among the workspace's holders. A device of this
    /// device's own person is admitted to the workspace as one of the owner's
    /// devices first; another person's server takes the workspace only
    /// when its operator's work setting does. Asked again, the server is
    /// handed the workspace afresh, as `role`. Answers that address.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, `device` is this device, the server
    /// cannot be reached or is not that device, or it refused or answered an
    /// address that is not its own for the workspace in that role.
    pub async fn attach_server(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: EndpointAddr,
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let host = &self.inner.host;
        if device == host.cert.device_public_key {
            return Err("this device holds the workspace already".into());
        }
        let reached = self.reach_host(device, addr).await?;
        let member = reached.certificate.clone();
        let (workspace_id, arguments) = self.hand_over(workspace, &member, role).await?;
        let answer = reached.ask(HOST_WORKSPACE, arguments).await?;
        let hosted: HostedWorkspace =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let hint: TransportHint = super::decode_hex(&hosted.host_hint)?;
        let address =
            WorkspaceDeviceHintV1::from_transport_hint(&hint).map_err(|error| error.to_string())?;
        if hint.host != member
            || address.holds_keys != (role == CopyRole::KeyHolder)
            || address.workspace_id != workspace_id
        {
            return Err(
                "the device answered an address that is not its own for the workspace in the \
                 role it was asked to hold it in"
                    .into(),
            );
        }
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device(&hint, key, now)
        })
        .await?;
        tracing::info!(%workspace, holder = %hex::encode(device), person = %hex::encode(member.user_public_key), ?role, "another device holds the workspace");
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(hint)
    }

    /// Have the holder `device` of `workspace` hold it as `role` from now on,
    /// as its owners chose: its signed entry among the workspace's holders
    /// says so, and every device reads who gets the workspace's keys from it.
    /// Made a store, it leaves the workspace's key group, whose commit
    /// removing it rotates the keys, so it reads nothing written after; made a
    /// key holder, the group admits it. Another device is handed the
    /// workspace afresh in its new role at the address it is named at; this
    /// device signs its own entry. Answers the holder's address.
    ///
    /// # Errors
    ///
    /// The workspace is not held here or does not name `device` among its
    /// holders, or the holder refused or could not be reached.
    pub async fn set_holder_role(
        &self,
        workspace: &str,
        device: [u8; 32],
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let (named, own) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let named = governed
                .peer()
                .governance()
                .keeping_devices()
                .into_iter()
                .find(|hint| host_device(hint) == device);
            let own = (device == self.inner.host.cert.device_public_key)
                .then(|| self.transport_hint_as(&governed, role))
                .transpose();
            drop(governed);
            (named, own.map_err(|error| error.to_string())?)
        };
        let named =
            named.ok_or_else(|| format!("{} does not hold the workspace", hex::encode(device)))?;
        if tilo_peer_state::holder_role(&named) == role {
            return Ok(named);
        }
        let Some(own) = own else {
            let addr = WorkspaceDeviceHintV1::from_transport_hint(&named)
                .map_err(|error| error.to_string())?
                .endpoint_addr;
            return self.attach_server(workspace, device, addr, role).await;
        };
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device(&own, key, now)
        })
        .await?;
        tracing::info!(%workspace, ?role, "this device holds the workspace in another role");
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(own)
    }

    /// What `workspace` is handed to the device `member` with, to hold it as
    /// `role`: the workspace as a joining member gets it and this device's
    /// own signed address, under the workspace's id. A device of this one's
    /// own person is admitted to the workspace as one of the owner's devices
    /// first, so it takes in and writes the workspace's changes as they do;
    /// named a store, it holds no keys all the same
    /// ([`tilo_peer_state::PeerGovernance::key_holders`]).
    async fn hand_over(
        &self,
        workspace: &str,
        member: &DeviceCert,
        role: CopyRole,
    ) -> Result<(WorkspaceId, Map<String, Value>), String> {
        if member.user_public_key == self.inner.host.cert.user_public_key {
            self.enter(workspace, member)
                .await
                .map_err(|error| error.to_string())?;
        }
        let (workspace_id, holder_hint, bootstrap) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let mut bootstrap = governed
                .governed(self.serving_certificate())
                .bootstrap([0; 32], member.clone(), Role::Editor)
                .map_err(|error| error.to_string())?;
            // The host follows governance alone: records reach it over the
            // attach session, and leaving them here keeps the hand-over
            // within one action frame.
            bootstrap.records.clear();
            let hint = self
                .transport_hint(&governed)
                .map_err(|error| error.to_string())?;
            (governed.workspace_id(), hint, bootstrap)
        };
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        arguments.insert(
            "workspace_id".into(),
            Value::String(workspace_id.to_string()),
        );
        arguments.insert(
            "holder_hint".into(),
            Value::String(hex_postcard(&holder_hint)?),
        );
        arguments.insert("bootstrap".into(), Value::String(hex_postcard(&bootstrap)?));
        arguments.insert(
            "role".into(),
            serde_json::to_value(role).map_err(|error| error.to_string())?,
        );
        Ok((workspace_id, arguments))
    }

    /// Hand `workspace` to the device `device` at `addr` as its owner's
    /// choice of it does, but under the name `named`, and answer what that
    /// device said: what a test asks with whether a device that is handed a
    /// workspace takes it under any name but its own id.
    ///
    /// # Errors
    ///
    /// The device was not reached, or refused.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn hand_over_named(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: EndpointAddr,
        named: &str,
    ) -> Result<Value, String> {
        let reached = self.reach_host(device, addr).await?;
        let (_, mut arguments) = self
            .hand_over(workspace, &reached.certificate, CopyRole::KeyHolder)
            .await?;
        arguments.insert("workspace".into(), Value::String(named.to_owned()));
        reached.ask(HOST_WORKSPACE, arguments).await
    }

    /// Stop having the holder `device` hold `workspace`, as its owners chose:
    /// its address leaves the workspace's holders at once, and the holder is
    /// asked to stop ([`UNHOST_WORKSPACE`]) at `addr` when given. What only
    /// that holder did passes on with its entry: the workspace's compacted
    /// base is made by the holder governance names for it
    /// ([`tilo_peer_state::PeerGovernance::base_maker`]), so with this one
    /// gone the next key holder of the owner makes it. Answers whether the
    /// holder confirmed it stopped; one that could not be reached is still no
    /// longer named to members.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, or its hosts could not be recorded.
    pub async fn detach_host(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: Option<EndpointAddr>,
    ) -> Result<bool, String> {
        let known = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            governed
                .peer()
                .governance()
                .keeping_devices()
                .iter()
                .find(|known| host_device(known) == device)
                .map(|known| {
                    WorkspaceDeviceHintV1::from_transport_hint(known)
                        .map(|address| address.endpoint_addr)
                })
                .and_then(Result::ok)
        };
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device_removed(&device, key, now)
        })
        .await?;
        let _ = self.inner.changes.send(workspace.to_owned());
        let Some(addr) = addr.or(known) else {
            return Ok(false);
        };
        let stopped = async {
            let reached = self.reach_host(device, addr).await?;
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
            reached.ask(UNHOST_WORKSPACE, arguments).await
        }
        .await;
        match stopped {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::info!(%workspace, host = %hex::encode(device), %error, "a detached host was not reached");
                Ok(false)
            }
        }
    }

    /// Whether `caller` chooses which hosts hold `governed`: a device of the
    /// workspace's owner, who decides where it lives; or that person's server key, which
    /// acts for them from a script or a command. Never an agent's grant, and
    /// never a device for being of the person whose host this is: every
    /// holder answers it alike.
    pub(super) fn chooses_servers(governed: &GovernedWorkspace, caller: &DeviceCert) -> bool {
        let person = caller.user_public_key;
        let speaks_for_person = caller.capabilities.agent.is_none()
            || caller
                .capabilities
                .own_key_in(governed.peer().workspace_id());
        speaks_for_person && person == governed.peer().owner()
    }

    /// The servers keeping `workspace`, as its governance names them: this
    /// one, the server asked, first, then each other one by the name it signs
    /// into its own address.
    ///
    /// # Errors
    ///
    /// The workspace is not held here.
    pub async fn keeping_devices(&self, workspace: &str) -> Result<Vec<KeepingDevice>, String> {
        let hosts = self
            .hosts(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut holders = holders_of(&hosts);
        let here = hex::encode(self.inner.host.cert.device_public_key);
        holders.sort_by_key(|holder| holder.device != here);
        Ok(holders
            .into_iter()
            .map(|holder| KeepingDevice {
                here: holder.device == here,
                device: holder.device,
                person: holder.person,
                label: holder.label,
                reads: holder.reads,
                cloud: holder.cloud,
            })
            .collect())
    }

    /// Have the holder whose site is at `origin` (`https://tilo.sh`) hold
    /// `workspace` as well, as `role`: the holder says who it is and where it
    /// is dialed at its origin, and is then handed the workspace
    /// ([`Self::attach_server`]), which it takes as its own rules for whom it
    /// keeps workspaces say.
    ///
    /// # Errors
    ///
    /// As [`Self::attach_server`], or the origin did not say who it is.
    pub async fn add_server(
        &self,
        workspace: &str,
        origin: &str,
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let (device, addr) = host_at(origin, self.site_reach()).await?;
        self.attach_server(workspace, device, addr, role).await
    }

    /// Which addresses this device asks when an owner names a holder by its
    /// site: those on the public internet, and its own network too only
    /// where its operator lets it call there, the one leave a person's model
    /// provider is called by ([`Self::call_private_model_addresses`]).
    fn site_reach(&self) -> Destinations {
        if self.inner.models.private_addresses() {
            Destinations::Any
        } else {
            Destinations::Public
        }
    }

    /// Stop having the holder `named` hold `workspace`: named by its site's
    /// origin, or by its device key (64 hex) as [`Self::keeping_devices`] names it.
    /// Answers whether the holder confirmed it let the workspace go
    /// ([`Self::detach_host`]).
    ///
    /// # Errors
    ///
    /// The holder is not one of the workspace's, or the origin did not say
    /// who it is.
    pub async fn remove_server(&self, workspace: &str, named: &str) -> Result<bool, String> {
        let named = named.trim();
        let (device, addr) = if let Some(device) = device_key(named) {
            (device, None)
        } else {
            let (device, addr) = host_at(named, self.site_reach()).await?;
            (device, Some(addr))
        };
        if !self.names_server(workspace, device).await {
            return Err(format!("{named} does not hold the workspace"));
        }
        self.detach_host(workspace, device, addr).await
    }

    /// Move `workspace` from the server `from` (its site, or its device key
    /// as [`Self::keeping_devices`] names it) to the one at the site `to`
    /// ([`Self::move_holder`]).
    ///
    /// # Errors
    ///
    /// As [`Self::move_holder`], or a site did not say who it is.
    pub async fn move_server(&self, workspace: &str, from: &str, to: &str) -> Result<bool, String> {
        let from = from.trim();
        let old = match device_key(from) {
            Some(device) => device,
            None => host_at(from, self.site_reach()).await?.0,
        };
        let (new, addr) = host_at(to, self.site_reach()).await?;
        self.move_holder(workspace, old, new, addr)
            .await
            .map_err(|error| {
                error
                    .replace("the old server", from)
                    .replace("the new server", to)
            })
    }

    /// Move `workspace` from the server `old` to the server `new` at `addr`:
    /// `new` keeps it as well first, in the role `old` held it in
    /// ([`Self::attach_server`]), then `old`
    /// keeps it no more ([`Self::detach_host`]). Answers whether `old`
    /// confirmed it let the workspace go.
    ///
    /// # Errors
    ///
    /// `old` does not keep the workspace, or `new` did not take it: the
    /// workspace then stays where it was. Or `new` took it and `old` could
    /// not be let go: both keep it then, and the error says so.
    pub async fn move_holder(
        &self,
        workspace: &str,
        old: [u8; 32],
        new: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<bool, String> {
        if !self.names_server(workspace, old).await {
            return Err("the old server does not keep the workspace".into());
        }
        let role = self
            .hosts(workspace)
            .await
            .ok()
            .and_then(|hosts| {
                hosts
                    .iter()
                    .find(|hint| host_device(hint) == old)
                    .map(tilo_peer_state::holder_role)
            })
            .unwrap_or(CopyRole::KeyHolder);
        self.attach_server(workspace, new, addr, role)
            .await
            .map_err(|error| {
                format!(
                    "the new server did not take the workspace, which stays where it was: {error}"
                )
            })?;
        self.detach_host(workspace, old, None).await.map_err(|error| {
            format!("the new server keeps the workspace now, and so does the old server still: {error}")
        })
    }

    /// `device`, a host at `addr`, reached for its host operations: its
    /// certificate, as its handshake proves it, and the session they are
    /// asked on. The handshake pins the device, whoever's it is. The session
    /// subscribes to nothing, so it takes no session of the device's place
    /// there.
    async fn reach_host(&self, device: [u8; 32], addr: EndpointAddr) -> Result<OtherHost, String> {
        let host = &self.inner.host;
        let revocations = host.revocations.read_recover().clone();
        let client = ServerClient::connect_with_signer(
            host.endpoint.clone(),
            addr,
            None,
            Some(device),
            tilo_server_session::client::ClientCredentials {
                cert: host.cert.clone(),
                signer: &host.key,
                revocations,
                entering: None,
            },
        )
        .await
        .map_err(|error| match &error {
            // A host refuses a session it does not trust by closing it
            // unanswered: another person's host trusts one only when it keeps
            // workspaces for the people it lets in, or its operator takes
            // workspaces it does not own.
            tilo_server_session::client::ClientError::NoHandshakeAnswer(_) => format!(
                "the host refused this host's session: it keeps workspaces only for the \
                 people it let in, and this one keeps none ({error})"
            ),
            other => other.to_string(),
        })?;
        Ok(OtherHost {
            certificate: client.server_peer.cert.clone(),
            client,
        })
    }
}

/// Another host, reached for its host operations ([`Workspaces::reach_host`]).
struct OtherHost {
    /// Its certificate, as its handshake proved it.
    certificate: DeviceCert,
    client: ServerClient,
}

impl OtherHost {
    /// Ask the host the host operation `action` with `arguments`, and answer
    /// what it produced.
    async fn ask(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let status = ask_host(&self.client, host_invocation(action, arguments)).await?;
        receipt_output(status)
    }
}

/// The invocation of the host operation `action` with `arguments`.
fn host_invocation(action: &str, arguments: Map<String, Value>) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// What a host operation produced, from its receipt.
fn receipt_output(status: ReceiptStatus) -> Result<Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(json!({}))),
        // The other server's own words, as its refusal or failure says them.
        ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionCallError::Refused { reason },
        } => Err(reason),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
            Err("the other server did not finish it".into())
        }
    }
}

/// `value` as the hand-over sends it: postcard in hex.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, String> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| error.to_string())
}

/// One host of a workspace, as its owners list them ([`Workspaces::keeping_devices`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeepingDevice {
    /// The host's device key, hex: what removing it may name it by.
    pub device: String,
    /// The person the host runs for, by their user public key, hex.
    pub person: String,
    /// Whether this is the server asked: the one the list was read from.
    pub here: bool,
    /// What it calls itself, as its own signed address names it.
    pub label: String,
    /// Whether it holds the workspace's keys: it reads the workspace. A store
    /// keeps its ciphertext and reads none of it.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider, whose
    /// operator can read what it holds.
    pub cloud: bool,
}

/// `holders` as a member reads them: each named by its own signed address. A
/// holder that names itself nowhere goes by its device's label, and one whose
/// address does not decode reads as holding no keys.
pub(super) fn holders_of(holders: &[TransportHint]) -> Vec<WorkspaceHolder> {
    let mut keepers: Vec<WorkspaceHolder> = holders
        .iter()
        .map(|hint| {
            let payload = WorkspaceDeviceHintV1::from_transport_hint(hint).ok();
            let label = payload
                .as_ref()
                .map(|payload| payload.label.trim())
                .filter(|label| !label.is_empty())
                .unwrap_or(&hint.host.device_label);
            WorkspaceHolder {
                device: hex::encode(hint.host.device_public_key),
                person: hex::encode(hint.host.user_public_key),
                label: label.to_owned(),
                reads: payload.as_ref().is_some_and(|payload| payload.holds_keys),
                cloud: payload.as_ref().is_some_and(|payload| payload.cloud),
            }
        })
        .collect();
    keepers.sort_by(|a, b| a.label.cmp(&b.label));
    keepers
}

/// `text` as a device key, when it is 64 hex characters; anything else names
/// a host by its site.
fn device_key(text: &str) -> Option<[u8; 32]> {
    use hex::FromHex as _;
    <[u8; 32]>::from_hex(text).ok()
}

/// How long a holder's site may take to say who it is.
const SITE_ANSWERS: Duration = Duration::from_secs(20);
/// The most read of what a site says of its holder.
const MAX_SITE_ANSWER: usize = 64 * 1024;

/// A site, asked through the function it holds.
struct Site<F>(F);

impl<F> HttpClient for Site<F>
where
    F: Fn(&str, Vec<u8>) -> HttpFuture<'static> + Send + Sync,
{
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        (self.0)(path, body)
    }
}

/// The holder whose site is at `origin`, as it says there: its device and
/// where it is dialed.
///
/// An owner names the site, and this device asks it from inside its own
/// network. So it asks as it asks a provider a person named: an https
/// address, on the public internet at the lookup the connection dials, and
/// no redirect is followed. With [`Destinations::Any`], its operator's
/// leave, an address on its own network is asked too, also over plain http.
/// What a site answers a refusal with is not handed on.
async fn host_at(
    origin: &str,
    destinations: Destinations,
) -> Result<([u8; 32], EndpointAddr), String> {
    let origin = origin.trim().trim_end_matches('/');
    let no_site = || {
        format!(
            "{origin:?} is no holder's address: name its site by its https address, like \
             https://tilo.sh"
        )
    };
    let url = tilo_net::Url::parse(origin).map_err(|_| no_site())?;
    let plain = url.scheme() == "http" && destinations == Destinations::Any;
    if !(url.scheme() == "https" || plain)
        || !url.username().is_empty()
        || url.password().is_some()
        || check_destination(&url, destinations).is_err()
    {
        return Err(no_site());
    }
    let client =
        guarded_client(destinations, system_lookup()).map_err(|error| error.to_string())?;
    let site = origin.to_owned();
    let asked = Site(move |path: &str, body: Vec<u8>| -> HttpFuture<'static> {
        let request = client
            .post(format!("{site}{path}"))
            .header("content-type", "application/octet-stream")
            .timeout(SITE_ANSWERS)
            .body(body);
        Box::pin(async move {
            let mut response = request
                .send()
                .await
                .map_err(|error| error.without_url().to_string())?;
            let status = response.status().as_u16();
            let mut answer = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| error.without_url().to_string())?
            {
                if answer.len().saturating_add(chunk.len()) > MAX_SITE_ANSWER {
                    return Err("it answered more than a holder says of itself".to_owned());
                }
                answer.extend_from_slice(&chunk);
            }
            Ok((status, answer))
        })
    });
    let info = tilo_server_client::https::server_info(&asked)
        .await
        .map_err(|error| match error {
            // What a site answers a refusal with stays with it: an owner
            // names any site, and its words are not this device's to hand on.
            LinkError::Refused(_) => format!("{origin} refused to say which holder it is"),
            other => format!("{origin} did not say which holder it is: {other}"),
        })?;
    let address = info
        .address
        .ok_or_else(|| format!("{origin} names no address to reach its host at"))?;
    Ok((info.server_cert.device_public_key, address.endpoint_addr))
}

impl Workspaces {
    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `workspace`, saying the role this device holds it in as
    /// its governance names it (a key holder until it names one): the relay
    /// this host is reached through and the addresses a stranger can dial,
    /// never the ones only the network around this host knows
    /// (`tilo_delivery_protocol::workspace_join::reachable`).
    pub(super) fn transport_hint(
        &self,
        workspace: &GovernedWorkspace,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        self.transport_hint_as(workspace, self.own_role(workspace))
    }

    /// The role this device's governance names it in `workspace`, a key
    /// holder until it names one.
    fn own_role(&self, workspace: &GovernedWorkspace) -> CopyRole {
        workspace
            .peer()
            .governance()
            .holder_role(&self.inner.host.cert.device_public_key)
            .unwrap_or(CopyRole::KeyHolder)
    }

    /// The address an invitation names: this device's once it is reached through one of its
    /// relays, which wake for it and are waited for up to a net report, because a link signed
    /// before this device reached one names only the network it stands in. A device that reaches
    /// none in that time names the address it has, so one with no way out still hands out a link.
    /// Nothing held back names the address the endpoint has. Asked before the workspace is
    /// locked: the wait belongs to the invitation, not to everything the workspace answers.
    pub(super) async fn invitation_addr(&self) -> EndpointAddr {
        match self
            .reachability()
            .reached_addr("an invitation names this device's address")
        {
            Some(reached) => reached.await,
            None => self.endpoint_addr(),
        }
    }

    /// The hint an invitation of `workspace` carries, signed for this device answering at `addr`.
    pub(super) fn invitation_hint(
        &self,
        workspace: &GovernedWorkspace,
        addr: &EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        self.signed_hint(workspace, self.own_role(workspace), addr)
    }

    /// [`Self::transport_hint`], saying this device holds `workspace` as
    /// `role`: a store neither holds its keys nor takes its work.
    pub(super) fn transport_hint_as(
        &self,
        workspace: &GovernedWorkspace,
        role: CopyRole,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let addr = self.endpoint_addr();
        self.signed_hint(workspace, role, &addr)
    }

    /// Sign `workspace`'s answer for this device holding it `role`, answering at `addr`.
    fn signed_hint(
        &self,
        workspace: &GovernedWorkspace,
        role: CopyRole,
        addr: &EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let holds_keys = role == CopyRole::KeyHolder;
        let host = &self.inner.host;
        // An invitation is handed to whoever the link reaches, so what this
        // host answers at inside its own network stays out of it.
        let endpoint_addr = tilo_delivery_protocol::workspace_join::reachable(addr);
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        let claim = host.holder();
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
            version: 1,
            holds_keys,
            runs_work: holds_keys && !host.work.takes_nothing(),
            label: claim.label,
            cloud: claim.cloud,
            moderates: claim.moderates.contains(workspace.workspace()),
            workspace_id: workspace.workspace_id(),
            endpoint_addr,
            user_public_key: host.cert.user_public_key,
            device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }

    /// Name this keeper among the holders of every workspace it serves as
    /// `label`, which its signed addresses carry from now on: a server's name
    /// its person changed in their admin workspace, so members read the name
    /// it goes by now.
    pub async fn name_holder(&self, label: &str) {
        let label = label.trim();
        self.claim(|claim| {
            if claim.label == label {
                return false;
            }
            claim.label.clear();
            claim.label.push_str(label);
            true
        })
        .await;
    }

    /// Say among `workspace`'s holders that this keeper has a moderation
    /// agent for it, which its signed address carries from now on: what lets
    /// a person who governs the workspace turn it on.
    pub async fn offer_moderation(&self, workspace: &str) {
        self.claim(|claim| claim.moderates.insert(workspace.to_owned()))
            .await;
    }

    /// Change what this keeper says of itself among the holders with
    /// `change`, and sign its address anew in every workspace it serves when
    /// that changed anything.
    async fn claim(&self, change: impl FnOnce(&mut crate::workspaces::HolderClaim) -> bool) {
        {
            let mut claim = self.inner.host.holder.write_recover();
            if !change(&mut claim) {
                return;
            }
        }
        self.name_own_hosts().await;
    }
}
