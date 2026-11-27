//! The member list a device entering a workspace signs its own admission on, taken from its
//! host's join service and kept until the device takes the workspace in.
//!
//! The entry's answer says where the list is ([`Entered::Sign`]'s `host`): the host's join
//! service, which gives it to this device for this entry, binary. A member list of a few hundred
//! people is a third of a megabyte, which an action's answer does not fit. Once the device is
//! in, the list it holds, its own admission on it, is kept for the proof of membership that takes
//! the workspace in: the host sends that proof only what the list lacks, so the list reaches the
//! device once.
//!
//! [`Entered::Sign`]: tilo_server_protocol::entering::Entered::Sign

use std::collections::HashMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::MemberLinks;

/// The member list of each workspace this session entered and has not taken in yet, as the
/// device holds it.
pub(super) type EnteredLists = Arc<std::sync::Mutex<HashMap<WorkspaceId, Vec<Vec<u8>>>>>;

/// The member list an entering device signs its admission on, as it holds it, and whether it
/// took it from the host's join service: only a host that serves it there sends a proof of
/// membership only what the device lacks of it.
#[derive(Default)]
pub(super) struct SignedOn {
    pub(super) list: Vec<Vec<u8>>,
    taken: bool,
}

impl SignedOn {
    /// Take the member list an entry's [`Sign`] answer has this device sign its admission on:
    /// `governance`, the list it carries, from a host that sends it there or keeps no address for
    /// the workspace; else from the host's join service at `list_at` ([`ListFrom::take`]), only
    /// what the list held for an earlier answer of the same `entering` lacks.
    ///
    /// # Errors
    ///
    /// The entry names no workspace, or the join service did not give the list.
    ///
    /// [`Sign`]: tilo_server_protocol::entering::Entered::Sign
    pub(super) async fn take(
        &mut self,
        from: &ListFrom,
        (governance, list_at): (Vec<Vec<u8>>, Option<Vec<u8>>),
        signing: (&DeviceCert, &dyn tilo_server_client::DeviceSigner),
        entering: &tilo_server_protocol::entering::Entering,
    ) -> Result<(), super::EntryFailure> {
        // A host with no address for the workspace sends the list in the answer, and so does a
        // host from before the join service gave it (Protocol 71 inline list).
        let Some(at) = list_at.filter(|_| governance.is_empty()) else {
            *self = Self {
                list: governance,
                taken: false,
            };
            return Ok(());
        };
        let workspace =
            WorkspaceId::parse(&entering.workspace).ok_or_else(|| super::EntryFailure {
                code: "refused",
                message: "the entry names no workspace".into(),
            })?;
        let list = from
            .take(&at, signing, (workspace, entering.request_id), &self.list)
            .await
            .map_err(|reason| super::EntryFailure {
                code: "unavailable",
                message: format!(
                    "this device could not take the workspace's member list: {reason}"
                ),
            })?;
        *self = Self { list, taken: true };
        Ok(())
    }

    /// The member list as the device holds it once the workspace took `admission`, what it
    /// signed on the list: kept for its proof of membership, where it took it from the join
    /// service; empty otherwise.
    ///
    /// # Errors
    ///
    /// The list does not read.
    pub(super) fn held(self, admission: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, super::EntryFailure> {
        if !self.taken {
            return Ok(Vec::new());
        }
        tilo_peer_state::member_list_since::with_own(&self.list, admission).map_err(|reason| {
            super::EntryFailure {
                code: "refused",
                message: format!("the workspace's member list does not read: {reason}"),
            }
        })
    }
}

/// Where a device asks its host's join service for a member list: the dialer its session reaches
/// the host through, once it made one, and the host's certificate.
#[derive(Clone)]
pub(super) struct ListFrom {
    pub(super) dialer: Option<Arc<Dialer>>,
    pub(super) server_cert: DeviceCert,
}

impl ListFrom {
    /// The member list of `workspace_id` the host's join service at `at`, its signed address,
    /// gives this device, `cert` signing with `signer`, for its entry `request_id`: only what
    /// `held`, the list it took for an earlier answer of the same entry, lacks is sent, and the
    /// whole list is answered.
    ///
    /// # Errors
    ///
    /// The address does not read or names another host, the host is not reached, or it refuses.
    pub(super) async fn take(
        &self,
        at: &[u8],
        (cert, signer): (&DeviceCert, &dyn tilo_server_client::DeviceSigner),
        (workspace_id, request_id): (WorkspaceId, [u8; 32]),
        held: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, String> {
        let hint: tilo_peer_state::join::TransportHint =
            postcard::from_bytes(at).map_err(|error| {
                format!("the host's address for the workspace does not read: {error}")
            })?;
        if hint.host.device_public_key != self.server_cert.device_public_key {
            return Err(
                "the workspace's member list is kept by another host than its entry".into(),
            );
        }
        let dialer = self
            .dialer
            .as_ref()
            .ok_or("this session has not reached its host yet")?;
        match dialer.server().carrier() {
            tilo_server_client::Carrier::WebTransport => {
                let link = dialer
                    .link(Purpose::Join)
                    .await
                    .map_err(|error| error.to_string())?;
                link.prove_member(cert, signer, &self.server_cert)
                    .await
                    .map_err(|error| error.to_string())?;
                tilo_peer_state::membership::entry_list(
                    (cert.device_public_key, |request| {
                        super::ask_join(&link, request)
                    }),
                    request_id,
                    workspace_id,
                    cert,
                    signer,
                    held,
                )
                .await
            }
            #[cfg(not(target_arch = "wasm32"))]
            tilo_server_client::Carrier::Direct | tilo_server_client::Carrier::Relay => {
                let served =
                    tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
                        &hint,
                    )
                    .map_err(|error| error.to_string())?;
                let endpoint = dialer.endpoint().await.map_err(|error| error.to_string())?;
                tilo_peer_state::owner::enroll::entry_list(
                    &tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint),
                    &served.endpoint_addr,
                    (workspace_id, request_id),
                    cert,
                    signer,
                    held,
                )
                .await
            }
            #[cfg(target_arch = "wasm32")]
            tilo_server_client::Carrier::Direct | tilo_server_client::Carrier::Relay => {
                Err("a page takes a member list only on its host's WebTransport listener".into())
            }
        }
    }
}

/// Keep `list`, the member list of the workspace `workspace` names as this device holds it once
/// it entered, for the proof of membership that takes the workspace in: the device service's,
/// where it lends this session its copies (`store`), else this session's own (`lists`).
pub(super) fn keep_entered(
    (store, lists): (Option<&Arc<dyn MemberLinks>>, &EnteredLists),
    workspace: &str,
    list: Vec<Vec<u8>>,
) {
    let Some(workspace) = WorkspaceId::parse(workspace) else {
        return;
    };
    if list.is_empty() {
        return;
    }
    match store {
        Some(store) => store.keep_entered_list(workspace, list),
        None => {
            lists.lock_recover().insert(workspace, list);
        }
    }
}
