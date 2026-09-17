//! Canonical continuity policy for leaves in the shared settings document.
//!
//! This registry is deliberately exhaustive for settings owned by qOS. A new
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
        "qos.accessibility.large_cursor",
        "qos.accessibility.cursor_size",
        "qos.accessibility.screen_reader",
        "qos.accessibility.on_screen_keyboard",
        "qos.accessibility.sticky_keys",
        "qos.accessibility.slow_keys",
        "qos.accessibility.visual_alerts",
        "qos.accessibility.color_filter",
        "qos.accessibility.magnifier_zoom",
        "qos.accessibility.cursor_crosshair",
        "qos.accessibility.thick_text_caret",
        "qos.accessibility.switch_control",
        "qos.accessibility.switch_key_pair",
        "qos.accessibility.switch_scan_ms",
        "qos.accessibility.screen_reader_rate",
        "qos.accessibility.screen_reader_voice",
        "qos.appearance.theme",
        "qos.appearance.high_contrast",
        "qos.appearance.reduce_motion",
        "qos.appearance.text_scale",
        "qos.appearance.ui_scale",
        "qos.appearance.studio",
        "qos.locale.currency",
        "qos.locale.date_format",
        "qos.locale.time_format",
        "qos.locale.measurement",
        "qos.locale.number_style",
        "qos.locale.first_day_of_week",
        "qos.notifications.do_not_disturb",
        "qos.notifications.show_banners",
        "qos.notifications.show_previews",
        "qos.notifications.play_sounds",
        "qos.notifications.show_on_lock_screen",
        "qos.notifications.show_badges",
        "qos.notifications.focus",
        "qos.notifications.per_app",
        "qos.input.natural_scroll",
        "qos.input.touchpad_natural_scroll",
        "qos.input.mouse_natural_scroll",
        "qos.input.tap_to_click",
        "qos.input.pointer_speed",
        "qos.input.touchpad_pointer_speed",
        "qos.input.mouse_pointer_speed",
        "qos.input.key_repeat",
        "qos.input.key_repeat_delay_ms",
        "qos.input.key_repeat_rate_hz",
        "qos.input.keyboard_layout",
        "qos.input.tap_drag",
        "qos.input.disable_while_typing",
        "qos.input.middle_emulation",
        "qos.input.left_handed",
        "qos.input.accel_profile",
        "qos.input.touchpad_gestures",
        "qos.input.compose_key",
        "qos.input.keyboard_layouts",
        "qos.audio.event_sounds",
        "qos.audio.chat_join",
        "qos.audio.chat_leave",
        "qos.audio.balance",
        "qos.audio.mono_output",
        "qos.display.night_light",
        "qos.display.night_light_temperature",
        "qos.shortcuts.bindings",
        "qos.desktop_controls.vim_mode",
        "qos.quick_settings.controls",
        "qos.text_assistance.check_spelling",
        "qos.text_assistance.auto_correct",
    ],
    DeviceLocal => [
        "qos.audio.output_device",
        "qos.audio.input_device",
        "qos.display.auto_brightness",
        "qos.display.scale",
        "qos.input.pen_output",
        "qos.power.auto_suspend",
        "qos.power.suspend_minutes",
        "qos.power.saver_suspend_minutes",
        "qos.power.balanced_suspend_minutes",
        "qos.power.performance_suspend_minutes",
        "qos.power.blank_minutes",
        "qos.power.lid_close_action",
        "qos.power.power_key_action",
        "qos.power.low_battery_percent",
        "qos.power.critical_battery_percent",
        "qos.power.critical_action",
        "qos.updates.auto",
        "qos.updates.channel",
        "qos.appearance.wallpaper_path",
        "qos.device.name",
        "qos.display.hardware.edid",
        "qos.network.interface.mac",
        "qos.storage.mount.usb",
        "qos.continuity.enabled",
        "qos.continuity.clipboard",
        "qos.continuity.notifications",
        "qos.continuity.wifi_settings",
        "qos.continuity.media",
        "qos.continuity.display_input",
        "qos.continuity.file_transfer",
        "qos.continuity.realtime_sessions",
        "qos.continuity.typed_gamepad",
        "qos.continuity.app_continuations",
    ],
    Secret => [
        "qos.agent_providers.api_key",
        "qos.security.local_credential.pin",
    ],
    Derived => [
        "qos.version",
        "qos.updates.last_checked",
        "qos.continuity.audit",
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
            let key = "qos.version";
            if classify(key).is_none() {
                missing.push(key.to_string());
            }
            continue;
        }
        let serde_json::Value::Object(leaves) = value else {
            missing.push(format!("qos.{section}"));
            continue;
        };
        for leaf in leaves.keys() {
            let key = format!("qos.{section}.{leaf}");
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
            assert!(policy.key.starts_with("qos."), "{}", policy.key);
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
            "qos.agent_providers.api_key",
            "qos.security.local_credential.pin",
            "qos.audio.output_device",
            "qos.display.scale",
            "qos.input.pen_output",
            "qos.continuity.clipboard",
            "qos.continuity.typed_gamepad",
            "qos.continuity.app_continuations",
            "qos.continuity.audit",
        ] {
            assert!(classify(key).is_some(), "{key} must be reviewed");
            assert!(!is_personal(key), "{key} must not roam");
        }
    }

    #[test]
    fn continuity_privacy_choices_are_device_local_and_audit_is_derived() {
        for key in [
            "qos.continuity.enabled",
            "qos.continuity.clipboard",
            "qos.continuity.notifications",
            "qos.continuity.wifi_settings",
            "qos.continuity.media",
            "qos.continuity.display_input",
            "qos.continuity.file_transfer",
            "qos.continuity.realtime_sessions",
            "qos.continuity.typed_gamepad",
            "qos.continuity.app_continuations",
        ] {
            assert_eq!(classify(key), Some(ContinuityClass::DeviceLocal), "{key}");
        }
        assert_eq!(
            classify("qos.continuity.audit"),
            Some(ContinuityClass::Derived)
        );
    }

    #[test]
    fn unknown_settings_fail_closed() {
        assert_eq!(classify("qos.input.future_preference"), None);
        assert!(!is_personal("qos.input.future_preference"));
    }
}
