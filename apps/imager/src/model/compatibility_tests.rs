//! The frozen pre-extraction definitions guard the public wire and CRDT contracts.
#[expect(
    dead_code,
    reason = "the frozen fixture retains the entire pre-extraction model API"
)]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the frozen fixture keeps the pre-extraction signatures verbatim"
)]
#[path = "../../tests/fixtures/model_before.rs"]
mod before;

#[path = "frozen_snapshot.rs"]
mod frozen_snapshot;

use super::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use ui::tilo_resource::{Id, JsonSchema, Model, Path};

impl From<&before::TargetDrive> for TargetDrive {
    fn from(value: &before::TargetDrive) -> Self {
        serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
    }
}

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

#[test]
fn frozen_model_json_schema_paths_and_source_ids_are_unchanged() {
    let source = before::Source {
        kind: before::SourceKind::TiloImage,
        path: "/reviewed/tilo.img".into(),
        name: "Tilo release".into(),
        size: 123456789,
        label: "Tilo".into(),
        installable: true,
        identity: Some(before::SourceIdentity {
            volume: 17,
            file: 29,
        }),
        expected_sha256: Some("a".repeat(64)),
    };
    let canonical: Source = serde_json::from_value(wire(&source)).unwrap();
    assert_eq!(serialized_roundtrip(&canonical), wire(&source));
    assert_eq!(model_contract(&canonical), model_contract(&source));
    assert_eq!(Source::json_schema(), before::Source::json_schema());
    assert_eq!(
        SourceIdentity::json_schema(),
        before::SourceIdentity::json_schema()
    );
    assert_eq!(SourceKind::json_schema(), before::SourceKind::json_schema());
    assert_eq!(canonical.id(), source.id());
    for digest in [None, Some(String::new())] {
        let mut old = source.clone();
        old.expected_sha256 = digest;
        let new: Source = serde_json::from_value(wire(&old)).unwrap();
        assert_eq!(new.id(), old.id());
        old.identity = None;
        let new: Source = serde_json::from_value(wire(&old)).unwrap();
        assert_eq!(new.id(), old.id());
    }
    let old_paths = before::Source::paths_at(Path::<before::Source, before::Source>::root());
    let new_paths = Source::paths_at(Path::<Source, Source>::root());
    macro_rules! paths {
        ($old:ident, $new:ident; $($field:ident),+) => {
            $(assert_eq!($old.$field.segments(), $new.$field.segments());)+
        };
    }
    paths!(old_paths, new_paths; kind, path, name, size, label, installable, identity, expected_sha256);
    let recent = before::RecentSource {
        id: Id::from("reviewed"),
        rank: -3,
        source,
    };
    let current = RecentSource {
        id: recent.id.clone(),
        rank: recent.rank,
        source: canonical,
    };
    assert_eq!(model_contract(&current), model_contract(&recent));

    let mut profile = before::InstallProfile::sample_workstation();
    profile.purpose = "server".into();
    profile.wifi_ssid = "Network".into();
    profile.device_label = "New device".into();
    profile.enroll_local_identity = true;
    profile.prepared_setup = Some(before::PreparedSetupExpectation {
        schema: 1,
        sealed_bytes: 1024,
        ciphertext_sha256: "b".repeat(64),
    });
    let canonical: InstallProfile = serde_json::from_value(wire(&profile)).unwrap();
    assert_eq!(serialized_roundtrip(&canonical), wire(&profile));
    assert_eq!(model_contract(&canonical), model_contract(&profile));
    assert_eq!(
        canonical.to_media_profile_json().unwrap(),
        profile.to_media_profile_json().unwrap()
    );
    assert_eq!(
        PreparedSetupExpectation::json_schema(),
        before::PreparedSetupExpectation::json_schema()
    );
    assert_eq!(Filesystem::json_schema(), before::Filesystem::json_schema());
    let old_paths = before::InstallProfile::paths_at(Path::<
        before::InstallProfile,
        before::InstallProfile,
    >::root());
    let new_paths = InstallProfile::paths_at(Path::<InstallProfile, InstallProfile>::root());
    paths!(old_paths, new_paths; purpose, hostname, locale, keyboard, timezone, filesystem, wifi_ssid, device_label, enroll_local_identity, prepared_setup);
    let old_user = old_paths.user;
    let new_user = new_paths.user;
    paths!(old_user, new_user; full_name, username);
    for filesystem in before::Filesystem::all() {
        let old: Value = wire(&filesystem);
        let canonical: Filesystem = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serialized_roundtrip(&canonical), old);
    }
}

#[test]
fn frozen_release_and_drive_wire_fields_are_unchanged() {
    let mut release = wire(&before::sample_releases()[0]);
    release["url"] = json!("https://example.org/tilo.img.zst");
    release["archive_sha256"] = json!("c".repeat(64));
    release["archive_size"] = json!(42);
    release["image_sha256"] = json!("a".repeat(64));
    let old: before::TiloRelease = serde_json::from_value(release).unwrap();
    let new: TiloRelease = serde_json::from_value(wire(&old)).unwrap();
    assert_eq!(serialized_roundtrip(&new), wire(&old));
    assert_eq!(wire(&new.to_source()), wire(&old.to_source()));
    assert_eq!(new.selection_id(), old.selection_id());
    for old in before::sample_targets() {
        let new: TargetDrive = serde_json::from_value(wire(&old)).unwrap();
        assert_eq!(serialized_roundtrip(&new), wire(&old));
        assert_eq!(new.is_media_target(), old.is_media_target());
        assert_eq!(new.dev_path(), old.dev_path());
        assert_eq!(new.contents_summary(), old.contents_summary());
    }
    for value in [0, 999, 1000, 1500000, u64::MAX] {
        assert_eq!(fmt_size(value), before::fmt_size(value));
        assert_eq!(fmt_rate(value), before::fmt_rate(value));
    }
}
