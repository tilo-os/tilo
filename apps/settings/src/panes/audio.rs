//! Audio pane — output volume, mute, input device.
//
// TODO(qs_actions): wire to `shell::qs_actions::audio::*`
//   - `current_volume() -> f32`
//   - `set_volume(value: f32)`
//   - `toggle_mute()`
//   - `outputs() -> Vec<AudioDeviceInfo>` (for the device picker)
// Same `Coming soon` placeholder pattern as the other panes — see
// `display.rs` for the rationale.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Output volume, mute and input device selection land here once \
         shell::qs_actions::audio is wired in.",
    )
}
