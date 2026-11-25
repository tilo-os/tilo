//! Calendar publishes the whole calendar as an iCalendar export action on the calendar. The
//! system's one Export… lists it; Calendar keeps no export menu of its own.

use tilo_render_protocol::patch::Level;

use crate::export_probe::{export_formats, window};

const CALENDAR_APP: &str = "so.tilo.calendar";

#[test]
fn a_calendar_lists_the_format_it_exports() {
    static PACKAGES: &[(&str, &str)] = &[
        crate::fixtures::CONTACTS,
        crate::fixtures::MAIL,
        crate::fixtures::CALENDAR,
    ];
    let (mut instance, _far, _device) = window(CALENDAR_APP, PACKAGES);
    let host = instance.host().expect("the host");
    host.root_of(CALENDAR_APP).expect("the calendar");
    instance.update();
    let view = instance.view(Level::Root).expect("the calendar's window");
    let tree = view.borrow().tree().clone();

    assert_eq!(
        export_formats(&tree),
        ["iCalendar"],
        "the calendar's bar lists the format Calendar publishes for it"
    );
}
