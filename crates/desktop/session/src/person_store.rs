//! The person's own workspace as this session keeps what is theirs alone in it: through the
//! device's daemon, which keeps the workspace ([`PersonStore`]).
//!
//! The session holds no copy of the workspace. It reads each resource from the daemon, asks the
//! daemon for each change as the resource's typed action, which the daemon makes, signs and seals
//! as this device, and hears of every write the daemon takes on the daemon's own feed of them.

use runtime_collab::wire::shell::{ShellCall, ShellReply};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RESOURCE_PROVIDER, ReceiptStatus, RemoteActionTarget,
};
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_session_client::daemon_documents::{
    DaemonShell, DaemonWritesFeed, document_read, listing, whole,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::{Answer, Authoring, Moved, PersonStore};

/// The person's own workspace, kept by the device's daemon this session reaches on `shell`.
pub(crate) struct DaemonPersonStore {
    shell: DaemonShell,
    writes: DaemonWritesFeed,
    certificate: DeviceCert,
    /// The person's workspace's id.
    workspace: String,
}

impl DaemonPersonStore {
    /// The person's workspace that `certificate`'s person keeps, as the daemon on `shell` keeps
    /// it and `writes` tells its writes.
    pub(crate) fn new(
        shell: DaemonShell,
        writes: DaemonWritesFeed,
        certificate: DeviceCert,
    ) -> Self {
        let workspace =
            tilo_server_protocol::servers::person_workspace_id(&certificate.user_public_key);
        Self {
            shell,
            writes,
            certificate,
            workspace,
        }
    }

    /// The resource `(app, name)` as the daemon holds it now, saved; `None` while it holds none.
    async fn saved(&self, app: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
        match document_read(&self.shell.read(whole(&self.workspace, app, name)).await?)? {
            DocumentRead::Whole { bytes, .. } => Ok(Some(bytes)),
            DocumentRead::Absent | DocumentRead::Pending => Ok(None),
            DocumentRead::Changes { .. } | DocumentRead::Unchanged(_) => {
                Err("the daemon answered a whole read with part of a resource".to_owned())
            }
        }
    }
}

impl PersonStore for DaemonPersonStore {
    fn certificate(&self) -> DeviceCert {
        self.certificate.clone()
    }

    fn held(&self) -> Answer<'_, bool> {
        Box::pin(async move {
            match self.shell.ask(ShellCall::Workspaces).await {
                Ok(ShellReply::Workspaces(held)) => held.contains(&self.workspace),
                Ok(other) => {
                    tracing::warn!(answer = ?other, "the daemon did not say which workspaces it keeps");
                    false
                }
                Err(error) => {
                    tracing::warn!(%error, "the daemon did not say which workspaces it keeps");
                    false
                }
            }
        })
    }

    fn routes(&self) -> Answer<'_, Result<Vec<(String, String)>, String>> {
        Box::pin(async move {
            let listing = self.shell.read(listing(&self.workspace)).await?;
            Ok(
                tilo_actions::remote::ListedResource::read_listing(&self.workspace, "", &listing)
                    .into_iter()
                    .map(|listed| (listed.address.app, listed.address.record))
                    .collect(),
            )
        })
    }

    fn document<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
    ) -> Answer<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(self.saved(app, name))
    }

    fn author(
        &self,
        app: String,
        name: String,
        authoring: Authoring,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            // The daemon builds, numbers and keeps every change this device makes to the person's
            // records: a change built here from what was last read would take the number the
            // daemon's own next change takes, and the workspace would refuse one of the two.
            let Some(call) = authoring.action else {
                return Err(format!(
                    "{app} {name} is changed through its actions, which the device's daemon makes"
                ));
            };
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: format!("{RESOURCE_PROVIDER}:{app}:{name}"),
                    address: call.address,
                },
                argument: InvokeArgument::Json(call.arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            let receipt = self
                .shell
                .invoke_receipt(Some(&self.workspace), request)
                .await?;
            match receipt.status {
                ReceiptStatus::Completed { .. } => Ok(()),
                ReceiptStatus::Rejected { error } => Err(error.to_string()),
                ReceiptStatus::Failed { error } => Err(error.message),
                status => Err(format!("the action did not settle: {status:?}")),
            }
        })
    }

    fn moves(&self) -> tokio::sync::mpsc::UnboundedReceiver<Moved> {
        use tokio::sync::broadcast::error::RecvError;
        let (tell, told) = tokio::sync::mpsc::unbounded_channel();
        let mut writes = self.writes.follow();
        let workspace = self.workspace.clone();
        self.shell.runtime().spawn(async move {
            loop {
                let moved = match writes.recv().await {
                    Ok(write) if write.workspace == workspace => {
                        Moved::Resource(write.app, write.record)
                    }
                    // A write that names no workspace is the feed falling behind: the daemon took
                    // writes it did not tell.
                    Ok(write) if write.workspace.is_empty() => Moved::Everything,
                    // Another workspace's write: whether the daemon holds this one may have moved
                    // with it, as a join or a founding writes its first resources.
                    Ok(_) => Moved::Standing,
                    Err(RecvError::Lagged(_)) => Moved::Everything,
                    Err(RecvError::Closed) => return,
                };
                if tell.send(moved).is_err() {
                    return;
                }
            }
        });
        told
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tilo_session_client::daemon_documents::inherited_link;
    use tilo_workspace_service::workspaces::PersonResources;
    use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

    /// Two parts of one device write the person's devices record at once, many times over, as the
    /// session's parts and the daemon's own do: every write lands, none refused. Each asks the daemon for its
    /// action and the daemon builds the change, so no two changes of the device's take one
    /// number, which two changes each built from the last read would.
    #[test]
    fn writes_of_the_persons_record_from_two_parts_of_one_device_all_land() {
        use std::os::unix::fs::PermissionsExt as _;
        const ROUNDS: u8 = 8;
        let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
        let (_identity, cert, _key) = crate::governance::tests::provisioned_identity(&mut home);
        let profile = tempfile::tempdir().expect("a profile for the daemon");
        std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the profile is this user's");
        let daemon = tilo_runtime::daemon::Running::start(profile.path())
            .expect("the device's daemon starts");
        let within = tilo_test_env::hang_bound();
        let runtime = tilo_rt::app().handle().clone();
        let link = inherited_link(
            Some(
                daemon
                    .link_shell(within)
                    .expect("the daemon makes the shell's link"),
            ),
            &runtime,
        );
        let shell = DaemonShell::new(link.map(|(link, _)| link), runtime.clone());
        let store =
            || DaemonPersonStore::new(shell.clone(), DaemonWritesFeed::start(&shell), cert.clone());
        let probe = store();
        runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + within;
            while !probe.held().await {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the daemon never held the person's workspace"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        let device = hex::encode(cert.device_public_key);
        let certificate =
            hex::encode(postcard::to_allocvec(&cert).expect("the certificate encodes"));
        let parts: Vec<(PersonResources, &str)> = ["first", "second"]
            .into_iter()
            .map(|fact| {
                let part = PersonResources::default();
                part.attach(std::sync::Arc::new(store()), &runtime);
                (part, fact)
            })
            .collect();
        for round in 1..=ROUNDS {
            for (part, fact) in &parts {
                let keep = Devices::KEEP_FACT.with((
                    device.clone(),
                    certificate.clone(),
                    format!("{fact}-{round}"),
                    format!("{round:02x}"),
                ));
                part.apply(DEVICES, move || keep.clone());
            }
        }
        let written: Vec<String> = (1..=ROUNDS)
            .flat_map(|round| parts.iter().map(move |(_, fact)| format!("{fact}-{round}")))
            .collect();
        let kept = |part: &PersonResources| {
            part.read::<Devices>(DEVICES)
                .and_then(|devices| {
                    devices
                        .devices
                        .get(&tilo_resource::Id::from_raw(device.clone()))
                        .map(|kept| kept.facts.clone())
                })
                .unwrap_or_default()
        };
        runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + within;
            loop {
                let facts = kept(&parts[0].0);
                if written.iter().all(|fact| facts.contains_key(fact)) {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "a part's write was refused or never landed: the record says {facts:?}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        drop(parts);
        drop(daemon);
    }
}
