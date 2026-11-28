//! Interactive dashboard: a dense window of panels to eyeball incremental
//! rendering in.
//! Run with:
//! ```
//! cargo run -p ui-examples --release --example ui-lab -- dashboard
//! ```

use tilo_ui_app::App;
use tilo_ui_base::i18n::I18nManager;
use tilo_ui_model::State;

use super::dashboard_scene;

pub fn run(_args: &[String]) {
    let wake_scope = tilo_ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    I18nManager::init("en-US");
    let app = App::from_scoped_parts(wake_scope, State::new(), dashboard_scene::build());
    app.run();
}
