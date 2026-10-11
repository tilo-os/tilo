//! Canonical continuity policy for leaves in the shared settings document.
//!
//! This registry is deliberately exhaustive for settings owned by Tilo. A new
//! user-facing preference must be classified here as personal, device-local,
//! secret, or derived. Consumers must never infer portability from a section
//! name: display connectors, audio nodes, credentials, and generated state can
//! sit beside genuinely personal preferences.

/// Where durable intent is allowed to live.
///
/// This is intentionally broader than “does it roam?”.  A caller reviewing a
/// new key has to choose the owning boundary, rather than getting portability
/// accidentally from a section name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingScope {
    /// A preference that describes the person and may converge across their
    /// authenticated devices.
    Personal,
    /// Shared workspace policy. It belongs in the workspace record plane and
    /// must never be copied through a person's settings fabric.
    Workspace,
    /// Intent tied to this machine's hardware or operating environment.
    DeviceLocal,
    /// Credential material. Secrets use a purpose-built encrypted transfer,
    /// never the ordinary settings channel.
    Secret,
    /// Observed/generated state rather than durable user intent.
    Derived,
}

/// Backwards-compatible name for consumers that only ask the continuity
/// question. New code should use [`SettingScope`].
pub type ContinuityClass = SettingScope;

/// One reviewed leaf in the shared settings schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuityPolicy {
    pub key: &'static str,
    pub class: SettingScope,
}

macro_rules! policies {
    ($( $class:ident => [ $( $key:literal ),* $(,)? ] ),* $(,)?) => {
        /// Every reviewed settings-store leaf.
        pub const POLICIES: &[ContinuityPolicy] = &[
            $($(ContinuityPolicy { key: $key, class: SettingScope::$class },)*)*
        ];
    };
}

policies! {
    Personal => [
        "tilo.accessibility.large_cursor",
        "tilo.accessibility.cursor_size",
        "tilo.accessibility.screen_reader",
        "tilo.accessibility.on_screen_keyboard",
        // Recognition language describes the person, not a microphone or grant.
        "tilo.accessibility.dictation_language",
        "tilo.accessibility.dictation_literal",
        "tilo.accessibility.sticky_keys",
        "tilo.accessibility.slow_keys",
        "tilo.accessibility.bounce_keys",
        "tilo.accessibility.bounce_keys_delay_ms",
        "tilo.accessibility.bounce_keys_alert",
        "tilo.accessibility.mouse_keys",
        "tilo.accessibility.visual_alerts",
        "tilo.accessibility.color_filter",
        "tilo.accessibility.magnifier_zoom",
        "tilo.accessibility.magnifier_last_zoom",
        "tilo.accessibility.magnifier_view",
        "tilo.accessibility.cursor_crosshair",
        "tilo.accessibility.thick_text_caret",
        "tilo.accessibility.switch_control",
        "tilo.accessibility.switch_key_pair",
        "tilo.accessibility.switch_scan_ms",
        "tilo.accessibility.screen_reader_rate",
        "tilo.accessibility.screen_reader_voice",
        "tilo.appearance.theme",
        "tilo.appearance.mode",
        "tilo.appearance.dark_theme",
        "tilo.appearance.light_theme",
        "tilo.appearance.custom_themes",
        "tilo.appearance.density",
        "tilo.appearance.schedule",
        "tilo.appearance.sunrise",
        "tilo.appearance.sunset",
        "tilo.appearance.wallpaper_scope",
        "tilo.appearance.wallpaper_target",
        "tilo.appearance.high_contrast",
        "tilo.appearance.reduce_motion",
        "tilo.appearance.text_scale",
        "tilo.appearance.ui_scale",
        "tilo.appearance.reduce_transparency",
        "tilo.appearance.forced_colors",
        "tilo.appearance.workspace_temperatures",
        "tilo.appearance.studio",
        "tilo.identity.colour",
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
        "tilo.input.tap_drag_lock",
        "tilo.input.disable_while_typing",
        "tilo.input.middle_emulation",
        "tilo.input.left_handed",
        "tilo.input.accel_profile",
        "tilo.input.touchpad_gestures",
        "tilo.input.touchpad_click_method",
        "tilo.input.touchpad_scroll_method",
        "tilo.input.scroll_speed",
        "tilo.input.compose_key",
        "tilo.input.caps_lock",
        "tilo.input.keyboard_layouts",
        "tilo.audio.system_sounds",
        "tilo.audio.balance",
        "tilo.audio.mono_output",
        "tilo.audio.echo_cancellation",
        "tilo.display.night_light",
        "tilo.display.night_light_schedule",
        "tilo.display.night_light_temperature",
        // Where each of the person's displays sits across their devices.
        // Each device's own modes and screen layout stay device-local.
        "tilo.display.arrangement",
        "tilo.shortcuts.bindings",
        "tilo.desktop_controls.vim_mode",
        "tilo.quick_settings.controls",
        "tilo.default_apps.handlers",
        "tilo.text_assistance.check_spelling",
        "tilo.text_assistance.auto_correct",
        "tilo.text_assistance.spelling_language",
        // Hosted providers name no device: an endpoint on the internet and a
        // reference to the key, never the key itself.
        "tilo.agent_providers.hosted",
    ],
    DeviceLocal => [
        "tilo.audio.output_device",
        "tilo.audio.input_device",
        "tilo.display.auto_brightness",
        "tilo.display.scale",
        "tilo.input.tablets",
        "tilo.input.touchpad_enabled",
        "tilo.input.host_input_migration_version",
        // Physical model and explicit inspection choices stay with this device.
        // Assignments identify physical devices on this host, unlike personal source order.
        "tilo.input.keyboard_assignments",
        "tilo.input.keyboard_model",
        "tilo.input.keyboard_preview",
        "tilo.input.keyboard_preview_extra_levels",
        "tilo.input.keyboard_preview_level",
        "tilo.input.keyboard_inspected_key",
        // Like whether the touchpad is on, whether a mouse switches it off
        // says nothing about another device's touchpad.
        "tilo.input.touchpad_disabled_on_external_mouse",
        "tilo.power.auto_suspend",
        "tilo.power.suspend_minutes",
        "tilo.power.saver_suspend_minutes",
        "tilo.power.balanced_suspend_minutes",
        "tilo.power.performance_suspend_minutes",
        "tilo.power.blank_minutes",
        "tilo.power.lock_minutes",
        "tilo.power.lid_close_action",
        "tilo.power.power_key_action",
        "tilo.power.low_battery_percent",
        "tilo.power.critical_battery_percent",
        "tilo.power.critical_action",
        "tilo.updates.auto",
        "tilo.appearance.wallpaper_path",
        "tilo.appearance.distribution_defaults",
        "tilo.device.name",
        "tilo.display.hardware.edid",
        "tilo.network.interface.mac",
        "tilo.storage.mount.usb",
        "tilo.continuity.enabled",
        "tilo.continuity.clipboard",
        "tilo.continuity.notifications",
        "tilo.continuity.wifi_settings",
        "tilo.continuity.media",
        "tilo.continuity.display_input",
        "tilo.continuity.automatic_display_input",
        "tilo.continuity.file_transfer",
        "tilo.continuity.realtime_sessions",
        "tilo.continuity.typed_gamepad",
        "tilo.continuity.app_continuations",
        // Connector identities, modes, calibration and staged apply state
        // are meaningful only on the machine that owns the hardware.
        "tilo.display_profiles.profiles",
        "tilo.display_transaction.id",
        "tilo.display_transaction.state",
        "tilo.display_transaction.set",
        "tilo.display_transaction.previous",
        "tilo.display_transaction.candidate",
        "tilo.display_transaction.deadline_unix_ms",
        "tilo.display_transaction.message",
        // Providers on this device or its network, and which one this device
        // uses by default.
        "tilo.agent_providers.config",
        "tilo.collaboration.status_sharing",
        "tilo.collaboration.manual_status",
    ],
    Secret => [
        "tilo.agent_providers.api_key",
        "tilo.security.local_credential",
        "tilo.security.local_credential.pin",
    ],
    Derived => [
        "tilo.version",
        "tilo.updates.last_checked",
        "tilo.continuity.audit",
        "tilo.history.seen_frontiers",
    ],
}

/// The section holding every app's [`SettingScope::Personal`] preferences,
/// one object per app id. An app's typed settings declare their scope, and
/// only those declared personal are written here, so each entry is a
/// reviewed leaf: `tilo.app_personal.<app id>` roams as one value.
pub const APP_PERSONAL_SECTION: &str = "app_personal";

/// The section holding every app's [`SettingScope::DeviceLocal`]
/// preferences, one object per app id. They never leave the device.
pub const APP_DEVICE_LOCAL_SECTION: &str = "app_device_local";

/// The largest serialized personal value the device fabric carries. An app's
/// personal section is one value, so its whole object must fit.
pub const MAX_PERSONAL_VALUE_BYTES: usize = 64 * 1024;

/// The scope of an app's settings section leaf (`tilo.<section>.<app id>`),
/// when `key` names one.
fn app_section_scope(key: &str) -> Option<SettingScope> {
    let rest = key.strip_prefix("tilo.")?;
    let (section, app) = rest.split_once('.')?;
    if !valid_app_id(app) {
        return None;
    }
    match section {
        APP_PERSONAL_SECTION => Some(SettingScope::Personal),
        APP_DEVICE_LOCAL_SECTION => Some(SettingScope::DeviceLocal),
        _ => None,
    }
}

/// A reverse-DNS app id: dot-separated, non-empty labels of ASCII letters,
/// digits, `-` and `_`.
fn valid_app_id(app: &str) -> bool {
    app.len() <= 200
        && app.contains('.')
        && app.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        })
}

/// A reviewed, typed settings leaf. Construction succeeds only for registry
/// entries, so an unknown future key cannot silently acquire roaming policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingKey(&'static ContinuityPolicy);

impl SettingKey {
    #[must_use]
    pub const fn canonical(self) -> &'static str {
        self.0.key
    }

    #[must_use]
    pub const fn scope(self) -> SettingScope {
        self.0.class
    }
}

/// Look up a canonical key in the reviewed registry.
#[must_use]
pub fn registered(key: &str) -> Option<SettingKey> {
    POLICIES
        .iter()
        .find(|policy| policy.key == key)
        .map(SettingKey)
}

/// Return the reviewed continuity class for an exact settings leaf: a
/// registry entry, or an app's section in [`APP_PERSONAL_SECTION`] or
/// [`APP_DEVICE_LOCAL_SECTION`], whose scope the app declared by writing it
/// there.
#[must_use]
pub fn classify(key: &str) -> Option<SettingScope> {
    registered(key)
        .map(SettingKey::scope)
        .or_else(|| app_section_scope(key))
}

/// Whether an exact settings leaf may use the ordinary roaming channel.
#[must_use]
pub fn is_personal(key: &str) -> bool {
    classify(key) == Some(SettingScope::Personal)
}

/// Project only reviewed personal leaves from the shared settings document.
///
/// This is the sole projection used by personal-device continuity. Unknown,
/// workspace-owned, local, secret, and derived leaves fail closed.
#[must_use]
pub fn personal_values(
    document: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut projected = std::collections::BTreeMap::new();
    for (section, value) in document {
        let serde_json::Value::Object(leaves) = value else {
            continue;
        };
        for (leaf, value) in leaves {
            let canonical = format!("tilo.{section}.{leaf}");
            if classify(&canonical) == Some(SettingScope::Personal) {
                projected.insert(canonical, value.clone());
            }
        }
    }
    projected
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
    fn continuity_policy_keys_are_canonical_and_private_values_stay_local() {
        // Phase `policy_keys_are_unique_and_canonical`.
        {
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
        // Phase `credentials_and_hardware_are_never_personal`.
        {
            for key in [
                "tilo.agent_providers.api_key",
                "tilo.security.local_credential.pin",
                "tilo.audio.output_device",
                "tilo.display.scale",
                "tilo.input.tablets",
                "tilo.continuity.clipboard",
                "tilo.continuity.typed_gamepad",
                "tilo.continuity.app_continuations",
                "tilo.continuity.audit",
            ] {
                assert!(classify(key).is_some(), "{key} must be reviewed");
                assert!(!is_personal(key), "{key} must not roam");
            }
        }
        // Phase `continuity_privacy_choices_are_device_local_and_audit_is_derived`.
        {
            for key in [
                "tilo.continuity.enabled",
                "tilo.continuity.clipboard",
                "tilo.continuity.notifications",
                "tilo.continuity.wifi_settings",
                "tilo.continuity.media",
                "tilo.continuity.display_input",
                "tilo.continuity.automatic_display_input",
                "tilo.continuity.file_transfer",
                "tilo.continuity.realtime_sessions",
                "tilo.continuity.typed_gamepad",
                "tilo.continuity.app_continuations",
            ] {
                assert_eq!(classify(key), Some(ContinuityClass::DeviceLocal), "{key}");
            }
            assert_eq!(
                classify("tilo.continuity.audit"),
                Some(ContinuityClass::Derived)
            );
        }
        // Phase `unknown_settings_fail_closed`.
        {
            assert_eq!(classify("tilo.input.future_preference"), None);
            assert!(!is_personal("tilo.input.future_preference"));
        }
    }

    #[test]
    fn app_sections_roam_only_when_declared_personal() {
        let document = serde_json::json!({
            "app_personal": {
                "so.tilo.monitor": {"confirm_kill": false, "interval_ms": 2000},
                "not-an-app-id": {"x": 1}
            },
            "app_device_local": {"so.tilo.monitor": {"last_window": [0, 0]}},
            "so.tilo.monitor": {"stray": true}
        });
        let document = document.as_object().unwrap();
        assert_eq!(
            personal_values(document),
            std::collections::BTreeMap::from([(
                "tilo.app_personal.so.tilo.monitor".to_string(),
                serde_json::json!({"confirm_kill": false, "interval_ms": 2000})
            )])
        );
        assert_eq!(
            classify("tilo.app_device_local.so.tilo.monitor"),
            Some(SettingScope::DeviceLocal)
        );
        assert_eq!(classify("tilo.app_personal.not-an-app-id"), None);
        assert_eq!(classify("tilo.app_personal."), None);
        assert_eq!(classify("tilo.so.tilo.monitor.stray"), None);
        assert_eq!(
            unclassified_leaves(document),
            vec![
                "tilo.app_personal.not-an-app-id".to_string(),
                "tilo.so.tilo.monitor.stray".to_string()
            ]
        );
    }

    #[test]
    fn the_typed_registry_reviews_every_writer_and_ownership_boundary() {
        // Phase `typed_registry_projects_only_reviewed_personal_values`.
        {
            let document = serde_json::json!({
                "appearance": {
                    "mode": "light",
                    "wallpaper_path": "/home/me/private.png",
                    "future_value": "must stay local"
                },
                "display_profiles": {"profiles": {"edid:Dell|DELL U2720Q|7XK2P13": {"config": {}}}},
                "agent_providers": {"config": {"profiles": [{"endpoint": "http://127.0.0.1:8000"}]}},
                "security": {"local_credential": {"pin": "never"}}
            });
            let projected = personal_values(document.as_object().unwrap());
            assert_eq!(
                projected,
                std::collections::BTreeMap::from([(
                    "tilo.appearance.mode".to_string(),
                    serde_json::json!("light")
                )])
            );
        }
        // Phase `every_cross_crate_settings_writer_has_reviewed_leaves`.
        {
            // These are the non-core writers of settings.json. Core Settings
            // structs have their own schema-derived coverage test in
            // apps/settings/src/prefs.rs. Keeping the other writer shapes here
            // makes a new cross-crate leaf fail until its ownership is reviewed.
            let document = serde_json::json!({
                "display_profiles": {"profiles": {}},
                "display_transaction": {
                    "id": 1,
                    "state": "requested",
                    "set": {},
                    "previous": {},
                    "candidate": {},
                    "deadline_unix_ms": null,
                    "message": null
                },
                "shortcuts": {"bindings": {}},
                "input": {"touchpad_enabled": false},
                "desktop_controls": {"vim_mode": false},
                "quick_settings": {"controls": ["wifi"]},
                "text_assistance": {"check_spelling": true, "auto_correct": true},
                "continuity": {
                    "enabled": false,
                    "clipboard": false,
                    "notifications": false,
                    "wifi_settings": false,
                    "media": false,
                    "display_input": false,
                    "file_transfer": false,
                    "realtime_sessions": false,
                    "typed_gamepad": false,
                    "app_continuations": false,
                    "audit": []
                },
                "agent_providers": {"config": {}, "hosted": {}},
                "collaboration": {"status_sharing": "automatic", "manual_status": null},
                "history": {"seen_frontiers": {}}
            });
            assert_eq!(
                unclassified_leaves(document.as_object().unwrap()),
                Vec::<String>::new()
            );
        }
        // Phase `registry_names_all_ownership_boundaries`.
        {
            assert!(
                POLICIES
                    .iter()
                    .any(|entry| entry.class == SettingScope::Personal)
            );
            assert!(
                POLICIES
                    .iter()
                    .any(|entry| entry.class == SettingScope::DeviceLocal)
            );
            assert!(
                POLICIES
                    .iter()
                    .any(|entry| entry.class == SettingScope::Secret)
            );
            assert!(
                POLICIES
                    .iter()
                    .any(|entry| entry.class == SettingScope::Derived)
            );
            // Workspace policy intentionally has no settings.json leaf: it is
            // stored in the workspace record plane. Keeping the variant in the
            // type system prevents callers from treating “not personal” as
            // synonymous with “device-local”.
            assert_ne!(SettingScope::Workspace, SettingScope::DeviceLocal);
        }
    }
}
