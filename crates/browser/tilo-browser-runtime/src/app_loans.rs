//! Services the bundle lends one app when the page mounts it, as a native
//! session lends them at its mount: Messages its messaging service. The
//! page provides every app its records and content itself; a service that
//! belongs to one app, and whose code the runtime does not carry, comes
//! from the bundle that links that app.

use std::cell::RefCell;

/// Lends `HostContext` the services of one app.
pub type Lend = fn(&mut ui::application::HostContext);

thread_local! {
    static LENDERS: RefCell<Vec<(&'static str, Lend)>> = const { RefCell::new(Vec::new()) };
}

/// Lend each window of `app_id` the page mounts what `lend` gives it,
/// after the page's own provisions (the person's records among them).
pub fn lend_on_mount(app_id: &'static str, lend: Lend) {
    LENDERS.with_borrow_mut(|lenders| lenders.push((app_id, lend)));
}

/// Give `context` what the bundle lends its app.
pub(crate) fn lend(context: &mut ui::application::HostContext) {
    let lenders: Vec<Lend> = LENDERS.with_borrow(|lenders| {
        lenders
            .iter()
            .filter(|(app, _)| *app == context.app_id())
            .map(|(_, lend)| *lend)
            .collect()
    });
    for lend in lenders {
        lend(context);
    }
}
