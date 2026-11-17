//! The records every app of the workspace opens beside its own (the workspace's
//! emoji), as a page hands them to the windows it mounts.

use std::cell::RefCell;
use std::rc::Rc;

use ui::permissions::MemberId;
use ui::resource::workspace::WorkspaceResources;
use ui::resource::{ResourceOwners, ResourceStore};

/// Each record in its owner's store of `workspace`: the one a window of the owner
/// opened, or opened now and kept among `stores`, the stores the page flushes
/// and replicates, so the record reaches every window of the workspace and every
/// peer. In a `replicated` workspace a store's records come from the
/// workspace's hosts ([`ResourceStore::ask_workspace_for_missing`]).
pub(crate) fn in_page(
    owners: &ResourceOwners,
    stores: &Rc<RefCell<Vec<ResourceStore>>>,
    workspace: &str,
    author: String,
    replicated: bool,
) -> WorkspaceResources {
    let (owners, stores, workspace) = (owners.clone(), Rc::clone(stores), workspace.to_owned());
    WorkspaceResources::new(MemberId::person(author), move |owner| {
        let home = (owner.to_owned(), workspace.clone());
        if let Some(store) = stores
            .borrow()
            .iter()
            .find(|store| store.app_and_home() == home)
        {
            return Ok(store.clone());
        }
        let store = owners.clone().open_beside(owner, &workspace)?;
        if replicated {
            store.ask_workspace_for_missing();
        }
        stores.borrow_mut().push(store.clone());
        Ok(store)
    })
}
