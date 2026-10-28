//! Zoom in, Zoom out and Actual size as host actions, so the palette,
//! assistive technology and agents reach the window's content zoom
//! (`crate::content_zoom`) through the same door, not only Ctrl+Plus,
//! Ctrl+Minus, Ctrl+0, Ctrl+scroll and a pinch.

use tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource};

use crate::content_zoom::ZoomStep;
use crate::i18n::I18nManager;

pub(crate) const ZOOM_RESOURCE: &str = "tilo.host:zoom";
pub(crate) const ZOOM_OPERATION: &str = "tilo.host.zoom";

/// The window's zoom actions. Each runs one step, as its key does.
#[expect(
    clippy::expect_used,
    reason = "fixed unit zoom payloads serialize infallibly"
)]
pub(crate) fn zoom_resource() -> Resource {
    let action = |step: ZoomStep, id: &str, key: &str| {
        ActionSpec::operation(
            id,
            I18nManager::lookup_plain(key),
            OperationKind::View,
            ZOOM_OPERATION,
            &step,
        )
        .expect("unit zoom operation serializes")
    };
    Resource::new(
        ZOOM_RESOURCE,
        "tilo.host.zoom",
        I18nManager::lookup_plain("host-menu-view"),
    )
    .with_action(action(ZoomStep::In, "zoom-in", "host-zoom-in"))
    .with_action(action(ZoomStep::Out, "zoom-out", "host-zoom-out"))
    .with_action(action(ZoomStep::Reset, "zoom-reset", "host-zoom-reset"))
}

/// The step a zoom operation carries.
pub(crate) fn decode(encoded: &EncodedAction) -> Result<ZoomStep, String> {
    debug_assert_eq!(encoded.type_id, ZOOM_OPERATION);
    serde_json::from_slice(&encoded.payload).map_err(|_| "Invalid zoom operation".to_owned())
}
