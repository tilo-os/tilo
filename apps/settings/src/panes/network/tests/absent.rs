//! Rows left out for a device that runs no network service, or has no Wi-Fi
//! radio: the page names what is missing once instead of showing a dead switch,
//! an unreadable state or a row with nothing behind it, and the actions only
//! those services can carry out are published unavailable.

use super::*;

#[test]
fn without_a_network_service_the_page_leaves_out_what_only_it_fills() {
    ui::i18n::I18nManager::set_locale("en-US");
    let fake = Arc::new(FakeNetwork::default());
    *fake.service_present.lock().unwrap() = Some(false);
    *fake.unreadable.lock().unwrap() = Some("NetworkManager is not running".into());
    let mut network = model(&fake);
    let lines = shown(&mut network);
    for label in [t("net-wifi"), t("net-vpn"), t("net-airplane")] {
        assert!(
            !lines.contains(&label),
            "`{label}` is left out without a network service: {lines:#?}"
        );
    }
    assert!(
        !lines.iter().any(|line| line.contains("Can't read")),
        "nothing is shown as unreadable: {lines:#?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| *line == &t("net-no-service"))
            .count(),
        1,
        "the page names the missing service once: {lines:#?}"
    );
    assert!(
        lines.contains(&t("net-usage")),
        "data usage stays: {lines:#?}"
    );
}

#[test]
fn without_a_wifi_adapter_the_page_leaves_out_wifi_and_names_it_once() {
    ui::i18n::I18nManager::set_locale("en-US");
    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    let mut snapshot = NetworkSnapshot::of_networks(Vec::new());
    snapshot.radios = Some(Ok(os_actions::RadioState::default()));
    snapshot.wifi_enabled = Some(Ok(false));
    network.adopt_snapshot(snapshot).unwrap();
    let lines = shown(&mut network);
    assert!(
        !lines.contains(&t("net-wifi")),
        "no Wi-Fi card without a Wi-Fi radio: {lines:#?}"
    );
    assert!(
        !lines.contains(&t("net-airplane")),
        "airplane mode needs a radio to switch: {lines:#?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| *line == &t("net-no-wifi-adapter"))
            .count(),
        1,
        "the page names the missing adapter once: {lines:#?}"
    );
    assert!(
        lines.contains(&t("net-vpn")),
        "the network service still fills the VPN row: {lines:#?}"
    );
}

#[test]
fn with_a_network_service_and_a_radio_wifi_and_airplane_mode_are_offered() {
    ui::i18n::I18nManager::set_locale("en-US");
    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    let lines = shown(&mut network);
    for label in [t("net-wifi"), t("net-vpn"), t("net-airplane")] {
        assert!(lines.contains(&label), "`{label}`: {lines:#?}");
    }
}

#[test]
fn without_a_network_service_its_actions_are_unavailable() {
    ui::i18n::I18nManager::set_locale("en-US");
    let fake = Arc::new(FakeNetwork::default());
    *fake.service_present.lock().unwrap() = Some(false);
    *fake.unreadable.lock().unwrap() = Some("NetworkManager is not running".into());
    let network = model(&fake);
    let (mut app, _network) = window_showing(network, (1200, 1800));
    let no_service = unavailable("no_service", &t("net-no-service"));
    for id in ["set-wifi", "set-hotspot", "set-airplane-mode"] {
        assert_eq!(published(&mut app, id), no_service, "`{id}`");
    }
}
