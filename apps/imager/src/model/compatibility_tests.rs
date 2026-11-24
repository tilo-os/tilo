use crate::model as after;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tilo_resource::{Id, Identified, JsonSchema, Model, Path};

fn wire<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn model_contract<T: Model>(value: &T) -> Value {
    let json = value.to_json();
    assert_eq!(T::from_json(&json).unwrap().to_json(), json);
    json!({"value": json, "schema": T::value_schema()})
}

fn serialized_roundtrip<T: Serialize + DeserializeOwned>(value: &T) -> Value {
    let json = wire(value);
    assert_eq!(
        wire(&serde_json::from_value::<T>(json.clone()).unwrap()),
        json
    );
    json
}

/// Every fact the model's wire format, schemas, typed paths, ids and
/// formatting expose, as one JSON value.
macro_rules! snapshot {
    ($model:ident) => {{
        let source = $model::sample_source();
        let profile = $model::InstallProfile::sample_workstation();
        let releases = $model::sample_releases();
        let targets = $model::sample_targets();

        let mut without_digest = source.clone();
        without_digest.expected_sha256 = Some(String::new());
        without_digest.identity = None;
        let mut without_identity = source.clone();
        without_identity.identity = None;

        let mut configured = profile.clone();
        configured.purpose = "server".into();
        configured.wifi_ssid = "Network".into();
        configured.device_label = "New device".into();
        configured.enroll_local_identity = true;

        let mut release = wire(&releases[0]);
        release["url"] = json!("https://example.org/tilo.img.zst");
        release["archive_sha256"] = json!("c".repeat(64));
        release["archive_size"] = json!(42);
        release["image_sha256"] = json!("a".repeat(64));
        let release: $model::TiloRelease = serde_json::from_value(release).unwrap();

        let recent = $model::RecentSource {
            id: Id::from("reviewed"),
            rank: -3,
            source: source.clone(),
        };

        let source_paths = $model::Source::paths_at(Path::<$model::Source, $model::Source>::root());
        let profile_paths = $model::InstallProfile::paths_at(Path::<
            $model::InstallProfile,
            $model::InstallProfile,
        >::root());

        let sizes: Vec<String> = [0u64, 999, 1000, 1_500_000, u64::MAX]
            .into_iter()
            .map($model::fmt_size)
            .collect();
        let rates: Vec<String> = [0u64, 999, 1000, 1_500_000, u64::MAX]
            .into_iter()
            .map($model::fmt_rate)
            .collect();

        let filesystems: Vec<Value> = $model::Filesystem::all()
            .into_iter()
            .map(|filesystem| wire(&filesystem))
            .collect();
        let target_facts: Vec<Value> = targets
            .iter()
            .map(|target| {
                json!({
                    "is_media_target": target.is_media_target(),
                    "partitions": target.partitions.len(),
                })
            })
            .collect();
        let mut absolute = targets[0].clone();
        absolute.name = "/dev/sdb".into();

        json!({
            "serde": {
                "source": serialized_roundtrip(&source),
                "releases": wire(&releases),
                "targets": wire(&targets),
                "profile": serialized_roundtrip(&profile),
                "configured_profile": serialized_roundtrip(&configured),
                "release": serialized_roundtrip(&release),
            },
            "model": {
                "source": source.to_json(),
                "profile": profile.to_json(),
                "configured_profile": configured.to_json(),
                "recent": model_contract(&recent),
            },
            "schema": {
                "source": $model::Source::json_schema(),
                "identity": $model::SourceIdentity::json_schema(),
                "kind": $model::SourceKind::json_schema(),
                "filesystem": $model::Filesystem::json_schema(),
            },
            "model_schema": {
                "source": $model::Source::value_schema(),
                "profile": $model::InstallProfile::value_schema(),
                "user": $model::UserAccount::value_schema(),
                "recent": $model::RecentSource::value_schema(),
            },
            "paths": {
                "source": {
                    "kind": wire(&source_paths.kind.segments()),
                    "path": wire(&source_paths.path.segments()),
                    "name": wire(&source_paths.name.segments()),
                    "size": wire(&source_paths.size.segments()),
                    "label": wire(&source_paths.label.segments()),
                    "installable": wire(&source_paths.installable.segments()),
                    "identity": wire(&source_paths.identity.segments()),
                    "expected_sha256": wire(&source_paths.expected_sha256.segments()),
                },
                "profile": {
                    "purpose": wire(&profile_paths.purpose.segments()),
                    "hostname": wire(&profile_paths.hostname.segments()),
                    "locale": wire(&profile_paths.locale.segments()),
                    "keyboard": wire(&profile_paths.keyboard.segments()),
                    "timezone": wire(&profile_paths.timezone.segments()),
                    "filesystem": wire(&profile_paths.filesystem.segments()),
                    "wifi_ssid": wire(&profile_paths.wifi_ssid.segments()),
                    "device_label": wire(&profile_paths.device_label.segments()),
                    "enroll_local_identity": wire(&profile_paths.enroll_local_identity.segments()),
                    "user": {
                        "full_name": wire(&profile_paths.user.full_name.segments()),
                        "username": wire(&profile_paths.user.username.segments()),
                    },
                },
            },
            "ids": {
                "source": wire(&source.id()),
                "source_without_digest": wire(&without_digest.id()),
                "source_without_identity": wire(&without_identity.id()),
                "recent": wire(&recent.id()),
                "release": wire(&release.selection_id()),
            },
            "release_source": wire(&release.to_source()),
            "formatting": { "size": sizes, "rate": rates },
            "filesystems": filesystems,
            "targets": target_facts,
            "absolute_name_dev_path": wire(&absolute.dev_path()),
            "media_profile": String::from_utf8(configured.to_media_profile_json().unwrap()).unwrap(),
        })
    }};
}

tilo_test_env::scenario! {
    fn the_frozen_model_wire_format_is_unchanged;

    phase frozen_model_json_schema_paths_and_source_ids_are_unchanged;
    {
        let mut current = snapshot!(after);
        let expected: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/model_before.json")).unwrap();
        // The label's target is a stored word ("x86-64") rather than the prose
        // the frozen model wrote: a surface paints "Generic x86-64" from its
        // catalogue. A source is kept twice, and in the release's mapping to a
        // source; the label is the only field that differs, and everything
        // else still matches the frozen model.
        for place in [("serde", "source"), ("model", "source")] {
            assert_eq!(
                current[place.0][place.1]["label"],
                json!("Tilo 1.0 (stable), x86-64")
            );
            current[place.0][place.1]["label"] = expected[place.0][place.1]["label"].clone();
        }
        assert_eq!(
            current["release_source"]["label"],
            json!("Tilo 1.0 (stable), x86-64")
        );
        current["release_source"]["label"] = expected["release_source"]["label"].clone();
        assert_eq!(current, expected);
    }
}
