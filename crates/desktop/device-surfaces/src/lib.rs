//! The device's resources as the device publishes them, as `so.tilo.device`: the sound, the
//! network, Bluetooth, the modem, VPN, tethering, the battery and the displays, each its reading
//! and the actions it offers, and the rows of a reading that are resources of their own (a Wi-Fi
//! network, a Bluetooth accessory, a VPN profile), each the actions offered on the address its row
//! carries.
//!
//! The readings, the effects and the surface of each resource live here, beside the ones the
//! machine's services publish outside the session (the sound, the connectivity, the system
//! services and the power), so a view of another language compiles against one table
//! ([`device_surfaces`]) without loading a session. `device-surface.json` beside this crate is
//! what [`device_surfaces`] publishes; the shell compiles against it and the test below holds it
//! to the declarations.

use tilo_resource::surface::Surface;

pub mod agents;
pub mod clipboard;
pub mod clock;
pub mod controls;
pub mod devices;
pub mod displays;
pub mod input;
pub mod inputs;
pub mod keyboard;
pub mod lists;
pub mod live;
pub mod notifications;
pub mod osd;
pub mod overlays;
pub mod people;
pub mod privacy;
pub mod quick_settings;
pub mod shares;
pub mod time;
pub mod windows;

/// The device's resources, as their surfaces: each reading and the actions it offers, in the order
/// the shell reads them.
#[must_use]
pub fn device_surfaces() -> Vec<Surface> {
    vec![
        tilo_audio_service::sound::surface(),
        tilo_connectivity_service::network_surface(),
        tilo_connectivity_service::wifi_network_surface(),
        tilo_connectivity_service::saved_network_surface(),
        tilo_connectivity_service::bluetooth_surface(),
        tilo_connectivity_service::bluetooth_accessory_surface(),
        tilo_connectivity_service::bluetooth_nearby_surface(),
        tilo_connectivity_service::modem_surface(),
        tilo_connectivity_service::vpn_surface(),
        tilo_connectivity_service::vpn_profile_surface(),
        tilo_connectivity_service::tethering_surface(),
        tilo_session_power::battery_surface(),
        tilo_session_power::battery_history_surface(),
        tilo_system_service::power_surface(),
        tilo_system_service::printers_surface(),
        tilo_system_service::users_surface(),
        tilo_system_service::drives_surface(),
        tilo_system_service::scanners_surface(),
        tilo_system_service::storage_surface(),
        tilo_system_service::disk_unlock_surface(),
        tilo_system_service::host_info_surface(),
        tilo_system_service::codec_pack_surface(),
        tilo_system_service::windows_programs_surface(),
        tilo_system_service::windows_apps::surface(),
        tilo_system_service::firmware_surface(),
        tilo_system_service::containers_surface(),
        tilo_system_service::processes_surface(),
        tilo_system_service::disks_surface(),
        tilo_system_service::firewall_surface(),
        tilo_system_service::journal_surface(),
        tilo_system_service::update_surface(),
        tilo_system_service::time_zone_surface(),
        displays::surface(),
        displays::screen_surface(),
        inputs::surface(),
        keyboard::surface(),
        privacy::surface(),
        notifications::surface(),
        osd::surface(),
        controls::night_light_surface(),
        notifications::banners_surface(),
        time::surface(),
        // Whether the person is typing, which what would cover their text steps aside for.
        input::surface(),
        clock::surface(),
        controls::apps_surface(),
        lists::windows_surface(),
        lists::tray_surface(),
        lists::agents_surface(),
        lists::folders_surface(),
        lists::search_surface(),
        lists::launchpad_surface(),
        lists::wallpaper_surface(),
        lists::notice_surface(),
        live::surface(),
        overlays::overlays_surface(),
        overlays::power_menu_surface(),
        tilo_workspace_service::apps::nearby_send::surface(),
        devices::surface(),
        clipboard::surface(),
        // The person's speakers, which the speaker service publishes outside the session.
        tilo_speaker_service::published::surface(),
        quick_settings::controls_surface(),
        // This device's server settings, which its daemon changes (`device:server`).
        tilo_runtime::device::server_settings::surface(),
        // Who is here, the windows this device shares and the invitations into a workspace.
        people::surface(),
        shares::shares_surface(),
        lists::invite_surface(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct File {
        resources: Vec<Surface>,
    }

    /// The device's surface the shell compiles against is the one the device publishes.
    #[test]
    fn the_device_surface_file_is_what_the_device_publishes() {
        let published = device_surfaces();
        let committed: File = serde_json::from_str(include_str!("../device-surface.json"))
            .expect("device-surface.json does not read");
        if committed.resources == published {
            return;
        }
        let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": published }))
            .expect("a surface writes as JSON");
        // `UPDATE_SNAPSHOTS=1` writes the file the device publishes.
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("device-surface.json");
            std::fs::write(&path, &text).expect("device-surface.json is written");
            return;
        }
        panic!(
            "crates/desktop/device-surfaces/device-surface.json is not what the device \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1 cargo nextest run -p \
             tilo-device-surfaces, or write what it publishes:\n{text}"
        );
    }
}
