//! A bounded read of the same private cache that Quick Settings paints.
//! This does not initiate discovery or assert freshness of a kernel reading.
use super::{root::QuickSettingsRoot, wifi::QuickSettingsWifiPane};
use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use settings_store::hardware::QuickSettingsControl;

pub(crate) fn network(state: &ChromeState) -> Result<serde_json::Value, &'static str> {
    let cache = &state.quick_settings;
    if cache.wifi_networks.len() > 256
        || cache.wifi_networks.iter().any(|ap| ap.ssid.len() > 1024)
        || cache
            .wifi_scan_error
            .as_ref()
            .is_some_and(|error| error.len() > 4096)
    {
        return Err("quick-settings-network-inspect-limit");
    }
    let listed = QuickSettingsWifiPane::displayed_network_count(cache);
    let networks: Vec<_> = cache
        .wifi_networks
        .iter()
        .enumerate()
        .map(|(index, ap)| {
            serde_json::json!({
                "index": index, "ssid": ap.ssid, "signal": ap.signal,
                "secure": ap.secure, "connected": ap.connected, "listed": index < listed,
            })
        })
        .collect();
    let value = serde_json::json!({
        "source": "painted-cache", "revision": state.revision,
        "quick_settings_open": state.overlays.quick_settings,
        "network_control": {
            "label": QuickSettingsRoot::rendered_label(QuickSettingsControl::Wifi, cache),
            "subline": QuickSettingsRoot::subline(QuickSettingsControl::Wifi, cache),
            "checked": QuickSettingsRoot::on(QuickSettingsControl::Wifi, cache),
        },
        "wifi_panel": {
            "label": t("qs-wifi"),
            "status": QuickSettingsWifiPane::status_message(cache),
            "description": QuickSettingsWifiPane::status_message(cache).unwrap_or_else(|| t("qs-wifi-choose-a11y")),
            "listed_network_count": listed,
            "open": state.overlays.quick_settings && matches!(cache.nav_stack.last(), Some(super::QsLevel::Wifi)),
        },
        "wifi_available": cache.radios.wifi_available, "wifi_enabled": cache.wifi,
        "radio_wifi_enabled": cache.radios.wifi_enabled,
        "airplane_mode": cache.radios.airplane_mode, "hotspot_active": cache.radios.hotspot_active,
        "wired": cache.wired.as_ref().map(|wired| serde_json::json!({"device": wired.device,"connected": wired.connected})),
        "radio_change_pending": cache.radio_change_pending, "scanning": cache.wifi_scanning,
        "scan_error": cache.wifi_scan_error, "joining": cache.wifi_joining,
        "password_prompt_present": cache.wifi_password.is_some(),
        "networks": networks,
    });
    if serde_json::to_vec(&value)
        .map_err(|_| "quick-settings-network-inspect-limit")?
        .len()
        > 128 * 1024
    {
        return Err("quick-settings-network-inspect-limit");
    }
    Ok(value)
}
