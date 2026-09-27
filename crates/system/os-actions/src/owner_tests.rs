//! Every host-touching entry point, driven on a shell that owns the machine
//! and on one that does not, with every executor replaced by a recorder so no
//! test here reaches this machine.

use crate::owner::SystemSurfaces;
use crate::owner::testing::recording;
use crate::{
    HOST_MANAGED_REASON, audio, battery, bluetooth, bluez, brightness, codec_pack, display,
    firmware, keyboard_backlight, modem, network, power, printing, privacy, session, time, users,
};

type Call = (&'static str, fn() -> Result<(), String>);

const UUID: &str = "0f5a1c2e-3b4d-4e5f-8a9b-0c1d2e3f4a5b";

fn discard<T>(result: Result<T, String>) -> Result<(), String> {
    result.map(|_| ())
}

/// Every system action, and the reads that feed Quick Settings. Inputs are
/// valid so each call gets as far as its executor.
fn calls() -> Vec<Call> {
    vec![
        ("session lock", || {
            session::perform_session_action(session::SessionAction::Lock)
        }),
        ("session logout", || {
            session::perform_session_action(session::SessionAction::Logout)
        }),
        ("session suspend", || {
            session::perform_session_action(session::SessionAction::Suspend)
        }),
        ("session restart", || {
            session::perform_session_action(session::SessionAction::Restart)
        }),
        ("session shut down", || {
            session::perform_session_action(session::SessionAction::ShutDown)
        }),
        ("backlight", || display::set_brightness(40.0)),
        ("backlight target", || {
            brightness::BrightnessTarget::Backlight("/sys/class/backlight/fixture".into()).set(40.0)
        }),
        ("ddc target", || {
            brightness::BrightnessTarget::Ddc("/dev/i2c-fixture".into()).set(40.0)
        }),
        ("keyboard backlight", || {
            keyboard_backlight::set_keyboard_backlight(1)
        }),
        ("volume", || audio::set_volume(40.0)),
        ("mute", || audio::set_mute(true)),
        ("toggle mute", audio::toggle_mute),
        ("volume read", || discard(audio::get_volume())),
        ("wifi", || network::set_wifi(false)),
        ("wifi read", || discard(network::get_wifi())),
        ("wifi join", || {
            network::connect_wifi("Fixture", Some("fixture-pass"))
        }),
        ("wifi leave", || network::disconnect_wifi("Fixture")),
        ("wired", || network::set_wired(false)),
        ("airplane", || network::set_airplane_mode(true)),
        ("hotspot", || network::set_hotspot(true)),
        ("mobile broadband", || network::set_mobile_broadband(true)),
        ("vpn", || network::set_vpn_connection("Fixture", true)),
        ("vpn by uuid", || {
            network::set_vpn_connection_uuid(UUID, true)
        }),
        ("vpn delete", || network::delete_vpn_connection("Fixture")),
        ("vpn list", || discard(network::list_vpn_connections())),
        ("forget network", || network::forget_network_profile(UUID)),
        ("autoconnect", || {
            network::set_network_autoconnect(UUID, false)
        }),
        ("connectivity", || discard(network::get_connectivity())),
        ("captive portal", || discard(network::captive_portal_uri())),
        ("bluetooth power", || bluez::set_powered(true)),
        ("bluetooth scan", bluez::start_scan),
        ("bluetooth radio", || bluetooth::set_bluetooth(false)),
        ("bluetooth connect", || {
            bluetooth::bt_connect("00:11:22:33:44:55")
        }),
        ("bluetooth read", || discard(bluetooth::get_bluetooth())),
        ("power profile", || {
            power::set_power_profile(power::PowerProfile::Performance)
        }),
        ("charge limit", || battery::set_charge_limit(80)),
        ("time zone", || time::set_timezone("Europe/Berlin")),
        ("ntp", || time::set_ntp(true)),
        ("account role", || {
            users::set_role("fixture", users::AccountRole::Standard)
        }),
        ("sim unlock", || modem::unlock_mobile_sim("1234")),
        ("print queue", || printing::set_default("Office")),
        ("print queue removal", || printing::remove_queue("Office")),
        ("firmware refresh", firmware::refresh_firmware_metadata),
        ("codec pack", || {
            codec_pack::run(codec_pack::CodecAction::Install)
        }),
        ("media revoke", || privacy::revoke_media_client(42)),
        ("media monitor", || privacy::monitor_media_usage(|_| {})),
        ("uinput", || {
            use crate::linux_virtual_gamepad::LinuxGamepadDeviceProvider as _;
            crate::UinputGamepadDeviceProvider
                .open_fixed_gamepad()
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
    ]
}

#[test]
fn a_shell_that_does_not_own_the_machine_reaches_no_executor() {
    for (name, call) in calls() {
        let mut result = None;
        let reached = recording(SystemSurfaces::Hosted, || result = Some(call()));
        assert_eq!(reached, Vec::<String>::new(), "{name} reached the host");
        assert_eq!(
            result,
            Some(Err(HOST_MANAGED_REASON.to_owned())),
            "{name} must refuse as host-managed"
        );
    }
    let reached = recording(SystemSurfaces::Hosted, || {
        assert!(!session::user_switching_supported());
    });
    assert_eq!(reached, Vec::<String>::new());
}

#[test]
fn the_shell_that_owns_the_machine_reaches_each_executor() {
    for (name, call) in calls() {
        let mut result = None;
        let reached = recording(SystemSurfaces::Owned, || result = Some(call()));
        assert!(!reached.is_empty(), "{name} never reached its executor");
        let refused = result.and_then(Result::err).unwrap_or_default();
        assert!(
            refused.starts_with("recorded "),
            "{name} ended at the recorder, not the host: {refused}"
        );
    }
}
