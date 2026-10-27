//! The names a network setting can carry: the bands a hotspot offers, how a
//! connection counts its data, which hardware address it shows, how it gets
//! an IPv6 address and whether it uses a proxy, each written both as the
//! value it is stored as and in the person's language.

use os_actions::network_settings::Ipv6Method;
use os_actions::{ConnectivityState, HotspotBand};

use super::Radio;
use crate::i18n::t;

/// The bands a hotspot can use.
pub(crate) const HOTSPOT_BANDS: &[&str] = &["Automatic", "2.4 GHz", "5 GHz"];
/// Whether a connection's data counts against a limit.
pub(crate) const METERED_POLICIES: &[&str] = &["Automatic", "Metered", "Unmetered"];
/// Which hardware address a Wi-Fi connection shows.
pub(crate) const MAC_POLICIES: &[&str] = &[
    "Device address",
    "Random for this network",
    "Random each time",
];
/// How a connection gets its IPv6 address.
pub(crate) const IPV6_METHODS: &[&str] = &["Automatic", "Manual", "Link-local only", "Off"];
/// Whether a connection goes through a proxy, and how it finds it.
pub(crate) const PROXY_MODES: &[&str] = &["None", "Automatic", "Manual"];

pub(crate) fn connectivity_shown(state: ConnectivityState) -> String {
    t(match state {
        ConnectivityState::Unknown => "net-connectivity-unknown",
        ConnectivityState::Offline => "net-connectivity-offline",
        ConnectivityState::Portal => "net-connectivity-portal",
        ConnectivityState::Limited => "net-connectivity-limited",
        ConnectivityState::Full => "net-connectivity-full",
    })
}

pub(crate) fn band_shown(id: &str) -> String {
    t(match id {
        "2.4 GHz" => "net-band-24",
        "5 GHz" => "net-band-5",
        _ => "net-band-auto",
    })
}

pub(crate) fn band_from_label(label: &str) -> HotspotBand {
    if label == "2.4 GHz" || label == t("net-band-24") {
        HotspotBand::TwoPointFourGhz
    } else if label == "5 GHz" || label == t("net-band-5") {
        HotspotBand::FiveGhz
    } else {
        HotspotBand::Automatic
    }
}

pub(crate) fn metered_id(policy: os_actions::MeteredPolicy) -> &'static str {
    match policy {
        os_actions::MeteredPolicy::Automatic => "Automatic",
        os_actions::MeteredPolicy::Metered => "Metered",
        os_actions::MeteredPolicy::Unmetered => "Unmetered",
    }
}

pub(crate) fn metered_shown(id: &str) -> String {
    t(match id {
        "Metered" => "net-metered-yes",
        "Unmetered" => "net-metered-no",
        _ => "net-metered-auto",
    })
}

pub(crate) fn metered_from_label(label: &str) -> Option<os_actions::MeteredPolicy> {
    if label == "Metered" || label == t("net-metered-yes") {
        Some(os_actions::MeteredPolicy::Metered)
    } else if label == "Unmetered" || label == t("net-metered-no") {
        Some(os_actions::MeteredPolicy::Unmetered)
    } else if label == "Automatic" || label == t("net-metered-auto") {
        Some(os_actions::MeteredPolicy::Automatic)
    } else {
        None
    }
}

pub(crate) fn mac_id(policy: os_actions::MacPolicy) -> &'static str {
    match policy {
        os_actions::MacPolicy::Permanent => "Device address",
        os_actions::MacPolicy::Stable => "Random for this network",
        os_actions::MacPolicy::Random => "Random each time",
    }
}

pub(crate) fn mac_shown(id: &str) -> String {
    t(match id {
        "Device address" => "net-mac-permanent",
        "Random each time" => "net-mac-random",
        _ => "net-mac-stable",
    })
}

pub(crate) fn mac_from_label(label: &str) -> Option<os_actions::MacPolicy> {
    if label == "Random each time" || label == t("net-mac-random") {
        Some(os_actions::MacPolicy::Random)
    } else if label == "Device address" || label == t("net-mac-permanent") {
        Some(os_actions::MacPolicy::Permanent)
    } else if label == "Random for this network" || label == t("net-mac-stable") {
        Some(os_actions::MacPolicy::Stable)
    } else {
        None
    }
}

pub(crate) fn ipv6_id(method: Ipv6Method) -> &'static str {
    match method {
        Ipv6Method::Automatic => "Automatic",
        Ipv6Method::Manual => "Manual",
        Ipv6Method::LinkLocal => "Link-local only",
        Ipv6Method::Disabled => "Off",
    }
}

pub(crate) fn ipv6_shown(id: &str) -> String {
    t(match id {
        "Manual" => "net-ipv6-manual",
        "Link-local only" => "net-ipv6-link-local",
        "Off" => "net-ipv6-off",
        _ => "net-ipv6-auto",
    })
}

pub(crate) fn ipv6_from_label(label: &str) -> Option<Ipv6Method> {
    IPV6_METHODS
        .iter()
        .find(|id| **id == label || ipv6_shown(id) == label)
        .map(|id| match *id {
            "Manual" => Ipv6Method::Manual,
            "Link-local only" => Ipv6Method::LinkLocal,
            "Off" => Ipv6Method::Disabled,
            _ => Ipv6Method::Automatic,
        })
}

pub(crate) fn proxy_shown(id: &str) -> String {
    t(match id {
        "Automatic" => "net-proxy-auto",
        "Manual" => "net-proxy-manual",
        _ => "net-proxy-none",
    })
}

pub(crate) fn proxy_from_label(label: &str) -> Option<&'static str> {
    PROXY_MODES
        .iter()
        .copied()
        .find(|id| *id == label || proxy_shown(id) == label)
}

pub(crate) fn radio_shown(radio: Radio) -> String {
    t(match radio {
        Radio::Airplane => "net-radio-airplane",
        Radio::Hotspot => "net-radio-hotspot",
    })
}
