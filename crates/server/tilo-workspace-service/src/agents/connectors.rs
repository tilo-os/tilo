//! The connectors a device runs: what they report, what a run sends out through one, and the
//! secret the worker signs in with.
//!
//! What a connector reports (a message posted in a channel it bridges) is a trigger source the
//! device's trigger runner reads ([`crate::apps::triggers::ConnectorEvents`]).
//!
//! A shortcut's step that reaches an app whose connector runs here queues an outbox entry in the
//! app's record. The device that runs the connector hands each entry to its worker once
//! workspace-wide, in the order they were queued, and settles it in the record through the
//! device's dispatch with what the service answered ([`EffectDrain`]), so whoever reads the
//! record reads the result and nothing is sent twice.
//!
//! A connector's secret is kept in the workspace's one vault, never in a record and never in what
//! a program or an agent reads through it: the worker names the item it needs and the host
//! releases it there ([`Connectors::secrets`]).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use accounts::vault_items::{ItemKind, NewItem};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::{self, ItemRef};
use tilo_automation::grant::OUTBOX_FIELD;
use tilo_secrets::{Protection, SecretBackend, SecretError, SecretStore};

use serde_json::{Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionProvider, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_automerge::items::OutboxRow;

use crate::apps::triggers::{ConnectorEvents, ReportedEvent};
use crate::work::{NotTaken, Turn, keep_done, takes_turn};
use crate::workspaces::{ResourceWrite, Workspaces};

/// Why a connector did not perform an op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PerformError {
    Refused(String),
    /// The service or worker is unavailable now; it may succeed later.
    Unavailable(String),
}

/// A connector's worker: it performs one queued outbox entry, and reports what its service
/// told it.
pub trait Performer: Send + Sync {
    /// Perform `queued` (the entry's value) and answer the service's result.
    fn perform(&self, queued: Value) -> Result<Value, PerformError>;

    /// The events the service reported after `cursor`, oldest first.
    fn events_after(&self, cursor: u64) -> Vec<ReportedEvent>;
}

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

    /// The outbox drain of the connectors that run here: entries queued in the records of
    /// `workspaces` reach their workers, and are settled through `records`.
    #[must_use]
    pub fn drain(&self, workspaces: Workspaces, records: Arc<dyn EffectRecords>) -> EffectDrain {
        EffectDrain {
            workspaces,
            connectors: self.clone(),
            records,
            queued: BTreeSet::new(),
        }
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

impl ConnectorEvents for Connectors {
    fn since(&self, app: &str, cursor: u64) -> Vec<ReportedEvent> {
        self.worker(app)
            .map(|worker| worker.events_after(cursor))
            .unwrap_or_default()
    }
}

/// How often the drain looks again at the records whose entries could not all be sent.
const AGAIN: Duration = Duration::from_secs(1);

/// What reading and settling an outbox answers.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The records the outbox drain reads entries from and settles them in.
pub trait EffectRecords: Send + Sync {
    /// The entries queued at the outbox of record `record` of `app` in `workspace`, in the order
    /// they were queued, settled ones included.
    fn entries<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        record: &'a str,
    ) -> Answer<'a, Vec<OutboxRow>>;

    /// Settle entry `entry` of that outbox with `outcome`: `{"state": "done", "result": ..}` or
    /// `{"state": "failed", "result": {"reason": ..}}`.
    fn settle<'a>(
        &'a self,
        place: (&'a str, &'a str, &'a str),
        entry: &'a str,
        outcome: Value,
    ) -> Answer<'a, Result<(), String>>;
}

/// The records of the workspaces this device keeps, settled through its dispatch as the person
/// the device acts for: each settle is an ordinary field edit of the record, decided as any call
/// of theirs is.
pub struct DeviceRecords {
    pub workspaces: Workspaces,
    pub actions: Arc<dyn ActionProvider + Send + Sync>,
}

impl EffectRecords for DeviceRecords {
    fn entries<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        record: &'a str,
    ) -> Answer<'a, Vec<OutboxRow>> {
        Box::pin(async move {
            let Ok(Some(bytes)) = self
                .workspaces
                .resource_document(workspace, app, record)
                .await
            else {
                return Vec::new();
            };
            let Ok(doc) = automerge::AutoCommit::load(&bytes) else {
                tracing::warn!(%workspace, %app, %record, "a record with an outbox does not load; nothing of it is sent");
                return Vec::new();
            };
            tilo_automerge::items::outbox_entries(&doc, &doc.committed_heads(), OUTBOX_FIELD)
        })
    }

    fn settle<'a>(
        &'a self,
        (workspace, app, record): (&'a str, &'a str, &'a str),
        entry: &'a str,
        outcome: Value,
    ) -> Answer<'a, Result<(), String>> {
        let person = hex::encode(self.workspaces.serving_certificate().user_public_key);
        let caller = Caller::local_person(person).in_workspace(workspace);
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document: record.to_owned(),
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_agent_session::resource_provider(&route),
                address: ActionAddress::new(record, tilo_resource::FIELD_EDIT),
            },
            argument: InvokeArgument::Json(tilo_resource::outbox_settle_arguments(
                OUTBOX_FIELD,
                entry,
                &outcome,
            )),
            on_behalf_of: None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let actions = Arc::clone(&self.actions);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let receipt = actions
                    .invoke(&caller, request)
                    .wait_timeout(SETTLE_WAIT)
                    .map_err(|_| "the record did not take the settle in time".to_owned())?
                    .ok_or("the record dropped the settle")?
                    .map_err(|error| error.to_string())?;
                match receipt.status {
                    ReceiptStatus::Failed { error } => Err(error.message),
                    _ => Ok(()),
                }
            })
            .await
            .map_err(|error| error.to_string())?
        })
    }
}

/// How long a settle may take before the drain tries it again.
const SETTLE_WAIT: Duration = Duration::from_secs(30);

/// The outbox drain of a device's connectors ([`Connectors::drain`]).
pub struct EffectDrain {
    workspaces: Workspaces,
    connectors: Connectors,
    records: Arc<dyn EffectRecords>,
    /// The records whose outbox was not drained yet, by workspace, app and record.
    queued: BTreeSet<(String, String, String)>,
}

impl EffectDrain {
    /// Drain until the workspaces' service stops.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        loop {
            let mut written = Vec::new();
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => {
                        written.push(write);
                        while let Ok(more) = writes.try_recv() {
                            written.push(more);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                () = tokio::time::sleep(AGAIN) => {}
            }
            self.pass(written).await;
        }
    }

    /// Look at the outboxes of the records `written` and of those not drained yet: send each
    /// entry not settled, in order, until one cannot be sent now.
    pub async fn pass(&mut self, written: Vec<ResourceWrite>) {
        for write in written {
            if !write.record.is_empty() && self.connectors.runs(&write.app) {
                self.queued
                    .insert((write.workspace, write.app, write.record));
            }
        }
        for place in self.queued.clone() {
            if self.drained(&place).await {
                self.queued.remove(&place);
            }
        }
    }

    /// Send what record `place` queued: whether nothing of it is left to send.
    async fn drained(&self, (workspace, app, record): &(String, String, String)) -> bool {
        let Some(worker) = self.connectors.worker(app) else {
            return true;
        };
        let place = (workspace.as_str(), app.as_str(), record.as_str());
        let entries = self.records.entries(workspace, app, record).await;
        for row in entries.into_iter().filter(|row| row.settled.is_none()) {
            // One entry, one turn: two devices that run the connector never send it twice.
            let turn = Turn::Send {
                app: app.clone(),
                record: record.clone(),
                entry: row.id.clone(),
            };
            let outcome = match takes_turn(&self.workspaces, workspace, &turn).await {
                // Sent before, by this device or another, and not settled yet: settled with
                // what the service answered then.
                Err(NotTaken::Done(outcome)) => outcome,
                Err(_) => return false,
                Ok(claim) => {
                    let performer = Arc::clone(&worker);
                    let value = row.value.clone();
                    let performed = tokio::task::spawn_blocking(move || performer.perform(value))
                        .await
                        .unwrap_or_else(|error| Err(PerformError::Unavailable(error.to_string())));
                    let outcome = match performed {
                        Ok(result) => json!({"state": "done", "result": result}),
                        Err(PerformError::Refused(reason)) => {
                            json!({"state": "failed", "result": {"reason": reason}})
                        }
                        Err(PerformError::Unavailable(reason)) => {
                            // What follows it waits too: the entries go out in order.
                            tracing::info!(%app, %record, entry = %row.id, %reason, "a connector could not send an entry now; it is sent again later");
                            claim.release().await;
                            return false;
                        }
                    };
                    if let Err(error) =
                        keep_done(&self.workspaces, workspace, &turn, outcome.clone()).await
                    {
                        tracing::warn!(%app, %record, entry = %row.id, %error, "a sent entry was not kept done");
                    }
                    claim.release().await;
                    outcome
                }
            };
            if let Err(error) = self.records.settle(place, &row.id, outcome).await {
                tracing::warn!(%app, %record, entry = %row.id, %error, "a sent entry was not settled; it is settled again later");
                return false;
            }
        }
        true
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
