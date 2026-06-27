//! Network pane — known Wi-Fi networks, active connection, ethernet.
//
// TODO(qs_actions): wire to `shell::qs_actions::network::*`
//   - `wifi_networks() -> Vec<NetworkInfo>` (SSID, signal, secured)
//   - `connect(ssid: &str, secret: Option<&str>)`
//   - `disconnect(ssid: &str)`
//   - `ethernet_state() -> EthernetState`
// Today the pane is a `Coming soon` stub.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Known and visible Wi-Fi networks will be listed here once \
         shell::qs_actions::network is wired in.",
    )
}
