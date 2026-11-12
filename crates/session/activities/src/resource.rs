//! The layer as a resource: the door its effects are published on, so the
//! person, the palette, an agent and a script press an activity's control or
//! put the activity away through the same actions the cards offer.
//!
//! The layer keeps no document. What it shows is every app's live activity,
//! which the session publishes to it ([`crate::link`]), and its actions are
//! carried out there; nothing of it is recorded, synced or kept in History.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

use crate::link::ActivityService;
use crate::surface::ActivitiesSurface;

/// The floating layer resource: the door its effects are published on. It
/// holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Activities {}

#[resource(kind = "activities.layer")]
impl Activities {}

/// What anyone can do with the activity cards on screen. An activity is
/// named as the card's live row gives it: the app that shows it and its own
/// id. Pressing a control or closing runs what the activity offered, as the
/// person: whoever acts for them is asked first.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.activities")]
pub enum ActivityEffect {
    /// Press control
    #[effect(own_device, attended, icon = "play")]
    Control {
        #[param("The app showing the activity")]
        app: String,
        #[param("The activity, by its id")]
        activity: String,
        #[param("The control, by its id (pause, add-minute, close…)")]
        control: String,
    },
    /// Close activity
    #[effect(own_device, icon = "x")]
    Close {
        #[param("The app showing the activity")]
        app: String,
        #[param("The activity, by its id")]
        activity: String,
    },
    /// Collapse activity card
    #[effect(own_device)]
    Collapse {
        #[param("The app showing the activity")]
        app: String,
        #[param("The activity, by its id")]
        activity: String,
    },
}

/// The layer app: the activity cards over [`Activities`].
pub struct ActivitiesApp;

impl ResourceApp for ActivitiesApp {
    type Resource = Activities;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Activities";
    const ICON: Option<&'static str> = Some("layers");
    // Over the desktop: what is not a card shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    // The cards show this device's activities: they are nobody's workspace
    // participant.
    const ATTACHES_RUNTIME: bool = false;

    fn view(cx: &mut ViewCx<Activities>) -> Element {
        let lent = cx.take_lent::<ActivityService>();
        let surface = ActivitiesSurface::new(lent.map(|service| service.link));
        surface.link().wake_with(cx.waker());
        cx.surface(surface)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::fixture::SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::fixture::viewport(scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_activity_action_is_published_by_its_name() {
        let ids: Vec<&str> = [
            ActivityEffect::Control {
                app: String::new(),
                activity: String::new(),
                control: String::new(),
            },
            ActivityEffect::Close {
                app: String::new(),
                activity: String::new(),
            },
            ActivityEffect::Collapse {
                app: String::new(),
                activity: String::new(),
            },
        ]
        .iter()
        .map(Effect::id)
        .collect();
        assert_eq!(ids, ["control", "close", "collapse"]);
    }
}
