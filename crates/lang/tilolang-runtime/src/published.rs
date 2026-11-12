//! Actions a resource publishes that no loaded package declares.
//!
//! An app's resources publish the verbs a person may run on them ([`ResourceSpec::actions`] in
//! `tilo-actions`), and an app the session serves publishes them without a package ever loading
//! here: Chat's channel is a record the session keeps, its `send` is a verb the session's own
//! service answers. A Rust view (and a tilolang one) names such a verb on the resource it shows
//! by its published id — `cx.action(node, "press", "send")` — and the runtime sends it to the app
//! that publishes it, exactly as it sends a declared action of another app to that app's door.
//!
//! The host hands the table in ([`crate::Runtime::set_published_actions`]) from the resources it
//! publishes, so the runtime never guesses an address: an action id a loaded package declares is
//! that package's, and one only the table names is the resource's own.
//!
//! [`ResourceSpec::actions`]: https://docs.rs/tilo-actions

use std::collections::BTreeMap;

/// A verb a resource publishes, as the app publishing it runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedAction {
    /// The app whose door runs it (`chat`): what an invocation is addressed to.
    pub app: String,
    /// The names of the arguments it takes after the resource itself, in the order it declares
    /// them (`channel`, `text`): what an invocation made from a view's arguments is sent by.
    pub params: Vec<String>,
}

/// Every resource's published verbs, by the resource's id and then the verb's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Published(BTreeMap<String, BTreeMap<String, PublishedAction>>);

impl Published {
    /// The action `action` as the resource `resource` publishes it, when it does.
    #[must_use]
    pub fn get(&self, resource: &str, action: &str) -> Option<&PublishedAction> {
        self.0.get(resource)?.get(action)
    }

    /// Whether anything is published: a host that publishes no resource of its own has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Take `resource`'s published verbs, each with the app that runs it and the arguments it
    /// takes after the resource.
    pub fn insert(
        &mut self,
        resource: impl Into<String>,
        action: impl Into<String>,
        published: PublishedAction,
    ) {
        self.0
            .entry(resource.into())
            .or_default()
            .insert(action.into(), published);
    }
}
