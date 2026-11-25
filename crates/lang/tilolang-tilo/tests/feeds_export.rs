//! Feeds publishes a library's subscriptions as an OPML export action on the library. The
//! system's one Export… lists it; Feeds keeps no export menu of its own.

#[path = "support/export_probe.rs"]
mod export_probe;

use tilo_render_protocol::patch::Level;

use export_probe::{export_formats, window};

const FEEDS_APP: &str = "so.tilo.feeds";

#[test]
fn a_feed_library_lists_the_format_it_exports() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::FEEDS];
    let (mut instance, _far, _device) = window(FEEDS_APP, PACKAGES);
    let host = instance.host().expect("the host");
    host.root_of(FEEDS_APP).expect("the library");
    instance.update();
    let view = instance.view(Level::Root).expect("the library's window");
    let tree = view.borrow().tree().clone();

    assert_eq!(
        export_formats(&tree),
        ["OPML"],
        "the library's bar lists the format Feeds publishes for it"
    );
}
