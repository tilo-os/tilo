//! Bluetooth pane — paired devices, pairing flow, transfer history.
//
// TODO(qs_actions): wire to `shell::qs_actions::bluetooth::*`
//   - `devices() -> Vec<BtDevice>` (name, address, paired, connected)
//   - `pair(addr: &str)` / `unpair(addr: &str)`
//   - `connect(addr: &str)` / `disconnect(addr: &str)`
// Stub today.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Paired devices and the pairing flow will land here once \
         shell::qs_actions::bluetooth is wired in.",
    )
}
