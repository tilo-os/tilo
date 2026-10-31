//! The capture studio as an app: the bar that takes a screenshot, records
//! the screen or keeps the last half minute, and the actions anyone can
//! take on it.
//!
//! The studio keeps no document: what it shows is the desktop's windows and
//! what came of the captures it asked for, lent to it ([`crate::link`]), and
//! what the person did reaches the session as its asks and as these
//! published actions.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

use crate::link::Studio;
use crate::surface::StudioSurface;

/// The studio resource: the door its actions are published on, so a
/// keyboard shortcut, the palette, an agent and a script take a screenshot
/// or start a recording through the same actions the bar's own controls
/// use. It holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Capture {}

#[resource(kind = "desktop.capture-studio", label = "Capture studio")]
impl Capture {}

/// What anyone can do with the capture studio. Captures run on this device:
/// what they make is a file here, and nothing of what they show leaves it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.capture-studio")]
pub enum CaptureEffect {
    /// Open capture studio
    #[effect(own_device, attended, icon = "camera")]
    Open,
    /// Take a screenshot
    #[effect(own_device, attended, icon = "camera")]
    Screenshot {
        /// What is captured: `display`, `window` or `region`
        /// (the last selection made in the studio).
        source: Option<String>,
        /// The window, by the identifier the compositor lists it under
        /// (the focused one when left out).
        window: Option<String>,
    },
    /// Record the screen
    #[effect(own_device, attended, icon = "circle-dot")]
    Record {
        /// What is recorded: `display`, `window` or `region`
        /// (the last selection made in the studio).
        source: Option<String>,
        /// The window, by the identifier the compositor lists it under
        /// (the focused one when left out).
        window: Option<String>,
    },
    /// Stop recording
    #[effect(own_device, icon = "square")]
    StopRecording,
    /// Save the last half minute
    #[effect(own_device, attended, icon = "history")]
    SaveReplay,
}

/// The studio app: the capture bar over [`Capture`].
pub struct CaptureStudioApp;

impl ResourceApp for CaptureStudioApp {
    type Resource = Capture;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Capture studio";
    const ICON: Option<&'static str> = Some("camera");
    // Over the desktop: what is not the bar shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    // The studio shows this device's desktop: it is nobody's workspace
    // participant and opens before anyone is chosen.
    const ATTACHES_RUNTIME: bool = false;
    const OPENS_WITHOUT_IDENTITY: bool = true;

    fn view(cx: &mut ViewCx<Capture>) -> Element {
        let seat = cx.take_lent::<Studio>();
        let surface = StudioSurface::new(seat);
        surface.studio().link().wake_with(cx.waker());
        cx.surface(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_capture_action_is_published_by_its_name() {
        let ids: Vec<&str> = [
            CaptureEffect::Open,
            CaptureEffect::Screenshot {
                source: None,
                window: None,
            },
            CaptureEffect::Record {
                source: None,
                window: None,
            },
            CaptureEffect::StopRecording,
            CaptureEffect::SaveReplay,
        ]
        .iter()
        .map(Effect::id)
        .collect();
        assert_eq!(
            ids,
            [
                "open",
                "screenshot",
                "record",
                "stop-recording",
                "save-replay"
            ]
        );
    }
}
