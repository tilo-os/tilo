//! The connectors a workspace's host runs beside its programs: what a run sends out through one,
//! and the secret the worker signs in with.
//!
//! A program's step that reaches an app whose connector runs here queues an outbox entry in the
//! app's record. The run's API hands each entry to the connector's worker and settles it as the
//! program with what the service answered, so the step reads the result and nothing is sent
//! twice ([`Connectors::outboxes`], [`super::programs::api::ProgramApi`]).
//!
//! A connector's secret is kept in the workspace's one vault, never in a record and never in what
//! a program or an agent reads through it: the worker names the item it needs and the host
//! releases it there ([`Connectors::secrets`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use accounts::vault_items::{ItemKind, NewItem};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::{self, ItemRef};
use tilo_automation::grant::OUTBOX_FIELD;
use tilo_secrets::{Protection, SecretBackend, SecretError, SecretStore};

use super::programs::api::{OutboxSpec, Performer};
use crate::workspaces::Workspaces;

/// The secret a connector's worker makes for itself: one webhook per channel.
const WEBHOOK: &str = "webhook";

/// The connectors this host runs, by the app whose worker performs their outside calls.
#[derive(Clone, Default)]
pub struct Connectors {
    workers: BTreeMap<String, Arc<dyn Performer>>,
}

impl Connectors {
    /// No connector runs here.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// [`Self::new`], with `app`'s worker run here too.
    #[must_use]
    pub fn running(mut self, app: &str, worker: Arc<dyn Performer>) -> Self {
        self.workers.insert(app.to_owned(), worker);
        self
    }

    /// The worker of `app`, when this host runs one.
    #[must_use]
    pub fn worker(&self, app: &str) -> Option<Arc<dyn Performer>> {
        self.workers.get(app).cloned()
    }

    /// Whether this host runs `app`'s connector.
    #[must_use]
    pub fn runs(&self, app: &str) -> bool {
        self.workers.contains_key(app)
    }

    /// The outboxes a program's API drains: one for each connector, over the field its app's
    /// records queue their outside effects at.
    #[must_use]
    pub fn outboxes(&self) -> Vec<OutboxSpec> {
        self.workers
            .iter()
            .map(|(app, worker)| OutboxSpec {
                app_id: app.clone(),
                field: OUTBOX_FIELD.to_owned(),
                connector: Arc::clone(worker),
            })
            .collect()
    }

    /// The secret store connector `app`'s worker reads: the workspace's vault, opened as the
    /// device that hosts the workspace, so a secret is kept where people keep theirs.
    ///
    /// Called on the runtime the host serves its workspaces on, as the vault is read from the
    /// worker's own thread.
    #[must_use]
    pub fn secrets(&self, workspaces: &Workspaces, workspace: &str, app: &str) -> SecretStore {
        let person = hex::encode(workspaces.serving_certificate().user_public_key);
        let by = Caller::local_person(person).in_workspace(workspace);
        SecretStore::with_backend(
            app,
            Arc::new(VaultSecrets {
                workspaces: workspaces.clone(),
                workspace: workspace.to_owned(),
                app: app.to_owned(),
                by,
                handle: tokio::runtime::Handle::current(),
            }),
        )
    }
}

/// The workspace's vault as a connector's worker reads it: the item a purpose and the service's
/// own id name, never a record's field and never anything a program reads.
struct VaultSecrets {
    workspaces: Workspaces,
    workspace: String,
    app: String,
    /// The device the connector acts for, as the vault releases an item to it.
    by: Caller,
    handle: tokio::runtime::Handle,
}

impl VaultSecrets {
    /// The title the vault keeps connector `app`'s secret `purpose` for `item` under: what
    /// Passwords lists it as, so a person reads where a secret they did not type themselves came
    /// from.
    fn title(&self, purpose: &str, item: &str) -> String {
        person_vault::connector_title(&self.app, purpose, item)
    }

    /// The entry the vault keeps for connector `app` under `title`.
    async fn entry(&self, title: &str) -> Option<accounts::vault_items::ItemId> {
        let opened = self.workspaces.open_vault(&self.workspace).await.ok()?;
        opened
            .items()
            .facts()
            .into_iter()
            .find(|facts| facts.title == title)
            .map(|facts| facts.id)
    }
}

impl SecretBackend for VaultSecrets {
    fn protection(&self) -> Protection {
        // The workspace's vault is a document sealed under the workspace's own keys, held by the
        // devices that may read it.
        Protection::LocalFile
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<tilo_secrets::Secret>, SecretError> {
        let Some((purpose, item)) = key.split_once('/') else {
            return Err(SecretError::InvalidKey);
        };
        if app != self.app || !known_purpose(purpose) {
            return Ok(None);
        }
        let title = self.title(purpose, item);
        let released = self.handle.block_on(async {
            let id = self.entry(&title).await?;
            let reference = ItemRef::from_id(id.as_str())?;
            self.workspaces
                .workspace_secret(
                    &self.workspace,
                    &reference,
                    &self.by,
                    "the connector's secret",
                )
                .await
                .ok()
        });
        Ok(released.map(|secret| tilo_secrets::Secret::new(secret.as_bytes().to_vec())))
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        let Some((purpose, item)) = key.split_once('/') else {
            return Err(SecretError::InvalidKey);
        };
        // Only what the worker made for itself is kept here: the secret it signs in with is the
        // person's to put in the vault, and nothing else writes into a person's vault.
        if app != self.app || purpose != WEBHOOK {
            return Err(SecretError::Denied);
        }
        let secret = String::from_utf8(value.to_vec()).map_err(|_| SecretError::Denied)?;
        let title = self.title(purpose, item);
        let place = accounts::vault_items::Place::parse(&person_vault::connector_place(&self.app))
            .map_err(|_| SecretError::Denied)?;
        self.handle.block_on(async {
            self.workspaces
                .changing_vault(&self.workspace, |items| {
                    let kept = items.facts().into_iter().find(|facts| facts.title == title);
                    let adding = NewItem::new(ItemKind::Value, vec![place], String::new(), secret);
                    match kept {
                        Some(facts) => items.keep(Some(&facts.id), adding, &title),
                        None => items.keep(None, adding, &title),
                    }
                    .map(drop)
                })
                .await
                .map_err(SecretError::Failed)
        })
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        let Some((purpose, item)) = key.split_once('/') else {
            return Err(SecretError::InvalidKey);
        };
        if app != self.app || !known_purpose(purpose) {
            return Ok(());
        }
        let title = self.title(purpose, item);
        self.handle.block_on(async {
            self.workspaces
                .changing_vault(&self.workspace, |items| {
                    if let Some(facts) =
                        items.facts().into_iter().find(|facts| facts.title == title)
                    {
                        items.remove(&facts.id);
                    }
                    Ok(())
                })
                .await
                .map_err(SecretError::Failed)
        })
    }
}

/// Whether a connector's worker may ask for `purpose`.
#[must_use]
fn known_purpose(purpose: &str) -> bool {
    person_vault::CONNECTOR_PURPOSES.contains(&purpose)
}

#[cfg(test)]
mod tests;
