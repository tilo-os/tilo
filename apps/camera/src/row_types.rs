//! The types Camera's rows are items of: a row says what it is — its type's
//! noun and the app's name — so search and an agent name it as they name a
//! resource, and it offers the effects its type declares.

use ui::tilo_resource::row::RowBindings;
use ui::tilo_resource::words::RowType;
use ui::tilo_resource::{self, Effect as _};

use crate::actions::{CameraEffect, open_photos_action};

/// The capture pipeline: capturing and recording, recovering from a camera
/// that is not there, and the cameras to choose beneath it. It routes
/// browsing to Photos, which owns the library.
pub(crate) struct Capture;

impl RowType for Capture {
    const KIND: &'static str = "camera.capture";
    const LABEL: &'static str = "Capture";

    fn effects(id: &tilo_resource::Id) -> Vec<ui::tilo_actions::ActionSpec> {
        let mut effects = CameraEffect::row_specs(Self::KIND, id);
        effects.push(open_photos_action());
        effects
    }

    fn effects_with(
        id: &tilo_resource::Id,
        bindings: &RowBindings,
    ) -> Vec<ui::tilo_actions::ActionSpec> {
        let mut effects = CameraEffect::row_specs_with(Self::KIND, id, bindings);
        effects.push(open_photos_action());
        effects
    }
}

/// One camera of this device, offering to open it.
#[derive(tilo_resource::Row)]
#[row(kind = "camera.device", label = "Camera", effects = CameraEffect)]
pub(crate) struct CameraDevice;

/// The code scanner, offering to scan an existing image.
#[derive(tilo_resource::Row)]
#[row(kind = "camera.scanner", label = "Code scanner", effects = CameraEffect)]
pub(crate) struct Scanner;

/// The document scanner's session, offering its page work.
#[derive(tilo_resource::Row)]
#[row(
    kind = "camera.document-scanner",
    label = "Document scanner",
    effects = CameraEffect
)]
pub(crate) struct DocumentScanner;

/// A recent still capture, which offers the handoff to the app that owns its
/// verified bytes.
pub(crate) struct Photo;

impl RowType for Photo {
    const KIND: &'static str = "camera.photo";
    const LABEL: &'static str = "Photo";

    fn effects_with(
        _id: &tilo_resource::Id,
        bindings: &RowBindings,
    ) -> Vec<ui::tilo_actions::ActionSpec> {
        crate::actions::handoff_spec(false, bindings)
            .into_iter()
            .collect()
    }
}

/// A recent video capture, as [`Photo`] is a still one.
pub(crate) struct Video;

impl RowType for Video {
    const KIND: &'static str = "camera.video";
    const LABEL: &'static str = "Video";

    fn effects_with(
        _id: &tilo_resource::Id,
        bindings: &RowBindings,
    ) -> Vec<ui::tilo_actions::ActionSpec> {
        crate::actions::handoff_spec(true, bindings)
            .into_iter()
            .collect()
    }
}
