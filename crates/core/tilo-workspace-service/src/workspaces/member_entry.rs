//! A person's computer joining a workspace through its open entry: the device asks the
//! workspace's host, signs its own admission under the entry the workspace records with its own
//! key, and takes the workspace in once the host carried that admission to it. The host signs
//! nothing for it; every refusal the entry gives comes back in the entry's own words.

use std::sync::Arc;
use std::time::Duration;

use tilo_peer_state::join::TransportHint;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::entering::{Entered, Entering, Through};
use tilo_server_protocol::web::PageHost;

use super::holds_copies;
use crate::workspaces::Workspaces;

/// How long one ask of the entry may take, its host signing nothing and answering at once.
const ENTRY_ANSWER_WITHIN: Duration = Duration::from_secs(30);

impl Workspaces {
    /// Join `workspace` through its open entry at `host`, as the person `name` (else this
    /// device's label), and keep it here: its id. A device the workspace already admitted asks
    /// again and is answered in, so a second join holds what the first did.
    ///
    /// # Errors
    ///
    /// The entry refused the person (in its own words: not public, a role it does not give, a
    /// device of theirs already in), the host is not reached or is another than the entry
    /// names, or the copy could not be kept.
    pub async fn join_entry(
        &self,
        workspace: WorkspaceId,
        host: &PageHost,
        name: Option<&str>,
    ) -> Result<WorkspaceId, String> {
        Box::pin(self.join_entry_inner(workspace, host, name)).await
    }

    /// [`Self::join_entry`]'s steps.
    async fn join_entry_inner(
        &self,
        workspace: WorkspaceId,
        host: &PageHost,
        name: Option<&str>,
    ) -> Result<WorkspaceId, String> {
        let _entering = self.inner.member.entering.lock().await;
        let device = self.member_identity()?;
        holds_copies(&device.cert)?;
        if workspace.is_nil() {
            return Err("the entry names no workspace".into());
        }
        if self.member_link_up(workspace) && self.holds_as_this_device(&workspace.to_string()).await
        {
            return Ok(workspace);
        }
        let address = host
            .address
            .as_ref()
            .ok_or("the entry's host names nowhere it is reached")?;
        let remote = Arc::new(self.action_service().remote(
            &self.inner.host.endpoint,
            &address.endpoint_addr,
            tilo_server_protocol::workspaces::ENTRY_SCOPE,
        ));
        let entering = Entering {
            workspace: workspace.to_string(),
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&device.cert.device_label)
                .to_owned(),
            request_id: rand::random(),
            through: Through::Entry,
            admission: Vec::new(),
        };
        let entered = match ask(&remote, &device.cert, &entering).await? {
            Entered::Sign {
                governance,
                through,
            } => {
                self.sign_and_ask(
                    &remote,
                    &device,
                    &entering,
                    (workspace, &governance),
                    through,
                )
                .await?
            }
            entered => entered,
        };
        let hint = match entered {
            Entered::Admitted {
                host: Some(hint), ..
            } => hint,
            Entered::Admitted { host: None, .. } => {
                return Err("the workspace's host keeps no address to take it in from".into());
            }
            Entered::Refused(refusal) => return Err(refusal.reason().to_owned()),
            Entered::Sign { .. } => {
                return Err("the host did not take the admission this device signed".into());
            }
        };
        let hint: TransportHint = postcard::from_bytes(&hint).map_err(|error| {
            format!("the host's address for the workspace does not read: {error}")
        })?;
        if hint.host.device_public_key != host.server_cert.device_public_key {
            return Err("the workspace is kept by another host than its entry names".into());
        }
        let (served, _) = tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(&hint)
            .map_err(|error| error.to_string())?;
        let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(
            self.inner.host.endpoint.clone(),
        );
        let bootstrap = tilo_peer_state::owner::enroll::prove_membership(
            &client,
            &served.endpoint_addr,
            workspace,
            &device.cert,
            device.signer.as_ref(),
        )
        .await?;
        // The person's own computer asked for this join: theirs to write into their list.
        self.hold_member(&device, &bootstrap, &hint, true).await
    }
}

impl Workspaces {
    /// Sign this device's own admission on `governance`, as the entry answered `entering`, on
    /// top of what it signed into the workspace before, and ask the entry again with it, coming
    /// `through` the way the entry said: the entry's last answer.
    async fn sign_and_ask(
        &self,
        remote: &Arc<tilo_peer_state::actions::RemoteDevice>,
        device: &super::MemberDevice,
        entering: &Entering,
        (workspace, governance): (WorkspaceId, &[Vec<u8>]),
        through: Through,
    ) -> Result<Entered, String> {
        let invitation = match &through {
            Through::Invitation { invitation, .. } => Some(invitation.as_slice()),
            Through::Entry => None,
        };
        let key = device.cert.device_public_key;
        // What the device signed into the workspace before comes first: it never signs a second
        // change under a number it gave one already, after a restart either.
        let turn = tilo_peer_state::own_admission::signing_turn(&entering.workspace, key).await;
        let own = self.own_admissions(workspace, &key);
        // This device signs its own admission, the entry the workspace records its authority:
        // the host carries it to the workspace unchanged.
        let admission = tilo_peer_state::own_admission::sign_entry(
            &entering.workspace,
            (governance, &own),
            (invitation, entering.request_id),
            Some(entering.name.as_str()),
            (&device.cert, device.signer.as_ref()),
            tilo_clock::wall(),
        )
        .await
        .map_err(|reason| format!("this device could not sign its own admission: {reason}"))?;
        // Kept before it leaves: a hand-over of the workspace that reaches this device before
        // the host's answer signs on top of it.
        self.keep_own_admission(workspace, &key, &admission)
            .map_err(|reason| format!("this device could not keep its own admission: {reason}"))?;
        drop(turn);
        let signed = Entering {
            through,
            admission,
            ..entering.clone()
        };
        let answered = ask(remote, &device.cert, &signed).await;
        // A refused row leaves nothing of it in the workspace, so the next try signs its first
        // change again rather than one every copy would refuse.
        if !matches!(answered, Ok(Entered::Admitted { .. })) {
            self.forget_own_admission(workspace, &key);
        }
        answered
    }
}

/// One ask of the entry at `remote` for `entering`, as the device `cert`: the entry's answer.
async fn ask(
    remote: &Arc<tilo_peer_state::actions::RemoteDevice>,
    cert: &tilo_user_identity::DeviceCert,
    entering: &Entering,
) -> Result<Entered, String> {
    let request = tilo_server_protocol::workspaces::entry_invoke(
        entering,
        cert,
        tilo_actions::InvocationId::new(),
    )?;
    match Arc::clone(remote)
        .invoke_until_settled(request, ENTRY_ANSWER_WITHIN)
        .await
    {
        Some(Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. })) => {
            tilo_server_protocol::workspaces::entered_reply(&output.value.unwrap_or_default())
        }
        Some(Ok(tilo_actions::InvocationUpdate::Failed { error, .. })) => Err(error.message),
        Some(Ok(_)) => Err("the workspace's host did not answer the entry in time".into()),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the workspace's host is not reached".into()),
    }
}
