use super::{before, serialized_roundtrip, wire};
use crate::model as after;
use serde_json::json;
use ui::tilo_resource::{JsonSchema, Model};

#[test]
fn frozen_json_snapshot() {
    macro_rules! snapshot {
        ($model:ident) => { json!({
        "serde": {
            "source": serialized_roundtrip(&$model::sample_source()),
            "releases": wire(&$model::sample_releases()),
            "targets": wire(&$model::sample_targets()),
            "profile": serialized_roundtrip(&$model::InstallProfile::sample_workstation()),
        },
        "model": {
            "source": $model::sample_source().to_json(),
            "profile": $model::InstallProfile::sample_workstation().to_json(),
        },
        "schema": {
            "source": $model::Source::json_schema(),
            "identity": $model::SourceIdentity::json_schema(),
            "kind": $model::SourceKind::json_schema(),
            "filesystem": $model::Filesystem::json_schema(),
            "prepared_setup": $model::PreparedSetupExpectation::json_schema(),
        },
        "model_schema": {
            "source": $model::Source::value_schema(),
            "profile": $model::InstallProfile::value_schema(),
            "user": $model::UserAccount::value_schema(),
            "recent": $model::RecentSource::value_schema(),
        },
    }) };
    }
    let frozen = snapshot!(before);
    let mut current = snapshot!(after);
    // The label's target is a stored word ("x86-64") rather than the prose the
    // frozen model wrote: a surface paints "Generic x86-64" from its
    // catalogue. A source is kept twice in the snapshot and its label is the
    // only field that differs; everything else still matches the frozen model.
    for place in ["serde", "model"] {
        assert_eq!(
            current[place]["source"]["label"],
            json!("Tilo 1.0 (stable), x86-64")
        );
        current[place]["source"]["label"] = frozen[place]["source"]["label"].clone();
    }
    assert_eq!(current, frozen);
    println!("IMAGER_MODEL_BEFORE={frozen}");
}
