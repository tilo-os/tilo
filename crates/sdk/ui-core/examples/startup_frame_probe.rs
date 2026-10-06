//! Synthetic native SDK application for the sandboxed startup timeline.
//! This is a test payload, not an installed system app or an enrollment path.

use tilo_ui::{App, State, wake::WakeScope, widget::Text};

fn main() {
    tilo_startup_metrics::init();
    tilo_startup_metrics::mark("probe-main-start");
    let wake_scope = WakeScope::new();
    let _entry = wake_scope.enter();
    let app = App::from_scoped_parts(
        wake_scope,
        State::new(),
        Box::new(Text::new("Startup frame probe")),
    )
    .with_window_app_id("io.tilo.startup-frame-probe")
    .without_native_document_chrome();
    tilo_startup_metrics::mark("probe-app-ready");
    app.run();
    tilo_startup_metrics::dump();
}
