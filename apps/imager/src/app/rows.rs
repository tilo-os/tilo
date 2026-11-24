//! The removable drives an image may be written to, as rows of the declared
//! `imager.target` type.

use tilo_resource::RowBindings;
use tilo_resource::{EffectStatus, LiveRow};

use crate::app::ImagerApp;
use crate::model::TargetDrive;
use crate::resource::{ImagerEffect, TargetDriveRow};

/// The row id a target drive is published under.
pub(crate) fn target_id(target: &TargetDrive) -> String {
    let identity = target.stable_id.as_deref().unwrap_or(&target.name);
    format!("imager.target:{identity}")
}

/// One removable drive, offering to choose it and to write to it, with the
/// drive's kernel name and device path bound.
pub(crate) fn target_row(target: &TargetDrive) -> LiveRow {
    let device = target.dev_path();
    let bindings = RowBindings::new()
        .bind(
            ImagerEffect::CHOOSE_DRIVE,
            [("drive", serde_json::Value::from(target.name.clone()))],
        )
        .bind(
            ImagerEffect::WRITE,
            [
                ("device", serde_json::Value::from(device.clone())),
                ("confirmation", serde_json::Value::from(target.name.clone())),
            ],
        );
    LiveRow::typed_with::<TargetDriveRow>(target_id(target), device, &bindings)
}

/// What each target row's write cannot do now, from the writer's own state.
pub(crate) fn target_row_statuses(app: &ImagerApp) -> Vec<(String, Vec<EffectStatus>)> {
    let Err((code, reason)) = app.writer_refusal() else {
        return Vec::new();
    };
    app.targets
        .iter()
        .filter(|target| target.is_media_target())
        .map(|target| {
            (
                target_id(target),
                vec![EffectStatus::unavailable(
                    ImagerEffect::WRITE,
                    code,
                    reason.clone(),
                )],
            )
        })
        .collect()
}
