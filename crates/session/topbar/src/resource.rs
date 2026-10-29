//! The bar as a resource: the door its effects are published on, so the
//! person, the palette, an agent and a script switch workspaces, open quick
//! settings, the notifications, the people here or their own card, press a
//! running application's tray item or pause what plays through the same
//! actions.
//!
//! The bar keeps no document. What it shows is the desktop's, lent to it
//! ([`crate::link`]), and published as its live rows; nothing of it is
//! recorded, synced or kept in History.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

use crate::link::BarService;
use crate::surface::TopBarSurface;

/// The bar resource: the door its effects and rows are published on. It
/// holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct TopBar {}

#[resource(kind = "desktop.topbar", label = "Top bar")]
impl TopBar {}

/// What anyone can do from the desktop's top bar.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.topbar")]
pub enum TopBarEffect {
    /// Switch to workspace
    #[effect(own_device, icon = "layout-grid")]
    SwitchWorkspace {
        #[param("The workspace, by its name or its place counting from 1")]
        workspace: String,
    },
    /// Next workspace
    #[effect(own_device)]
    NextWorkspace,
    /// Previous workspace
    #[effect(own_device)]
    PreviousWorkspace,
    /// Show strips
    #[effect(view, own_device)]
    ShowStrips,
    /// Overview
    #[effect(view, own_device)]
    Overview,
    /// Open quick settings
    #[effect(view, own_device, icon = "settings")]
    QuickSettings,
    /// Open notifications
    #[effect(view, own_device, icon = "bell")]
    Notifications,
    /// Show who is here
    #[effect(view, own_device, icon = "users")]
    People,
    /// Open History
    #[effect(view, own_device)]
    Activity,
    /// Invite someone here
    #[effect(own_device, attended, icon = "user-plus")]
    Invite,
    /// Open your profile card
    #[effect(view, own_device)]
    Profile,
    /// Open the workspace menu
    #[effect(view, own_device)]
    WorkspaceMenu,
    /// Rename workspace
    #[effect(own_device)]
    RenameWorkspace {
        #[param("The workspace's new name")]
        name: String,
    },
    /// Press tray item
    #[effect(own_device, attended)]
    ActivateTrayItem {
        #[param("The running application's tray item, by its name")]
        item: String,
    },
    /// Play or pause
    #[effect(own_device, icon = "play")]
    PlayPause,
    /// Next track
    #[effect(own_device)]
    NextTrack,
    /// Previous track
    #[effect(own_device)]
    PreviousTrack,
    /// Continue here what another device offers
    #[effect(own_device)]
    Continue,
    /// Focus the top bar
    #[effect(view, own_device)]
    Focus,
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

    #[test]
    fn every_bar_action_is_published_by_its_name() {
        let ids: Vec<&str> = [
            TopBarEffect::SwitchWorkspace {
                workspace: String::new(),
            },
            TopBarEffect::QuickSettings,
            TopBarEffect::Notifications,
            TopBarEffect::People,
            TopBarEffect::Profile,
            TopBarEffect::ActivateTrayItem {
                item: String::new(),
            },
            TopBarEffect::PlayPause,
            TopBarEffect::NextTrack,
            TopBarEffect::Focus,
        ]
        .iter()
        .map(Effect::id)
        .collect();
        assert_eq!(
            ids,
            [
                "switch-workspace",
                "quick-settings",
                "notifications",
                "people",
                "profile",
                "activate-tray-item",
                "play-pause",
                "next-track",
                "focus",
            ]
        );
    }
}
