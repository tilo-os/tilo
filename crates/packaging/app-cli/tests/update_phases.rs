//! What `app update` costs when little changed, phase by phase, on a
//! loopback registry: the device has many apps installed, each the size of a
//! real one, and the registry offers either nothing new or one newer app.
//!
//! The work must follow what changed: one catalogue request however many
//! apps are installed, a download only for the app that changed, and nothing
//! re-read or re-verified for the apps that did not. The test pins those
//! counts and reports where the time goes (catalogue, comparison, download
//! and verification, install, and the collection an install runs after it)
//! without judging the wall time, which depends on the machine.
//!
//! `cargo test -p app-cli --test it update_phases -- --nocapture`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::time::{Duration, Instant};

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;

use crate::registry_to_installed::{
    author_tapp_with_files, scoped_env, signed_entry_version, test_publisher, tilo_install_bin,
};
use crate::update_latency::{FarRegistry, snapshot};

/// Installed apps.
const APPS: usize = 40;
/// Files in each app's payload besides its entrypoint, and their size: about
/// what a real app ships.
const FILES: usize = 48;
const FILE_BYTES: usize = 16 * 1024;

fn payload(id: &str) -> Vec<(String, Vec<u8>)> {
    (0..FILES)
        .map(|index| {
            let mut bytes = vec![0_u8; FILE_BYTES];
            let mut hasher = blake3::Hasher::new();
            hasher.update(id.as_bytes());
            hasher.update(&index.to_le_bytes());
            hasher.finalize_xof().fill(&mut bytes);
            (format!("share/{index}.bin"), bytes)
        })
        .collect()
}

fn app_id(index: usize) -> String {
    format!("so.tilo.update-phases-{index}")
}

/// Publish `versions[i]` of app `i` and return the registry entries.
fn publish(
    registry: &FarRegistry,
    base: &str,
    versions: &[&str],
) -> Vec<registry_types::RegistryEntry> {
    versions
        .iter()
        .enumerate()
        .map(|(index, version)| {
            let id = app_id(index);
            let tapp = author_tapp_with_files(&id, version, &test_publisher(), &payload(&id));
            let path = format!("/artifacts/{id}-{version}.tapp");
            registry.put(
                &format!("{path}{}", tilo_app_package::compressed::SUFFIX),
                tilo_app_package::compressed::compress(&tapp).unwrap(),
            );
            registry.put(&path, tapp.clone());
            signed_entry_version(&id, version, &format!("{base}{path}"), &tapp)
        })
        .collect()
}

fn timed<T>(
    phases: &mut Vec<(&'static str, Duration)>,
    name: &'static str,
    f: impl FnOnce() -> T,
) -> T {
    let started = Instant::now();
    let out = f();
    phases.push((name, started.elapsed()));
    out
}

fn report(title: &str, phases: &[(&'static str, Duration)]) {
    let total: Duration = phases.iter().map(|(_, took)| *took).sum();
    let parts = phases
        .iter()
        .map(|(name, took)| format!("{name} {took:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("perf {title}: {total:?} = {parts}");
}

fn app_update() -> Duration {
    let started = Instant::now();
    let update = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .arg("update")
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        update.status.success(),
        "app update: {}\n{}",
        String::from_utf8_lossy(&update.stdout),
        String::from_utf8_lossy(&update.stderr)
    );
    elapsed
}

fn artifacts(seen: &[String]) -> Vec<&str> {
    seen.iter()
        .map(String::as_str)
        .filter(|path| path.starts_with("/artifacts/"))
        .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture measured in its phases, then end to end"
)]
fn an_update_with_little_to_fetch_does_work_only_for_what_changed() {
    let registry = FarRegistry::default();
    let base = registry.start();
    let owner = SigningKey::from_bytes(&[29_u8; 32]);

    let config = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "near".into(),
                base_url: base.clone(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let installer = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", config.path()),
        ("TILO_STORE_CACHE_DIR", cache.path()),
        ("TILO_APPS_DATA_DIR", apps.path()),
        ("TILO_INSTALL_BIN", &installer),
        ("XDG_DATA_HOME", data.path()),
    ]);

    // The device: every app installed at 1.0.0.
    let mut versions = vec!["1.0.0"; APPS];
    let first = snapshot(publish(&registry, &base, &versions), &owner, None);
    registry.put("/entries", serde_json::to_vec(&first).unwrap());
    let started = Instant::now();
    for offer in app_cli::available().apps {
        app_cli::install::install_app(&offer).unwrap();
    }
    eprintln!(
        "perf installing {APPS} apps one by one: {:?}",
        started.elapsed()
    );
    assert_eq!(package_store::list_installed().unwrap().len(), APPS);
    registry.take_seen();

    // Nothing new: the catalogue is the whole of the network traffic.
    let nothing = app_update();
    let seen = registry.take_seen();
    eprintln!("perf app update with nothing to update ({APPS} apps installed): {nothing:?}");
    assert_eq!(
        seen,
        ["/entries"],
        "an update with nothing new fetched {seen:?}"
    );

    // One app is newer, measured phase by phase as `app update` runs them.
    versions[0] = "1.0.1";
    let second = snapshot(publish(&registry, &base, &versions), &owner, Some(&first));
    registry.put("/entries", serde_json::to_vec(&second).unwrap());
    let mut phases = Vec::new();
    let installed = timed(&mut phases, "installed apps", || {
        app_cli::installed_for_update().unwrap()
    });
    let listing = timed(&mut phases, "catalogue", app_cli::all_offers);
    let outdated: Vec<&app_cli::CatalogueApp> = timed(&mut phases, "compare", || {
        listing
            .apps
            .iter()
            .filter(|app| {
                installed.get(&app.package_id).is_some_and(|installed| {
                    tilo_app_package::version::compare(&app.version, &installed.version).is_gt()
                        || !installed.current
                })
            })
            .collect()
    });
    assert_eq!(
        outdated
            .iter()
            .map(|app| app.package_id.as_str())
            .collect::<Vec<_>>(),
        [app_id(0)]
    );
    let changed = outdated.first().copied().unwrap();
    let staged = timed(&mut phases, "download and verify", || {
        app_cli::install::stage(changed, &mut |_| {}).unwrap()
    });
    timed(&mut phases, "install (tilo-install update)", || {
        app_cli::install::update_staged_reporting(changed, staged, &mut |_| {}).unwrap();
    });
    report("one changed app, by phase", &phases);
    let collection = Instant::now();
    let gc = std::process::Command::new(&installer)
        .arg("gc")
        .output()
        .unwrap();
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    eprintln!(
        "perf the collection each install runs after it, alone: {:?}",
        collection.elapsed()
    );
    let seen = registry.take_seen();
    assert_eq!(
        seen.iter().filter(|path| *path == "/entries").count(),
        1,
        "{seen:?}"
    );
    assert_eq!(artifacts(&seen).len(), 1, "{seen:?}");

    // Another app is newer, end to end.
    versions[1] = "1.0.1";
    let third = snapshot(publish(&registry, &base, &versions), &owner, Some(&second));
    registry.put("/entries", serde_json::to_vec(&third).unwrap());
    let one = app_update();
    let seen = registry.take_seen();
    eprintln!("perf app update with one changed app ({APPS} apps installed): {one:?}");
    assert_eq!(
        seen.iter().filter(|path| *path == "/entries").count(),
        1,
        "{seen:?}"
    );
    let fetched = artifacts(&seen);
    assert_eq!(fetched.len(), 1, "{seen:?}");
    assert!(
        fetched
            .first()
            .is_some_and(|path| path.contains(&app_id(1))),
        "{seen:?}"
    );
    let rows = package_store::list_installed().unwrap();
    assert_eq!(rows.len(), APPS);
    for row in &rows {
        let expected = if row.id == app_id(0) || row.id == app_id(1) {
            "1.0.1"
        } else {
            "1.0.0"
        };
        assert_eq!(row.version, expected, "{}", row.id);
    }
}
