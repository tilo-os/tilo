//! The actions the host publishes as unavailable now.
//!
//! A Rust app publishes each of its resources' actions with a contract
//! (`tilo_actions::ActionContract`), and its availability says whether the action can run on
//! that resource in its current state. An action that cannot publishes
//! `Availability::Unavailable { reason }`, and the action layer refuses a press of it with that
//! reason. A view's control bound to such an action must say so before it is pressed, however
//! the view was written.
//!
//! The host that publishes the resources hands their actions' availability in
//! ([`crate::Runtime::set_unavailable_actions`]), keyed as a view names them: an action of an
//! app the runtime does not run is named by its id as that app's package declares it
//! (`Desktop/delete-workspace`); a verb a resource publishes without a package declaring it is
//! named by the resource and the verb ([`address_key`], `desktop:chrome#delete-workspace`).

use std::collections::BTreeMap;

/// What the host says of the actions a person can see now, by the key a view names each by: the
/// reason it cannot run, in the person's words ([`crate::unavailable`] module docs say which
/// key a view's action is held under).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unavailable(BTreeMap<String, String>);

impl Unavailable {
    /// Why the action keyed `action` cannot run now, as the host said it.
    #[must_use]
    pub fn get(&self, action: &str) -> Option<&str> {
        self.0.get(action).map(String::as_str)
    }

    /// Say that `action` cannot run now, and why.
    pub fn insert(&mut self, action: impl Into<String>, reason: impl Into<String>) {
        self.0.insert(action.into(), reason.into());
    }

    /// Whether the host has said nothing cannot run.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The key a verb a resource publishes is held under: the resource's id and the verb's, an
/// address (`desktop:chrome#delete-workspace`) as `tilo_actions::ActionAddress` writes one.
#[must_use]
pub fn address_key(resource: &str, action: &str) -> String {
    format!("{resource}#{action}")
}

/// The actions `resources` publish as unavailable now, each with the reason the resource gives:
/// keyed by [`address_key`].
#[must_use]
pub fn from_resources<'a>(
    resources: impl IntoIterator<Item = &'a tilo_actions::ResourceSpec>,
) -> Unavailable {
    fn collect(resource: &tilo_actions::ResourceSpec, out: &mut Unavailable) {
        for action in &resource.actions {
            if let tilo_actions::Availability::Unavailable { reason, .. } =
                &action.contract.availability
            {
                out.insert(
                    address_key(resource.id.as_str(), action.id.as_str()),
                    reason,
                );
            }
        }
        for child in &resource.children {
            collect(child, out);
        }
    }
    let mut out = Unavailable::default();
    for resource in resources {
        collect(resource, &mut out);
    }
    out
}
