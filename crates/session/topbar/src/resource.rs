//! The bar as a resource: the door its own verbs are published on — invite
//! someone, open your profile card, press a running application's tray
//! item, continue what another device offers, take the keyboard. What the
//! bar shows of the desktop is done with the verbs the desk, the chrome and
//! the media resource publish, not with a copy of them here.
//!
//! The bar keeps no document. What it shows is the desktop's, lent to it
//! ([`crate::link`]), and published as its live rows; nothing of it is
//! recorded, synced or kept in History.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

use crate::link::BarService;
use crate::surface::TopBarSurface;

/// The bar resource: the door its own verbs and rows are published on. It
/// holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct TopBar {}

#[resource(kind = "desktop.topbar")]
impl TopBar {}

/// What only the bar can do: press a running application's tray item, open
/// the profile card, invite someone, continue what another device offers,
/// or take the keyboard.
///
/// The bar shows the desktop's own verbs — switching workspaces, the
/// overview, quick settings, the notification centre, who is here, History,
/// the workspace menu and naming it, the strips — and the media transport;
/// those are published, with their grants and their history, by the
/// resources that carry them out ([`ChromeEffect`](tilo_desktop_surface::navigation::ChromeEffect)
/// and the media resource), so the bar names them where they live.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.bar")]
pub enum BarEffect {
    /// Invite someone here
    #[effect(own_device, attended, icon = "user-plus")]
    Invite,
    /// Open your profile card
    #[effect(view, own_device)]
    Profile,
    /// Press tray item
    #[effect(own_device, attended)]
    ActivateTrayItem {
        #[param("The running application's tray item, by its name")]
        item: String,
    },
    /// Continue here what another device offers
    #[effect(own_device)]
    Continue,
    /// Focus the top bar
    #[effect(view, own_device)]
    FocusBar,
}

/// The screenshot scenarios ([`crate::fixture`]).
pub(crate) const SCENARIOS: &[&str] = &["bar", "menu", "profile", "focused", "displays", "narrow"];

/// The bar app: the bar surface over [`TopBar`].
pub struct TopBarApp;

impl ResourceApp for TopBarApp {
    type Resource = TopBar;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Top bar";
    const ICON: Option<&'static str> = Some("layout-grid");
    // Over the desktop: what is not the bar shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    // The bar shows this device's desktop: it is nobody's workspace
    // participant.
    const ATTACHES_RUNTIME: bool = false;

    fn view(cx: &mut ViewCx<TopBar>) -> Element {
        let lent = cx.take_lent::<BarService>();
        let surface = TopBarSurface::new(lent.map(|service| service.link));
        surface.link().wake_with(cx.waker());
        cx.surface(surface)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::fixture::viewport(scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop's verbs are published where they are carried out; the
    /// bar names none of them a second time.
    #[test]
    fn the_bar_names_no_desktop_verb() {
        let desktop: std::collections::BTreeSet<&str> =
            tilo_desktop_surface::navigation::ChromeEffect::declarations()
                .into_iter()
                .chain(tilo_desktop_surface::navigation::DeskEffect::declarations())
                .map(|decl| decl.id)
                .collect();
        for decl in BarEffect::declarations() {
            assert!(
                !desktop.contains(&decl.id),
                "{} is the desktop's own verb",
                decl.id
            );
        }
    }

    #[test]
    fn every_bar_action_is_published_by_its_name() {
        let ids: Vec<&str> = [
            BarEffect::Invite,
            BarEffect::Profile,
            BarEffect::ActivateTrayItem {
                item: String::new(),
            },
            BarEffect::Continue,
            BarEffect::FocusBar,
        ]
        .iter()
        .map(Effect::id)
        .collect();
        assert_eq!(
            ids,
            [
                "invite",
                "profile",
                "activate-tray-item",
                "continue",
                "focus-bar",
            ]
        );
    }
}
