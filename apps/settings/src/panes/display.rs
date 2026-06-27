//! Display pane — brightness, scale, monitor layout.
//!
//! Stub today. The brightness slider is the headline control once the
//! shell-side `qs_actions::display::set_brightness` binding lands; the
//! input here will then bind via `with_on_change_path` against the
//! shared CRDT key the quick-settings panel already mutates.
//
// TODO(qs_actions): wire to `shell::qs_actions::display::*`
//   - `current_brightness() -> f32`  (0.0..=1.0)
//   - `set_brightness(value: f32)`
//   - `connected_outputs() -> Vec<OutputInfo>`
// Until then this pane shows the `Coming soon` placeholder so the
// skeleton remains useful for snapshotting and shell-side scaffolding
// without fabricating a binding that doesn't exist.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Brightness, scale and monitor layout will appear here once \
         shell::qs_actions::display is wired in.",
    )
}
