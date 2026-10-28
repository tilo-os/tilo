//! The records every app of the room opens beside its own (the workspace's
//! emoji), as a page hands them to the windows it mounts.

use std::cell::RefCell;
use std::rc::Rc;

use ui::permissions::UserId;
use ui::record::workspace::WorkspaceRecords;
use ui::record::{RecordOwners, RecordStore};

/// Each record in its owner's store of `room`: the one a window of the owner
/// opened, or opened now and kept among `stores`, the stores the page flushes
/// and replicates, so the record reaches every window of the room and every
/// peer.
pub(crate) fn in_page(
    owners: &RecordOwners,
    stores: &Rc<RefCell<Vec<RecordStore>>>,
    room: &str,
    author: String,
) -> WorkspaceRecords {
    let (owners, stores, room) = (owners.clone(), Rc::clone(stores), room.to_owned());
    WorkspaceRecords::new(UserId(author), move |owner| {
        let home = (owner.to_owned(), room.clone());
        if let Some(store) = stores
            .borrow()
            .iter()
            .find(|store| store.package_and_home() == home)
        {
            return Ok(store.clone());
        }
        let store = owners.clone().open_beside(owner, &room)?;
        stores.borrow_mut().push(store.clone());
        Ok(store)
    })
}
