//! The resources this device's daemon publishes to the apps attached to it: each served by the
//! provider that keeps it, behind one registration.
//!
//! A provider answers the resources it knows and refuses the rest; an app attached to the device
//! reads, watches and invokes them through this one table, so a daemon adds a reading by
//! registering the provider that serves it, never by growing a second channel.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tokio::sync::{mpsc, oneshot};

/// The providers a daemon publishes through, by the resources each answers.
#[derive(Default)]
pub struct Published {
    providers: Vec<(&'static [&'static str], std::sync::Arc<dyn PublishedResources>)>,
    /// The provider of every resource no other names: what the desktop session signed in here
    /// publishes, which it decides as it runs.
    rest: Option<std::sync::Arc<dyn PublishedResources>>,
    /// The watches this table minted, and the provider each was asked of: the numbers are this
    /// table's, so watches of two providers never collide.
    watches: Mutex<HashMap<u64, (std::sync::Arc<dyn PublishedResources>, ResourceWatch)>>,
    next_watch: AtomicU64,
}

impl Published {
    /// A table that serves nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The table with `provider` serving `resources`.
    #[must_use]
    pub fn serving(
        mut self,
        resources: &'static [&'static str],
        provider: std::sync::Arc<dyn PublishedResources>,
    ) -> Self {
        self.providers.push((resources, provider));
        self
    }

    /// The table with `provider` serving every resource no other provider names.
    #[must_use]
    pub fn serving_the_rest(mut self, provider: std::sync::Arc<dyn PublishedResources>) -> Self {
        self.rest = Some(provider);
        self
    }

    /// The provider that answers `resource`, where one does.
    fn provider(&self, resource: &str) -> Option<&std::sync::Arc<dyn PublishedResources>> {
        self.providers
            .iter()
            .find(|(served, _)| served.contains(&resource))
            .map(|(_, provider)| provider)
            .or(self.rest.as_ref())
    }
}

/// Why a resource is refused: no provider of this device keeps it.
fn unknown(resource: &str) -> String {
    format!("this device publishes no `{resource}` resource")
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        if let Some(provider) = self.provider(resource) {
            return provider.snapshot(caller, resource);
        }
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(Ok((0, None)));
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        let provider = self.provider(resource).ok_or_else(|| unknown(resource))?;
        let inner = provider.watch(caller, resource, sink)?;
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        self.watches
            .lock_recover()
            .insert(id, (std::sync::Arc::clone(provider), inner));
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        if let Some((provider, inner)) = self.watches.lock_recover().remove(&watch.id()) {
            provider.unwatch(inner);
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        if let Some(provider) = self.provider(resource) {
            return provider.invoke(caller, resource, action, arguments);
        }
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_resource".to_owned(),
            message: unknown(resource),
        });
        answered
    }
}

#[cfg(test)]
#[path = "published_tests.rs"]
mod tests;
