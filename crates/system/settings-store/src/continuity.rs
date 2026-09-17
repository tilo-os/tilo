//! Canonical continuity policy for leaves in the shared settings document.
//!
//! This registry is deliberately exhaustive for settings owned by Tilo. A new
//! user-facing preference must be classified here as personal, device-local,
//! secret, or derived. Consumers must never infer portability from a section
//! name: display connectors, audio nodes, credentials, and generated state can
//! sit beside genuinely personal preferences.

/// How a settings leaf participates in personal-device continuity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuityClass {
    /// A preference that describes the person and may converge across their
    /// authenticated devices.
    Personal,
    /// Intent tied to this machine's hardware or operating environment.
    DeviceLocal,
    /// Credential material. Secrets use a purpose-built encrypted transfer,
    /// never the ordinary settings channel.
    Secret,
    /// Observed/generated state rather than durable user intent.
    Derived,
}

/// One reviewed leaf in the shared settings schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuityPolicy {
    pub key: &'static str,
    pub class: ContinuityClass,
}

macro_rules! policies {
    ($( $class:ident => [ $( $key:literal ),* $(,)? ] ),* $(,)?) => {
        /// Every reviewed settings-store leaf.
        pub const POLICIES: &[ContinuityPolicy] = &[
            $($(ContinuityPolicy { key: $key, class: ContinuityClass::$class },)*)*
        ];
    };
}

policies! {
    Personal => [
        "tilo.accessibility.large_cursor",
        "tilo.accessibility.cursor_size",
        "tilo.accessibility.screen_reader",
        "tilo.accessibility.on_screen_keyboard",
        "tilo.accessibility.sticky_keys",
        "tilo.accessibility.slow_keys",
        "tilo.accessibility.visual_alerts",
        "tilo.accessibility.color_filter",
        "tilo.accessibility.magnifier_zoom",
        "tilo.accessibility.cursor_crosshair",
        "tilo.accessibility.thick_text_caret",
        "tilo.accessibility.switch_control",
        "tilo.accessibility.switch_key_pair",
        "tilo.accessibility.switch_scan_ms",
        "tilo.accessibility.screen_reader_rate",
        "tilo.accessibility.screen_reader_voice",
        "tilo.appearance.theme",
        "tilo.appearance.high_contrast",
        "tilo.appearance.reduce_motion",
        "tilo.appearance.text_scale",
        "tilo.appearance.ui_scale",
        "tilo.appearance.studio",
        "tilo.locale.currency",
        "tilo.locale.date_format",
        "tilo.locale.time_format",
        "tilo.locale.measurement",
        "tilo.locale.number_style",
        "tilo.locale.first_day_of_week",
        "tilo.notifications.do_not_disturb",
        "tilo.notifications.show_banners",
        "tilo.notifications.show_previews",
        "tilo.notifications.play_sounds",
        "tilo.notifications.show_on_lock_screen",
        "tilo.notifications.show_badges",
        "tilo.notifications.focus",
        "tilo.notifications.per_app",
        "tilo.input.natural_scroll",
        "tilo.input.touchpad_natural_scroll",
        "tilo.input.mouse_natural_scroll",
        "tilo.input.tap_to_click",
        "tilo.input.pointer_speed",
        "tilo.input.touchpad_pointer_speed",
        "tilo.input.mouse_pointer_speed",
        "tilo.input.key_repeat",
        "tilo.input.key_repeat_delay_ms",
        "tilo.input.key_repeat_rate_hz",
        "tilo.input.keyboard_layout",
        "tilo.input.tap_drag",
        "tilo.input.disable_while_typing",
        "tilo.input.middle_emulation",
        "tilo.input.left_handed",
        "tilo.input.accel_profile",
        "tilo.input.touchpad_gestures",
        "tilo.input.compose_key",
        "tilo.input.keyboard_layouts",
        "tilo.audio.event_sounds",
        "tilo.audio.chat_join",
        "tilo.audio.chat_leave",
        "tilo.audio.balance",
        "tilo.audio.mono_output",
        "tilo.display.night_light",
        "tilo.display.night_light_temperature",
        "tilo.shortcuts.bindings",
        "tilo.desktop_controls.vim_mode",
        "tilo.quick_settings.controls",
        "tilo.text_assistance.check_spelling",
        "tilo.text_assistance.auto_correct",
    ],
    DeviceLocal => [
        "tilo.audio.output_device",
        "tilo.audio.input_device",
        "tilo.display.auto_brightness",
        "tilo.display.scale",
        "tilo.input.pen_output",
        "tilo.power.auto_suspend",
        "tilo.power.suspend_minutes",
        "tilo.power.saver_suspend_minutes",
        "tilo.power.balanced_suspend_minutes",
        "tilo.power.performance_suspend_minutes",
        "tilo.power.blank_minutes",
        "tilo.power.lid_close_action",
        "tilo.power.power_key_action",
        "tilo.power.low_battery_percent",
        "tilo.power.critical_battery_percent",
        "tilo.power.critical_action",
        "tilo.updates.auto",
        "tilo.updates.channel",
        "tilo.appearance.wallpaper_path",
        "tilo.device.name",
        "tilo.display.hardware.edid",
        "tilo.network.interface.mac",
        "tilo.storage.mount.usb",
    ],
    Secret => [
        "tilo.agent_providers.api_key",
        "tilo.security.local_credential.pin",
    ],
    Derived => [
        "tilo.version",
        "tilo.updates.last_checked",
    ],
}

/// Return the reviewed continuity class for an exact settings leaf.
#[must_use]
pub fn classify(key: &str) -> Option<ContinuityClass> {
    POLICIES
        .iter()
        .find(|policy| policy.key == key)
        .map(|policy| policy.class)
}

/// Whether an exact settings leaf may use the ordinary roaming channel.
#[must_use]
pub fn is_personal(key: &str) -> bool {
    classify(key) == Some(ContinuityClass::Personal)
}

/// Enumerate unreviewed top-level section leaves from a settings document.
///
/// Objects below a leaf remain one value (for example notification per-app
/// rules and shortcut bindings), matching the device-fabric wire format.
#[must_use]
pub fn unclassified_leaves(document: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut missing = Vec::new();
    for (section, value) in document {
        if section == "version" {
            let key = "tilo.version";
            if classify(key).is_none() {
                missing.push(key.to_string());
            }
            continue;
        }
        let serde_json::Value::Object(leaves) = value else {
            missing.push(format!("tilo.{section}"));
            continue;
        };
        for leaf in leaves.keys() {
            let key = format!("tilo.{section}.{leaf}");
            if classify(&key).is_none() {
                missing.push(key);
            }
        }
    }
    missing.sort();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn policy_keys_are_unique_and_canonical() {
        let mut keys = BTreeSet::new();
        for policy in POLICIES {
            assert!(policy.key.starts_with("tilo."), "{}", policy.key);
            assert!(
                keys.insert(policy.key),
                "duplicate policy for {}",
                policy.key
            );
        }
    }

    #[test]
    fn credentials_and_hardware_are_never_personal() {
        for key in [
            "tilo.agent_providers.api_key",
            "tilo.security.local_credential.pin",
            "tilo.audio.output_device",
            "tilo.display.scale",
            "tilo.input.pen_output",
        ] {
            assert!(classify(key).is_some(), "{key} must be reviewed");
            assert!(!is_personal(key), "{key} must not roam");
        }
    }

    #[test]
    fn unknown_settings_fail_closed() {
        assert_eq!(classify("tilo.input.future_preference"), None);
        assert!(!is_personal("tilo.input.future_preference"));
    }
}
