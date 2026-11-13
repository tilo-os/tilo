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
    ///
    /// A resource is named here as the runtime names it on the tree it draws: a host publishes a
    /// resource by the address its catalogue gives (`chat.workspace/channels/c_general`), while
    /// the runtime reads every reference as its document and item
    /// ([`ResourceRef::id_string`](crate::value::ResourceRef::id_string)), so both sides are
    /// flattened to that one form — the same form a view's press looks the verb up with.
    #[must_use]
    pub fn get(&self, resource: &str, action: &str) -> Option<&PublishedAction> {
        self.0.get(&crate::value::flat_id(resource))?.get(action)
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
        let resource = crate::value::flat_id(&resource.into());
        self.0
            .entry(resource)
            .or_default()
            .insert(action.into(), published);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host publishes a channel by its catalogue address, and the runtime names the same
    /// resource on its tree by the document and item form: the verb is found by either, so a
    /// press is the channel's `post` however the address is written.
    #[test]
    fn a_verb_is_found_by_the_address_the_host_publishes_and_the_reference_the_tree_names() {
        let mut table = Published::default();
        table.insert(
            "chat.workspace/channels/c_general",
            "post",
            PublishedAction {
                app: "chat".to_owned(),
                params: vec!["channel".to_owned(), "text".to_owned()],
            },
        );
        assert!(
            table.get("chat.workspace/c_general", "post").is_some(),
            "the reference the tree names the channel by"
        );
        assert!(
            table
                .get("chat.workspace/channels/c_general", "post")
                .is_some(),
            "the catalogue address itself"
        );
        assert!(
            table.get("chat.workspace/other", "post").is_none(),
            "another item of the document is no channel"
        );
    }

    /// A resource published by a colon address (`chat.workspace.channels:c_crit`, a reading
    /// fixture's spelling) reads the same: the flattening leaves it alone.
    #[test]
    fn an_address_flattening_to_itself_is_published_as_it_stands() {
        let mut table = Published::default();
        table.insert(
            "chat.workspace.channels:c_crit",
            "post",
            PublishedAction {
                app: "chat".to_owned(),
                params: Vec::new(),
            },
        );
        assert!(
            table
                .get("chat.workspace.channels:c_crit", "post")
                .is_some(),
            "the colon address is its own reference"
        );
    }
}
