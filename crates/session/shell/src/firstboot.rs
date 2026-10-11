//! First-boot setup, invoked by `shell-firstboot.service` as
//! `/usr/bin/shell --firstboot-only`.
//!
//! The systemd unit guards re-runs with a `ConditionPathExists` on
//! `/etc/tilo/.firstboot-done`, so this module is a one-shot "make sure the
//! user-facing config dir exists" hook. It is cheap and idempotent. It never
//! creates a personal signing identity: that is an acknowledged interactive
//! ceremony owned by the compositor OOBE.

use std::error::Error;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tilo_identity_host::provisioning::IdentityStorage;
pub use tilo_identity_host::provisioning::{
    PRIMARY_IDENTITY_FILE, ProvisionOutcome, collaboration_identity_paths,
    ensure_device_certificate_for_record, login_account_display_name, prepare_identity_directory,
    verify_collaboration_identity,
};
use tilo_user_identity::IdentityPaths;

/// Where this session keeps a newly provisioned identity: the platform store,
/// except in this crate's unit tests, which keep it in private files.
#[cfg(not(test))]
const IDENTITY_STORAGE: IdentityStorage = IdentityStorage::Platform;
#[cfg(test)]
const IDENTITY_STORAGE: IdentityStorage = IdentityStorage::Files;

/// [`tilo_identity_host::provisioning::ensure_primary_identity`] in this
/// session's identity storage.
///
/// # Errors
///
/// Returns an error if file system operations fail (permission denied, I/O error, etc.).
pub fn ensure_primary_identity(identity_dir: &Path) -> Result<ProvisionOutcome, io::Error> {
    tilo_identity_host::provisioning::ensure_primary_identity(identity_dir, IDENTITY_STORAGE)
}

/// [`tilo_identity_host::provisioning::ensure_collaboration_identity`] in this
/// session's identity storage.
///
/// # Errors
///
/// Returns any provisioning, verification, migration or registry error.
pub fn ensure_collaboration_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
) -> Result<ProvisionOutcome, io::Error> {
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        identity_dir,
        paths,
        IDENTITY_STORAGE,
    )
}

/// [`tilo_identity_host::provisioning::ensure_throwaway_test_identity`] in
/// this session's identity storage.
///
/// # Errors
///
/// As the provisioning function.
pub fn ensure_throwaway_test_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
) -> Result<ProvisionOutcome, io::Error> {
    tilo_identity_host::provisioning::ensure_throwaway_test_identity(
        identity_dir,
        paths,
        IDENTITY_STORAGE,
    )
}

/// [`tilo_identity_host::provisioning::ensure_collaboration_identity_for_legacy_linux_host`]
/// in this session's identity storage.
///
/// # Errors
///
/// Returns any provisioning, verification, migration or registry error.
pub fn ensure_collaboration_identity_for_legacy_linux_host(
    identity_dir: &Path,
    paths: &IdentityPaths,
) -> Result<ProvisionOutcome, io::Error> {
    tilo_identity_host::provisioning::ensure_collaboration_identity_for_legacy_linux_host(
        identity_dir,
        paths,
        IDENTITY_STORAGE,
    )
}

/// Version of the host-session input import. It lives in the input section
/// because this is a compatibility migration for that section, not a global
/// settings-document schema version.
const HOST_INPUT_MIGRATION_VERSION_KEY: &str = "host_input_migration_version";
const HOST_INPUT_MIGRATION_VERSION: u64 = 5;

/// Process-global entry point. Resolves the user's XDG config dir from
/// `$XDG_CONFIG_HOME` (falling back to `$HOME/.config`) and creates
/// `tilo-desktop/` under it. Returns `Ok(())` on success and on the "no HOME / no XDG" no-op path so boot continues.
///
/// # Errors
///
/// Returns an error when directory creation itself fails (permission denied, ENOSPC); the caller (`main`) logs and continues.
pub fn run() -> Result<(), Box<dyn Error>> {
    run_with_env(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Import existing GNOME input preferences on the first host-session login.
///
/// Version 2 splits pointer settings by device class and preserves the host's
/// physical keyboard repeat values. Existing Tilo choices always win. The
/// migration marker is only written after at least one host setting could be
/// read (or the Tilo section was already complete), so a temporarily unavailable
/// settings service is retried on the next login.
///
/// # Errors
///
/// Returns an error when the shared settings store cannot persist the imported
/// values. Missing schemas or unreadable host values are ignored individually.
pub fn import_host_input_settings() -> Result<(), Box<dyn Error>> {
    use settings_store::hardware::INPUT_SECTION;
    // Ask the host outside the settings lock: every read starts a process.
    // The import is then decided again under the lock against the section as
    // it stands, so a choice saved in the meantime still wins.
    let mut host = std::collections::HashMap::new();
    let mut ask_host = |schema: &str, key: &str| {
        let value = gsettings_get(schema, key);
        host.insert((schema.to_string(), key.to_string()), value.clone());
        value
    };
    // The layout the machine was installed with stands in for a GNOME
    // session that lists no sources, as GNOME itself falls back to it.
    let system = settings_store::keyboard_sources::system_sources();
    let existing = settings_store::load_section(INPUT_SECTION);
    if host_input_migration_updates(&existing, &mut ask_host, &system).is_empty() {
        return Ok(());
    }
    let mut imported = false;
    settings_store::update_section_with(
        INPUT_SECTION,
        &settings_store::ChangeOrigin::action(HOST_INPUT_IMPORT_ORIGIN),
        &mut |existing| {
            let updates = host_input_migration_updates(
                existing,
                |schema, key| {
                    host.get(&(schema.to_string(), key.to_string()))
                        .cloned()
                        .flatten()
                },
                &system,
            );
            imported = !updates.is_empty();
            updates
        },
    )?;
    if imported {
        tracing::info!(
            version = HOST_INPUT_MIGRATION_VERSION,
            "firstboot: migrated host input settings"
        );
    }
    Ok(())
}

/// The change origin the host input import records.
const HOST_INPUT_IMPORT_ORIGIN: &str = "firstboot.import-host-input";

/// What the import adds to `existing`: host values read through `read`
/// (`gsettings get`), with `system` (the machine's keyboard layouts) used
/// when the host names no keyboard sources.
fn host_input_migration_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    mut read: impl FnMut(&str, &str) -> Option<String>,
    system: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    if existing
        .get(HOST_INPUT_MIGRATION_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|version| version >= HOST_INPUT_MIGRATION_VERSION)
    {
        return serde_json::Map::new();
    }

    let mut host_read_succeeded = false;
    let mut tracked_read = |schema: &str, key: &str| {
        let value = read(schema, key);
        host_read_succeeded |= value.is_some();
        value
    };
    let mut updates = host_keyboard_updates(existing, &mut tracked_read, system);
    updates.extend(host_pointer_updates(existing, &mut tracked_read));
    updates.extend(host_key_repeat_updates(existing, &mut tracked_read));

    if host_read_succeeded || host_input_settings_complete(existing) {
        updates.insert(
            HOST_INPUT_MIGRATION_VERSION_KEY.into(),
            serde_json::json!(HOST_INPUT_MIGRATION_VERSION),
        );
    }
    updates
}

fn host_input_settings_complete(existing: &serde_json::Map<String, serde_json::Value>) -> bool {
    let keyboard =
        existing.contains_key("keyboard_layout") || existing.contains_key("keyboard_layouts");
    let pointer = [
        "touchpad_natural_scroll",
        "mouse_natural_scroll",
        "touchpad_pointer_speed",
        "mouse_pointer_speed",
        "tap_to_click",
        "tap_drag",
        "disable_while_typing",
        "left_handed",
    ]
    .iter()
    .all(|key| existing.contains_key(*key));
    let repeat = existing.contains_key("key_repeat")
        || (existing.contains_key("key_repeat_delay_ms")
            && existing.contains_key("key_repeat_rate_hz"));
    keyboard && pointer && repeat
}

fn host_keyboard_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    read: &mut impl FnMut(&str, &str) -> Option<String>,
    system: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = host_layout_updates(existing, read, system);
    // The same whole-section writers store `compose_key: null`, meaning "no
    // compose key chosen", so null is as unowned as absent.
    if existing
        .get("compose_key")
        .is_none_or(serde_json::Value::is_null)
        && let Some(compose) = read("org.gnome.desktop.input-sources", "xkb-options")
            .and_then(|raw| parse_gnome_compose_key(&raw))
    {
        input.insert("compose_key".into(), serde_json::json!(compose));
    }
    input
}

/// The host's keyboard sources: GNOME's list, else the machine's layouts
/// (`XKB_DEFAULT_*`, localed). Without either the section stays unset and
/// the compositor follows the machine at runtime.
fn host_layout_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    read: &mut impl FnMut(&str, &str) -> Option<String>,
    system: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = serde_json::Map::new();
    let gnome = read("org.gnome.desktop.input-sources", "sources")
        .map(|raw| parse_gnome_xkb_sources(&raw))
        .unwrap_or_default();
    let source_ids = if gnome.is_empty() {
        system.to_vec()
    } else {
        gnome
    };
    let Some(active) = source_ids.first() else {
        return input;
    };
    // Version 2 persisted the settings-store default (`us`) before it consulted
    // the host, and any writer of the whole input section stores the same
    // default before this import first completes. Until then, treat only that
    // exact singleton/default state as unowned. A multi-layout list or any
    // non-US choice is an explicit Tilo preference and must continue to win.
    let unowned_us_default = existing.get("keyboard_layout") == Some(&serde_json::json!("us"))
        && existing.get("keyboard_layouts") == Some(&serde_json::json!(["us"]));
    if !existing.contains_key("keyboard_layout") && !existing.contains_key("keyboard_layouts")
        || unowned_us_default
    {
        input.insert("keyboard_layout".into(), serde_json::json!(active));
        input.insert("keyboard_layouts".into(), serde_json::json!(source_ids));
    }
    input
}

fn parse_gnome_xkb_sources(value: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut rest = value;
    while let Some((_, xkb_source)) = rest.split_once("('xkb', '") {
        let Some((source_id, remaining)) = xkb_source.split_once('\'') else {
            break;
        };
        let id = source_id.replacen('+', ":", 1);
        if settings_store::hardware::KeyboardInputSource::parse(&id).is_some()
            && !sources.contains(&id)
            && sources.len() < settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
        {
            sources.push(id);
        }
        rest = remaining;
    }
    sources
}

fn parse_gnome_compose_key(value: &str) -> Option<String> {
    ["ralt", "menu", "caps", "rwin", "lwin"]
        .into_iter()
        .find(|key| value.contains(&format!("compose:{key}")))
        .map(str::to_string)
}

fn host_pointer_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    read: &mut impl FnMut(&str, &str) -> Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = serde_json::Map::new();
    {
        let mut insert_bool = |key: &str, schema: &str, setting: &str| {
            if !existing.contains_key(key)
                && let Some(value) = read(schema, setting).and_then(|raw| match raw.trim() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                })
            {
                input.insert(key.into(), serde_json::json!(value));
            }
        };
        insert_bool(
            "touchpad_natural_scroll",
            "org.gnome.desktop.peripherals.touchpad",
            "natural-scroll",
        );
        insert_bool(
            "mouse_natural_scroll",
            "org.gnome.desktop.peripherals.mouse",
            "natural-scroll",
        );
        insert_bool(
            "tap_to_click",
            "org.gnome.desktop.peripherals.touchpad",
            "tap-to-click",
        );
        insert_bool(
            "tap_drag",
            "org.gnome.desktop.peripherals.touchpad",
            "tap-and-drag",
        );
        insert_bool(
            "tap_drag_lock",
            "org.gnome.desktop.peripherals.touchpad",
            "tap-and-drag-lock",
        );
        insert_bool(
            "disable_while_typing",
            "org.gnome.desktop.peripherals.touchpad",
            "disable-while-typing",
        );
        insert_bool(
            "left_handed",
            "org.gnome.desktop.peripherals.mouse",
            "left-handed",
        );
    }

    for (key, schema) in [
        (
            "touchpad_pointer_speed",
            "org.gnome.desktop.peripherals.touchpad",
        ),
        ("mouse_pointer_speed", "org.gnome.desktop.peripherals.mouse"),
    ] {
        if existing.contains_key(key) {
            continue;
        }
        if let Some(speed) = read(schema, "speed")
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|speed| speed.is_finite())
        {
            input.insert(key.into(), serde_json::json!(host_pointer_speed(speed)));
        }
    }
    input
}

fn parse_gnome_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn host_key_repeat_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    read: &mut impl FnMut(&str, &str) -> Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    const SCHEMA: &str = "org.gnome.desktop.peripherals.keyboard";
    let mut updates = serde_json::Map::new();
    if existing.contains_key("key_repeat")
        || existing.contains_key("key_repeat_delay_ms")
        || existing.contains_key("key_repeat_rate_hz")
    {
        return updates;
    }
    let Some(repeat) = read(SCHEMA, "repeat").and_then(|raw| parse_gnome_bool(&raw)) else {
        return updates;
    };
    if !repeat {
        updates.insert("key_repeat_delay_ms".into(), serde_json::json!(500));
        updates.insert("key_repeat_rate_hz".into(), serde_json::json!(0));
        return updates;
    }
    let Some(delay) = read(SCHEMA, "delay")
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|value| (1..=5_000).contains(value))
    else {
        return updates;
    };
    let Some(interval) = read(SCHEMA, "repeat-interval")
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|value| (1..=5_000).contains(value))
    else {
        return updates;
    };
    updates.insert("key_repeat_delay_ms".into(), serde_json::json!(delay));
    updates.insert(
        "key_repeat_rate_hz".into(),
        serde_json::json!(1_000_u32.checked_div(interval).unwrap_or(1).clamp(1, 1_000)),
    );
    updates
}

fn host_pointer_speed(speed: f64) -> f64 {
    speed.midpoint(1.0).clamp(0.0, 1.0)
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let mut command = Command::new("gsettings");
    command.args(["get", schema, key]);
    let output =
        bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(5)).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pure form of [`run`] that takes env-var values as arguments instead
/// of reading the process environment — tests drive this with a `tempdir`-rooted fixture so the suite never pokes the developer's `$HOME`.
///
/// # Errors
///
/// Returns an error when directory creation fails (permission denied, ENOSPC).
pub fn run_with_env(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<(), Box<dyn Error>> {
    let Some(config_dir) = resolve_config_dir(xdg_config_home, home) else {
        tracing::warn!(
            "firstboot: neither XDG_CONFIG_HOME nor HOME set; skipping config-dir creation"
        );
        return Ok(());
    };
    ensure_dirs(&config_dir)?;
    tracing::info!(
        "firstboot: ensured tilo-desktop config dir at {}",
        config_dir.display()
    );
    Ok(())
}

/// Compute the tilo-desktop config dir (`<config-base>/tilo-desktop`) following the same XDG fallback rules the peer-sync marker uses.
fn resolve_config_dir(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(
        tilo_paths::xdg::Base::Config
            .resolve_from(xdg_config_home, home)?
            .join("tilo-desktop"),
    )
}

fn ensure_dirs(config_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The import on a machine that names no keyboard layout of its own.
    fn migration_updates(
        existing: &serde_json::Map<String, serde_json::Value>,
        read: impl FnMut(&str, &str) -> Option<String>,
    ) -> serde_json::Map<String, serde_json::Value> {
        host_input_migration_updates(existing, read, &[])
    }

    fn keyboard_updates(
        existing: &serde_json::Map<String, serde_json::Value>,
        read: &mut impl FnMut(&str, &str) -> Option<String>,
    ) -> serde_json::Map<String, serde_json::Value> {
        host_keyboard_updates(existing, read, &[])
    }

    #[test]
    fn the_machine_layout_seeds_an_unset_or_unowned_us_keyboard() {
        let system = ["de:nodeadkeys".to_string()];
        let gnome_has_none = |schema: &str, key: &str| match (schema, key) {
            ("org.gnome.desktop.input-sources", "sources") => Some("@a(ss) []".into()),
            _ => None,
        };
        // A fresh section, GNOME listing nothing: the installed layout.
        let seeded = host_input_migration_updates(&serde_json::Map::new(), gnome_has_none, &system);
        assert_eq!(
            seeded.get("keyboard_layout"),
            Some(&serde_json::json!("de:nodeadkeys"))
        );
        assert_eq!(
            seeded.get("keyboard_layouts"),
            Some(&serde_json::json!(["de:nodeadkeys"]))
        );
        // Version four already ran but left the whole-section writers' `us`.
        let stale = serde_json::Map::from_iter([
            ("keyboard_layout".into(), serde_json::json!("us")),
            ("keyboard_layouts".into(), serde_json::json!(["us"])),
            (
                HOST_INPUT_MIGRATION_VERSION_KEY.into(),
                serde_json::json!(4),
            ),
        ]);
        let repaired = host_input_migration_updates(&stale, |_, _| None, &system);
        assert_eq!(
            repaired.get("keyboard_layouts"),
            Some(&serde_json::json!(["de:nodeadkeys"]))
        );
        // GNOME's own list still wins over the machine's.
        let gnome = host_input_migration_updates(
            &serde_json::Map::new(),
            |schema, key| {
                (schema == "org.gnome.desktop.input-sources" && key == "sources")
                    .then(|| "[('xkb', 'fr')]".into())
            },
            &system,
        );
        assert_eq!(
            gnome.get("keyboard_layouts"),
            Some(&serde_json::json!(["fr"]))
        );
        // A layout the person chose is never replaced.
        let chosen = serde_json::Map::from_iter([
            ("keyboard_layout".into(), serde_json::json!("us")),
            ("keyboard_layouts".into(), serde_json::json!(["us", "fr"])),
        ]);
        assert!(
            !host_input_migration_updates(&chosen, |_, _| None, &system)
                .contains_key("keyboard_layouts")
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn host_pointer_settings_import_once_without_overwriting_tilo_choices() {
        // Phase `host_pointer_speed_maps_libinput_range_to_tilo_range`.
        {
            assert_eq!(host_pointer_speed(-1.0), 0.0);
            assert_eq!(host_pointer_speed(0.0), 0.5);
            assert_eq!(host_pointer_speed(1.0), 1.0);
            assert_eq!(host_pointer_speed(4.0), 1.0);
        }
        // Phase `host_input_migration_splits_legacy_scroll_and_is_idempotent`.
        {
            let legacy =
                serde_json::Map::from_iter([("natural_scroll".into(), serde_json::json!(true))]);
            let updates = migration_updates(&legacy, |schema, key| match (schema, key) {
                ("org.gnome.desktop.peripherals.touchpad", "natural-scroll") => Some("true".into()),
                ("org.gnome.desktop.peripherals.mouse", "natural-scroll") => Some("false".into()),
                _ => None,
            });
            assert_eq!(
                updates.get("touchpad_natural_scroll"),
                Some(&serde_json::json!(true))
            );
            assert_eq!(
                updates.get("mouse_natural_scroll"),
                Some(&serde_json::json!(false))
            );
            assert_eq!(
                updates.get(HOST_INPUT_MIGRATION_VERSION_KEY),
                Some(&serde_json::json!(HOST_INPUT_MIGRATION_VERSION))
            );

            let mut migrated = legacy;
            migrated.extend(updates);
            assert!(migration_updates(&migrated, |_, _| panic!("must not read twice")).is_empty());
        }
        // Phase `host_pointer_import_keeps_device_classes_independent`.
        {
            let existing = serde_json::Map::new();
            let updates = host_pointer_updates(&existing, &mut |schema, key| match (schema, key) {
                ("org.gnome.desktop.peripherals.touchpad", "natural-scroll") => Some("true".into()),
                ("org.gnome.desktop.peripherals.mouse", "natural-scroll") => Some("false".into()),
                ("org.gnome.desktop.peripherals.touchpad", "speed") => Some("0.4".into()),
                ("org.gnome.desktop.peripherals.mouse", "speed") => Some("-0.6".into()),
                _ => None,
            });
            assert_eq!(
                updates.get("touchpad_natural_scroll"),
                Some(&serde_json::json!(true))
            );
            assert_eq!(
                updates.get("mouse_natural_scroll"),
                Some(&serde_json::json!(false))
            );
            assert_eq!(
                updates.get("touchpad_pointer_speed"),
                Some(&serde_json::json!(0.7))
            );
            assert_eq!(
                updates.get("mouse_pointer_speed"),
                Some(&serde_json::json!(0.2))
            );
        }
        // Phase `host_input_migration_preserves_explicit_per_device_tilo_choices`.
        {
            let existing = serde_json::Map::from_iter([
                ("touchpad_natural_scroll".into(), serde_json::json!(false)),
                ("mouse_natural_scroll".into(), serde_json::json!(true)),
                ("touchpad_pointer_speed".into(), serde_json::json!(0.8)),
            ]);
            let updates = migration_updates(&existing, |_, key| match key {
                "natural-scroll" => Some("true".into()),
                "speed" => Some("-0.5".into()),
                _ => None,
            });
            assert!(!updates.contains_key("touchpad_natural_scroll"));
            assert!(!updates.contains_key("mouse_natural_scroll"));
            assert!(!updates.contains_key("touchpad_pointer_speed"));
            assert_eq!(
                updates.get("mouse_pointer_speed"),
                Some(&serde_json::json!(0.25))
            );
        }
        // Phase `version_three_repairs_only_the_stale_version_two_us_default`.
        {
            let stale = serde_json::Map::from_iter([
                ("keyboard_layout".into(), serde_json::json!("us")),
                ("keyboard_layouts".into(), serde_json::json!(["us"])),
                (
                    HOST_INPUT_MIGRATION_VERSION_KEY.into(),
                    serde_json::json!(2),
                ),
            ]);
            let repaired = migration_updates(&stale, |schema, key| {
                (schema == "org.gnome.desktop.input-sources" && key == "sources")
                    .then(|| "[('xkb', 'de+nodeadkeys')]".into())
            });
            assert_eq!(
                repaired.get("keyboard_layout"),
                Some(&serde_json::json!("de:nodeadkeys"))
            );
            assert_eq!(
                repaired.get("keyboard_layouts"),
                Some(&serde_json::json!(["de:nodeadkeys"]))
            );
            assert_eq!(
                repaired.get(HOST_INPUT_MIGRATION_VERSION_KEY),
                Some(&serde_json::json!(HOST_INPUT_MIGRATION_VERSION))
            );

            // Version three kept a null compose key; four imports the host's.
            let null_compose = serde_json::Map::from_iter([
                ("keyboard_layout".into(), serde_json::json!("de:nodeadkeys")),
                ("compose_key".into(), serde_json::Value::Null),
                (
                    HOST_INPUT_MIGRATION_VERSION_KEY.into(),
                    serde_json::json!(3),
                ),
            ]);
            let repaired = migration_updates(&null_compose, |schema, key| {
                (schema == "org.gnome.desktop.input-sources" && key == "xkb-options")
                    .then(|| "['compose:rwin']".into())
            });
            assert_eq!(
                repaired.get("compose_key"),
                Some(&serde_json::json!("rwin"))
            );
            assert!(!repaired.contains_key("keyboard_layout"));

            let explicit = serde_json::Map::from_iter([
                ("keyboard_layout".into(), serde_json::json!("us")),
                (
                    "keyboard_layouts".into(),
                    serde_json::json!(["us", "de:nodeadkeys"]),
                ),
                (
                    HOST_INPUT_MIGRATION_VERSION_KEY.into(),
                    serde_json::json!(2),
                ),
            ]);
            let preserved = migration_updates(&explicit, |schema, key| {
                (schema == "org.gnome.desktop.input-sources" && key == "sources")
                    .then(|| "[('xkb', 'de+nodeadkeys')]".into())
            });
            assert!(!preserved.contains_key("keyboard_layout"));
            assert!(!preserved.contains_key("keyboard_layouts"));
        }
        // Phase `defaults_written_before_the_first_import_do_not_pin_us`.
        {
            let defaults = serde_json::Map::from_iter([
                ("keyboard_layout".into(), serde_json::json!("us")),
                ("keyboard_layouts".into(), serde_json::json!(["us"])),
                ("compose_key".into(), serde_json::Value::Null),
                ("key_repeat".into(), serde_json::json!(0.5)),
            ]);
            let imported = migration_updates(&defaults, |schema, key| match (schema, key) {
                ("org.gnome.desktop.input-sources", "sources") => {
                    Some("[('xkb', 'de+nodeadkeys')]".into())
                }
                ("org.gnome.desktop.input-sources", "xkb-options") => {
                    Some("['compose:rwin']".into())
                }
                _ => None,
            });
            assert_eq!(
                imported.get("keyboard_layouts"),
                Some(&serde_json::json!(["de:nodeadkeys"]))
            );
            assert_eq!(
                imported.get("compose_key"),
                Some(&serde_json::json!("rwin")),
                "a null compose key is the unset default, not a choice"
            );
            assert_eq!(
                imported.get(HOST_INPUT_MIGRATION_VERSION_KEY),
                Some(&serde_json::json!(HOST_INPUT_MIGRATION_VERSION))
            );
        }
    }

    #[test]
    fn host_keyboard_settings_import_completely_and_retry_when_unavailable() {
        // Phase `host_keyboard_import_is_complete_and_never_overwrites_tilo_choices`.
        {
            let empty = serde_json::Map::new();
            let imported = keyboard_updates(&empty, &mut |_, key| match key {
                "sources" => Some("[('xkb', 'de+nodeadkeys'), ('xkb', 'us')]".into()),
                "xkb-options" => Some("['compose:rwin']".into()),
                _ => None,
            });
            assert_eq!(
                imported.get("keyboard_layout"),
                Some(&serde_json::json!("de:nodeadkeys"))
            );
            assert_eq!(
                imported.get("keyboard_layouts"),
                Some(&serde_json::json!(["de:nodeadkeys", "us"]))
            );
            assert_eq!(
                imported.get("compose_key"),
                Some(&serde_json::json!("rwin"))
            );

            let existing = serde_json::Map::from_iter([
                ("keyboard_layout".into(), serde_json::json!("us")),
                ("compose_key".into(), serde_json::json!("caps")),
            ]);
            assert!(
                keyboard_updates(&existing, &mut |_, _| Some("[('xkb', 'de')]".into())).is_empty(),
                "an existing Tilo keyboard selection must win as one coherent preference"
            );
        }
        // Phase `host_repeat_import_preserves_exact_values_and_disabled_state`.
        {
            let enabled =
                migration_updates(&serde_json::Map::new(), |schema, key| match (schema, key) {
                    ("org.gnome.desktop.peripherals.keyboard", "repeat") => Some("true".into()),
                    ("org.gnome.desktop.peripherals.keyboard", "delay") => Some("300".into()),
                    ("org.gnome.desktop.peripherals.keyboard", "repeat-interval") => {
                        Some("25".into())
                    }
                    _ => None,
                });
            assert_eq!(
                enabled.get("key_repeat_delay_ms"),
                Some(&serde_json::json!(300))
            );
            assert_eq!(
                enabled.get("key_repeat_rate_hz"),
                Some(&serde_json::json!(40))
            );

            let disabled = migration_updates(&serde_json::Map::new(), |schema, key| {
                (schema == "org.gnome.desktop.peripherals.keyboard" && key == "repeat")
                    .then(|| "false".into())
            });
            assert_eq!(
                disabled.get("key_repeat_rate_hz"),
                Some(&serde_json::json!(0))
            );
        }
        // Phase `unavailable_host_settings_are_retried_instead_of_marked_complete`.
        {
            let existing = serde_json::Map::new();
            let unavailable = migration_updates(&existing, |_, _| None);
            assert!(unavailable.is_empty());

            let retried = migration_updates(&existing, |schema, key| {
                (schema == "org.gnome.desktop.input-sources" && key == "sources")
                    .then(|| "[('xkb', 'de+nodeadkeys')]".into())
            });
            assert_eq!(
                retried.get("keyboard_layout"),
                Some(&serde_json::json!("de:nodeadkeys"))
            );
            assert_eq!(
                retried.get(HOST_INPUT_MIGRATION_VERSION_KEY),
                Some(&serde_json::json!(HOST_INPUT_MIGRATION_VERSION))
            );
        }
        // Phase `gnome_xkb_sources_keep_order_and_variants`.
        {
            assert_eq!(
                parse_gnome_xkb_sources(
                    "[('xkb', 'de+nodeadkeys'), ('ibus', 'typing-booster'), ('xkb', 'us')]"
                ),
                ["de:nodeadkeys", "us"]
            );
        }
        // Phase `gnome_compose_option_maps_to_the_shared_settings_schema`.
        {
            assert_eq!(
                parse_gnome_compose_key("['grp:alt_shift_toggle', 'compose:rwin']").as_deref(),
                Some("rwin")
            );
            assert_eq!(parse_gnome_compose_key("@as []"), None);
        }
    }

    /// Minimal tempdir helper (avoids pulling `tempfile` as a dev-dep — the only test in `quick-shell` that needs one).
    #[test]
    fn firstboot_run_creates_the_desktop_dir_and_never_provisions_identity() {
        // Phase `run_with_xdg_creates_tilo_desktop_dir`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            run_with_env(Some(tmp.path().as_os_str()), None).expect("firstboot succeeds");
            let expected = tmp.path().join("tilo-desktop");
            assert!(
                expected.exists() && expected.is_dir(),
                "expected {expected:?} to be created"
            );
        }
        // Phase `run_with_home_falls_back_to_dot_config`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            run_with_env(None, Some(tmp.path().as_os_str())).expect("firstboot succeeds via HOME");
            let expected = tmp.path().join(".config").join("tilo-desktop");
            assert!(
                expected.exists() && expected.is_dir(),
                "expected {expected:?} to be created"
            );
        }
        // Phase `run_is_idempotent_on_existing_dir`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            run_with_env(Some(tmp.path().as_os_str()), None).expect("first run ok");
            run_with_env(Some(tmp.path().as_os_str()), None).expect("second run ok");
            let expected = tmp.path().join("tilo-desktop");
            assert!(expected.exists());
        }
        // Phase `run_without_env_is_a_noop_not_an_error`.
        {
            // Systemd `Type=oneshot` units fail the whole transaction if ExecStart
            // returns non-zero — first-boot must never break boot over missing env-vars in the unit context.
            run_with_env(None, None).expect("no-env case is a no-op, not an error");
        }
        // Phase `run_does_not_provision_identity_when_missing`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            run_with_env(Some(tmp.path().as_os_str()), None).expect("firstboot ok");
            let identity_file = tmp
                .path()
                .join("tilo")
                .join("identity")
                .join(PRIMARY_IDENTITY_FILE);
            assert!(
                !identity_file.exists(),
                "firstboot must leave personal identity creation to the OOBE"
            );
        }
        // Phase `run_does_not_touch_an_existing_identity`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let dir = tmp.path().join("tilo").join("identity");
            std::fs::create_dir_all(&dir).unwrap();
            let identity_file = dir.join(PRIMARY_IDENTITY_FILE);
            std::fs::write(&identity_file, b"MARKER: hands off").unwrap();
            run_with_env(Some(tmp.path().as_os_str()), None).expect("firstboot ok");
            assert_eq!(std::fs::read(identity_file).unwrap(), b"MARKER: hands off");
        }
    }
}
