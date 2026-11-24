//! What a device is offered from its repositories: only builds it can run, under the channel it
//! chose, once per app.

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::daemon_fixture::Daemon;
use crate::registry_fixture::{
    Device, Registry, author_tapp_version, native_target, registry_config, signed_entry_version,
    test_publisher,
};

fn publisher() -> SigningKey {
    SigningKey::from_bytes(&[11_u8; 32])
}

fn resigned(mut entry: RegistryEntry) -> RegistryEntry {
    entry.signature_hex.clear();
    sign_entry(&mut entry, &publisher());
    entry
}

fn snapshot(entries: Vec<RegistryEntry>, owner: &SigningKey) -> String {
    let now = tilo_clock::try_unix_now().unwrap();
    let mut publishers: Vec<PublisherAuthorization> = Vec::new();
    for entry in &entries {
        if !publishers
            .iter()
            .any(|known| known.pubkey_hex == entry.publisher_pubkey_hex)
        {
            publishers.push(PublisherAuthorization {
                pubkey_hex: entry.publisher_pubkey_hex.clone(),
                can_add_new: true,
            });
        }
    }
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: 1,
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: String::new(),
        moderation_head_hex: String::new(),
        publishers,
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    serde_json::to_string(&snapshot).unwrap()
}

fn bot_entry() -> RegistryEntry {
    let hash = "ab".repeat(32);
    let mut entry = RegistryEntry {
        package_id: "so.tilo.bots.echo".into(),
        kind: registry_types::EntryKind::Bot,
        name: "Echo".into(),
        description: "Echoes".into(),
        tagline: "A bot".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Bots".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher().verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "1.0.0".into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Bot(registry_types::BotArtifact {
                program_url: registry_types::bot_program_url("https://apps.tilo.so", &hash),
                program_blake3_hex: hash,
                program_size_bytes: 10,
                requires: registry_types::BotRequirements::default(),
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher());
    entry
}

fn android_version(mut version: RegistryVersion, number: &str) -> RegistryVersion {
    version.version = number.into();
    let tapp = version.tapp_mut().unwrap();
    tapp.tapp_url = format!("https://apps.tilo.so/artifacts/android-{number}.tapp");
    tapp.target = registry_types::RegistryTarget {
        platform: registry_types::ANDROID_PLATFORM.into(),
        architecture: "aarch64".into(),
        isa: "aarch64".into(),
        runtime: None,
        interfaces: Vec::new(),
    };
    version
}

const PLAIN: &str = "so.tilo.sel-plain";
const PREVIEW: &str = "so.tilo.sel-preview";
const ANDROID_MIXED: &str = "so.tilo.sel-android-mixed";
const ANDROID_ONLY: &str = "so.tilo.sel-android-only";
const NEW_SYSTEM: &str = "so.tilo.sel-new-system";
const NEW_SYSTEM_MIXED: &str = "so.tilo.sel-system-mixed";
const DUP: &str = "so.tilo.sel-dup";

tilo_test_env::scenario! {
    fn the_listing_offers_only_what_this_device_can_run_on_its_channel;

    phase the_listing_filters_builds_by_platform_interface_and_kind;
    {
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let mirror_owner = SigningKey::from_bytes(&[13; 32]);
        let tapp = author_tapp_version(PLAIN, "1.4.0", &publisher);

        let plain = signed_entry_version(
            PLAIN,
            "1.4.0",
            "https://apps.tilo.so/artifacts/plain-1.4.0.tapp",
            &tapp,
        );

        let mut preview = signed_entry_version(
            PREVIEW,
            "1.0.0",
            "https://apps.tilo.so/artifacts/preview-1.0.0.tapp",
            &tapp,
        );
        for (number, path) in [
            ("1.0.1", "preview-1.0.1.tapp"),
            ("1.1.0-dev.9", "preview-1.1.0-dev.9.tapp"),
        ] {
            let mut version = preview.versions[0].clone();
            version.version = number.into();
            version.tapp_mut().unwrap().tapp_url =
                format!("https://apps.tilo.so/artifacts/{path}");
            preview.versions.push(version);
        }
        let preview = resigned(preview);

        let mut mixed = signed_entry_version(
            ANDROID_MIXED,
            "1.4.0",
            "https://apps.tilo.so/artifacts/mixed-1.4.0.tapp",
            &tapp,
        );
        let android = android_version(mixed.versions[0].clone(), "9.0.0");
        mixed.versions.push(android);
        let mixed = resigned(mixed);

        let mut android_only = signed_entry_version(
            ANDROID_ONLY,
            "9.0.0",
            "https://apps.tilo.so/artifacts/android-only.tapp",
            &tapp,
        );
        android_only.versions = vec![android_version(android_only.versions[0].clone(), "9.0.0")];
        let android_only = resigned(android_only);

        let mut new_system = signed_entry_version(
            NEW_SYSTEM,
            "1.0.0",
            "https://apps.tilo.so/artifacts/new-system.tapp",
            &tapp,
        );
        new_system.versions[0].tapp_mut().unwrap().target.interfaces =
            vec!["system@999".into()];
        let new_system = resigned(new_system);

        // The newest build asks for an interface this OS does not serve; the
        // older build that fits is the one offered.
        let mut mixed_system = signed_entry_version(
            NEW_SYSTEM_MIXED,
            "1.4.0",
            "https://apps.tilo.so/artifacts/system-mixed-1.4.0.tapp",
            &tapp,
        );
        let mut too_new = mixed_system.versions[0].clone();
        too_new.version = "2.0.0".into();
        too_new.tapp_mut().unwrap().tapp_url =
            "https://apps.tilo.so/artifacts/system-mixed-2.0.0.tapp".into();
        too_new.tapp_mut().unwrap().target.interfaces = vec!["system@999".into()];
        mixed_system.versions.push(too_new);
        let mixed_system = resigned(mixed_system);

        let primary_dup = signed_entry_version(
            DUP,
            "1.4.0",
            "https://apps.tilo.so/artifacts/dup.tapp",
            &tapp,
        );
        let secondary_dup = signed_entry_version(
            DUP,
            "1.4.0",
            "https://apps.tilo.so/artifacts/dup.tapp",
            &tapp,
        );

        let primary = Registry::start(
            snapshot(
                vec![
                    plain,
                    preview,
                    mixed,
                    android_only,
                    new_system,
                    mixed_system,
                    primary_dup,
                    bot_entry(),
                ],
                &owner,
            ),
            tapp.clone(),
        );
        let secondary = Registry::start(snapshot(vec![secondary_dup], &mirror_owner), tapp);

        let interfaces = tempfile::tempdir().unwrap();
        let declaration = interfaces.path().join("system-interfaces.toml");
        std::fs::write(&declaration, "[interfaces]\nsystem = 1\n").unwrap();
        let device = Device::new(None, &[("TILO_SYSTEM_INTERFACES", &declaration)]);
        std::fs::write(
            device.config.path().join("registries.toml"),
            toml::to_string_pretty(&app_cli::RegistriesFile {
                registries: vec![
                    registry_config("primary", &primary.base_url(), &owner),
                    registry_config("secondary", &secondary.base_url(), &mirror_owner),
                ],
            })
            .unwrap(),
        )
        .unwrap();

        let listing = app_cli::available();
        assert!(
            listing.unreachable.is_empty(),
            "both registries are reachable: {:?}",
            listing.unreachable
        );
        let find = |id: &str| listing.apps.iter().find(|app| app.package_id == id);

        let plain = find(PLAIN).expect("a native build on this OS is offered");
        assert_eq!(plain.display_name, "Sheets");
        assert_eq!(plain.version, "1.4.0");
        assert_eq!(plain.category.as_deref(), Some("Office"));
        assert_eq!(plain.registry, "primary");
        assert_eq!(plain.trust, "publisher-signed");
        assert_eq!(
            plain.download_url,
            "https://apps.tilo.so/artifacts/plain-1.4.0.tapp"
        );
        assert_eq!(plain.architecture, native_target().0);

        assert_eq!(
            find(PREVIEW).expect("the stable release is offered").version,
            "1.0.1",
            "the newest stable release is offered; a prerelease waits for a development channel"
        );
        assert_eq!(
            find(ANDROID_MIXED)
                .expect("the native build is offered")
                .version,
            "1.4.0",
            "a newer Android build of the same app is not offered to Tilo OS"
        );
        assert!(
            find(ANDROID_ONLY).is_none(),
            "an Android-only app is never offered"
        );
        assert!(
            find(NEW_SYSTEM).is_none(),
            "a build for an interface this OS does not serve is not offered"
        );
        assert_eq!(
            find(NEW_SYSTEM_MIXED)
                .expect("the older build that fits is offered")
                .version,
            "1.4.0",
            "the newer build asks for an interface this OS does not serve"
        );
        assert!(find("so.tilo.bots.echo").is_none(), "a bot is not an app");
        for app in &listing.apps {
            assert!(
                !app.version.is_empty() && !app.download_url.is_empty(),
                "a listing never offers an app without a version to fetch: {app:?}"
            );
        }
        let dup: Vec<_> = listing.apps.iter().filter(|app| app.package_id == DUP).collect();
        assert_eq!(dup.len(), 1, "one app is listed once: {dup:?}");
        assert_eq!(
            dup[0].registry, "primary",
            "the user's repository order breaks the tie"
        );
    }

    phase a_development_channel_offers_the_prerelease;
    {
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let tapp = author_tapp_version(PREVIEW, "1.0.0", &publisher);
        let mut entry = signed_entry_version(
            PREVIEW,
            "1.0.0",
            "https://apps.tilo.so/artifacts/preview-1.0.0.tapp",
            &tapp,
        );
        let mut dev = entry.versions[0].clone();
        dev.version = "1.1.0-dev.9".into();
        dev.tapp_mut().unwrap().tapp_url =
            "https://apps.tilo.so/artifacts/preview-1.1.0-dev.9.tapp".into();
        entry.versions.push(dev);
        let entry = resigned(entry);
        let registry = Registry::start(snapshot(vec![entry], &owner), tapp);

        let device = Device::new(
            Some(registry_config("local", &registry.base_url(), &owner)),
            &[],
        );
        let _daemon = Daemon::start(device.apps.path());
        let offered = || {
            app_cli::available()
                .apps
                .into_iter()
                .find(|app| app.package_id == PREVIEW)
                .expect("the app is offered")
                .version
        };
        assert_eq!(offered(), "1.0.0");
        let selected = std::process::Command::new(tilo_test_env::package_binary!("app"))
            .args(["repo", "channel", "local", "development"])
            .output_locked()
            .unwrap();
        assert!(selected.status.success(), "{selected:?}");
        assert_eq!(
            offered(),
            "1.1.0-dev.9",
            "the chosen channel offers its prerelease"
        );
        assert!(
            device.config.path().join("registries.toml").exists(),
            "the channel selection is persisted"
        );
    }

    phase the_listing_json_names_every_field_the_store_reads;
    {
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let tapp = author_tapp_version(PLAIN, "1.4.0", &publisher);
        let entry = signed_entry_version(
            PLAIN,
            "1.4.0",
            "https://apps.tilo.so/artifacts/plain-1.4.0.tapp",
            &tapp,
        );
        let twin = signed_entry_version(
            "so.tilo.sel-plain-two",
            "1.4.0",
            "https://apps.tilo.so/artifacts/plain-two-1.4.0.tapp",
            &tapp,
        );
        let registry = Registry::start(snapshot(vec![entry, twin], &owner), tapp);
        let _device = Device::new(
            Some(registry_config("local", &registry.base_url(), &owner)),
            &[],
        );

        let out = std::process::Command::new(tilo_test_env::package_binary!("app"))
            .args(["list", "--json"])
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            listing["unreachable"],
            serde_json::json!([]),
            "the reachable registry reports nothing unreachable"
        );
        let app = listing["apps"]
            .as_array()
            .expect("apps is an array")
            .iter()
            .find(|app| app["package_id"] == PLAIN)
            .and_then(serde_json::Value::as_object)
            .expect("the listed app is an object");
        let mut keys: Vec<&str> = app.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = [
            "package_id",
            "display_name",
            "description",
            "tagline",
            "images",
            "supported_languages",
            "category",
            "version",
            "registry",
            "download_url",
            "tapp_root_hash_hex",
            "tapp_size_bytes",
            "tapp_blake3_hex",
            "tapp_surface_hex",
            "architecture",
            "isa",
            "runtime",
            "publisher_pubkey_hex",
            "trust",
        ];
        expected.sort_unstable();
        assert_eq!(
            keys, expected,
            "the App Store reads these exact field names"
        );
        assert_eq!(app["package_id"], PLAIN);
        assert_eq!(app["display_name"], "Sheets");
        assert_eq!(app["version"], "1.4.0");
        assert_eq!(app["registry"], "local");
        assert_eq!(app["trust"], "publisher-signed");
        assert_eq!(
            app["download_url"],
            "https://apps.tilo.so/artifacts/plain-1.4.0.tapp"
        );

        let install = |query: &str| {
            std::process::Command::new(tilo_test_env::package_binary!("app"))
                .args(["install", query])
                .output_locked()
                .unwrap()
        };
        let partial = install("sel-pl");
        assert!(!partial.status.success(), "a partial name resolves nothing");
        assert!(
            String::from_utf8_lossy(&partial.stderr).contains("no app named"),
            "{}",
            String::from_utf8_lossy(&partial.stderr)
        );
        let ambiguous = install("Sheets");
        assert!(!ambiguous.status.success(), "a shared name is ambiguous");
        assert!(
            String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"),
            "{}",
            String::from_utf8_lossy(&ambiguous.stderr)
        );
    }
}
