//! Contacts publishes both cards its book can be written as an export action on the book: the
//! current vCard 4.0, and the older 3.0 an address book that reads no 4.0 card opens. The
//! system's one Export… lists them; Contacts keeps no export menu of its own.

#[path = "support/export_probe.rs"]
mod export_probe;

use tilo_render_protocol::patch::Level;

use export_probe::{export_formats, window};

const CONTACTS_APP: &str = "so.tilo.contacts";

#[test]
fn a_contacts_book_lists_the_card_formats_it_exports() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::CONTACTS];
    let (mut instance, _far, _device) = window(CONTACTS_APP, PACKAGES);
    let host = instance.host().expect("the host");
    host.root_of(CONTACTS_APP).expect("the book");
    instance.update();
    let view = instance.view(Level::Root).expect("the book's window");
    let tree = view.borrow().tree().clone();

    // The book's bar lists both cards it can write, together under the system's one Export….
    assert_eq!(
        export_formats(&tree),
        ["vCard 4.0", "vCard 3.0"],
        "the book's bar lists the formats Contacts publishes for it"
    );
}
