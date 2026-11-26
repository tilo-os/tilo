//! What a workspace's owner alone keeps of it, as a record of the workspace itself
//! ([`Ownership`]: what they labelled it with, and whether keys reaching it alone run commands in
//! its machine unasked), which no role but the owner's reads or changes
//! (`tilo-server-protocol`'s `workspaces::OWNERSHIP`). A member's membership, the role they have
//! and when the workspace was made, is no record: it reads from the workspace's member list
//! ([`tilo_server_protocol::workspaces::MEMBERSHIP_APP`]).
//!
//! A host that holds the workspace keeps it, whether or not it holds its owner's own workspace.
//! Every listing of a person's workspaces joins each entry with both.

use std::collections::BTreeMap;

use tilo_resource::{Model, Refusal, resource};

/// The record of what the workspace's owner alone keeps of it.
pub const OWNERSHIP: (&str, &str) = tilo_server_protocol::workspaces::OWNERSHIP;

/// The most labels one workspace carries.
const MAX_LABELS: usize = 64;
/// The longest name or value of a label, bytes.
const MAX_LABEL: usize = 256;

/// What the workspace's owner alone keeps of it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ownership {
    /// What they labelled it with (`customer_id` → `cus_8121`), for their programs to find it by.
    pub labels: BTreeMap<String, String>,
    /// Keys reaching this workspace alone run commands in its machine without asking them each
    /// time. Off until they turn it on.
    pub unattended: bool,
}

#[resource(
    kind = "tilo.ownership",
    readers = "author",
    read_content = tilo_resource::read::fields
)]
impl Ownership {
    /// Label the workspace with `labels`, a JSON object of names to values, in place of what it
    /// was labelled with before: an empty one takes them all off.
    #[action]
    pub fn relabel(&mut self, labels: String) -> Result<(), Refusal> {
        let labels: BTreeMap<String, String> = serde_json::from_str(&labels)
            .map_err(|_| Refusal::new("labels are names with a value each"))?;
        if labels.len() > MAX_LABELS
            || labels.iter().any(|(name, value)| {
                name.is_empty() || name.len() > MAX_LABEL || value.len() > MAX_LABEL
            })
        {
            return Err(Refusal::new(format!(
                "a workspace carries at most {MAX_LABELS} labels, each named in 1 to {MAX_LABEL} \
                 bytes with a value of at most {MAX_LABEL}"
            )));
        }
        self.labels = labels;
        Ok(())
    }

    /// Let keys reaching the workspace alone run commands in its machine unasked, or stop them.
    #[action]
    pub fn run_unattended(&mut self, on: bool) -> Result<(), Refusal> {
        self.unattended = on;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "a test failing is its assertion failing"
    )]

    use std::sync::Arc;

    use collab_types::store::wire::{Command, Reply};

    use super::{OWNERSHIP, Ownership};
    use crate::workspaces::fixtures::{TestGroupKeys, hosting_device, test_host_holding};
    use crate::workspaces::person_workspace;

    /// What a workspace's owner alone keeps of it is founded for them alone: they read it, and
    /// another person of the workspace reads none of it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_person_reads_none_of_what_the_owner_alone_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let keys = Arc::new(TestGroupKeys::default());
        let host = test_host_holding(dir.path(), &key, &cert, keys).await;
        let workspaces = host.workspaces().clone();
        let workspace = workspaces.create("Studio").await.unwrap().workspace;
        person_workspace::apply_on(
            &workspaces,
            &workspace,
            OWNERSHIP,
            Ownership::RELABEL.with(r#"{"customer_id":"cus_8121"}"#.to_owned()),
        )
        .await
        .unwrap();
        let get = Command::Get {
            kind: "tilo.ownership".to_owned(),
            id: OWNERSHIP.1.to_owned(),
        };
        let owner = workspaces
            .read_store_as(&workspace, &hex::encode(cert.user_public_key), &get)
            .await
            .unwrap();
        assert!(
            matches!(owner, Reply::Resource(Some(_))),
            "the owner reads what they keep"
        );
        let other = workspaces
            .read_store_as(&workspace, &hex::encode([7; 32]), &get)
            .await
            .unwrap();
        assert!(
            matches!(other, Reply::Resource(None)),
            "another person reads none of it"
        );
        drop(workspaces);
        host.shutdown().await;
    }
}
