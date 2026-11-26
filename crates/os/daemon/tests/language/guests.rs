//! A person who is not the device's own, in a workspace: their device admitted by an invitation
//! its owner hands out, asking the daemon over the device link as their laptop does, and the
//! listing the workspace's governance offers them there.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionCallError, ActionProvider as _};
use tilo_peer_state::actions::{ActionService, NoActions, RemoteDevice};
use tilo_user_identity::RevocationList;

use crate::language::host;

/// The workspace the daemon founded: the one a test's owner device is in.
fn owner_workspace(host: &host::Host) -> &str {
    host.workspace()
}

/// A person's device, admitted to a workspace by an invitation its owner hands out in `role`.
pub struct Guest {
    remote: Arc<RemoteDevice>,
    caller: tilo_actions::remote::Caller,
    _service: ActionService,
    _endpoint: iroh::Endpoint,
}

impl Guest {
    /// The person this guest is, their root in hex: whose read a record of the workspace is
    /// decided by when the guest's device reads its copy.
    #[must_use]
    pub fn person(&self) -> String {
        hex::encode(self.caller.user_public_key().copied().unwrap_or_default())
    }

    /// The action at `address` of `provider` with `arguments`, followed until it settles.
    pub async fn call(
        &self,
        provider: &str,
        address: tilo_actions::ActionAddress,
        arguments: &Value,
    ) -> Result<InvocationUpdate, ActionCallError> {
        tilo_test_env::within(
            "the daemon to answer over the device link",
            Arc::clone(&self.remote).invoke_until_settled(
                host::call(provider, address, arguments),
                std::time::Duration::from_secs(30),
            ),
        )
        .await
        .expect("the device link answers")
    }

    /// Every action `provider` is listed with for this caller, walked through the record's tree
    /// as an agent walks it.
    pub async fn listed(&self, provider: &str) -> Vec<tilo_actions::ActionDescriptor> {
        /// The queries a record's tree takes before a listing gives up: a mistake is no tree.
        const LEVELS: usize = 64;
        let mut listed = Vec::new();
        let mut under: Vec<Option<String>> = vec![None];
        let mut visited = std::collections::HashSet::new();
        let mut asked = 0_usize;
        while let Some(parent) = under.pop() {
            if !visited.insert(parent.clone()) {
                continue;
            }
            asked = asked.saturating_add(1);
            assert!(
                asked <= LEVELS,
                "{provider} lists more levels than a tree has"
            );
            let page = self
                .remote
                .query(
                    &self.caller,
                    tilo_actions::remote::CatalogueQuery {
                        provider: Some(provider.to_owned()),
                        parent_resource: parent,
                        max_results: 500,
                        ..tilo_actions::remote::CatalogueQuery::default()
                    },
                )
                .await
                .expect("the device link answers")
                .unwrap_or_else(|error| panic!("{provider} is listed: {error}"));
            for row in page.rows {
                if row.has_children {
                    under.push(Some(row.resource.id.clone()));
                }
                if let Some(action) = row.action {
                    listed.push(action);
                }
            }
        }
        listed
    }
}

/// A new person, admitted to `workspace` by an invitation `owner` (a device of its owner's)
/// hands out.
pub async fn guest(host: &host::Host, owner: &host::Member, role: &str, name: &str) -> Guest {
    let workspace = owner_workspace(host);
    let (key, cert) = host::stranger(name);
    // The owner invites them, and the invitation their device redeems is the answer's.
    let invited = owner
        .call(
            tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
            tilo_actions::remote::workspace_governance_address(workspace, "invite"),
            &json!({"role": role, "recipient": hex::encode(cert.user_public_key)}),
        )
        .await
        .expect("the owner invites");
    let InvocationUpdate::Succeeded { output, .. } = &invited else {
        panic!("the invitation is issued: {invited:?}");
    };
    let answered = output.value.clone().unwrap_or(Value::Null);
    let signed = hex::decode(
        answered["invitation"]
            .as_str()
            .unwrap_or_else(|| panic!("the invitation is in the answer: {answered}")),
    )
    .unwrap();
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &signed,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let endpoint = host::endpoint(&key).await;
    tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            signed,
            cert.clone(),
            &key,
            cert.device_public_key,
        )
        .await
        .unwrap_or_else(|error| panic!("{name} joins: {error}"));
    // Their device answers under its certificate, as a daemon does once it holds one, and signs
    // the invocations it sends with it.
    let service = ActionService::new(key.clone(), Arc::new(NoActions));
    service.answer_under(cert.clone());
    let remote = Arc::new(service.remote(&endpoint, &host.daemon().endpoint_addr(), workspace));
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
    Guest {
        remote,
        caller,
        _service: service,
        _endpoint: endpoint,
    }
}
