//! The power menu ([`tilo_sheets::power`]) as the shell hosts it: what this
//! session offers goes to the app before it opens, and what the person
//! chose comes back to the one session executor the typed `session.*`
//! actions also reach.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use tilo_sheets::power::{Offers, PowerEffect, PowerSeat};

use super::sheet_apps::{SheetHost, mount_sheet};

impl Default for SheetHost<PowerSeat> {
    fn default() -> Self {
        Self::new(
            LayerId::PowerMenu,
            LayerId::PowerMenu.surface_kind(),
            tilo_sheets::power::definition,
        )
    }
}

/// The session action a power menu choice is.
const fn session_action(effect: PowerEffect) -> os_actions::SessionAction {
    match effect {
        PowerEffect::Lock => os_actions::SessionAction::Lock,
        PowerEffect::LogOut => os_actions::SessionAction::Logout,
        PowerEffect::SwitchUser => os_actions::SessionAction::SwitchUser,
        PowerEffect::Suspend => os_actions::SessionAction::Suspend,
        PowerEffect::Restart => os_actions::SessionAction::Restart,
        PowerEffect::ShutDown => os_actions::SessionAction::ShutDown,
    }
}

impl ShellOverlayHost {
    /// Put the power menu up, offering what this session may do.
    pub(super) fn open_power_menu(&mut self) {
        self.mount_power_menu();
        let policy = &self.session_action_policy;
        self.layers.power_menu.seat().set_offers(Offers {
            user_switching: policy.offers(os_actions::SessionAction::SwitchUser),
            host_power: policy.offers(os_actions::SessionAction::ShutDown),
            // The lock screen is the compositor's own, and it says on its
            // face when no password can be checked here.
            locking: true,
            hangs_from: f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX),
        });
        self.layers.power_menu.open();
        self.bump_chrome();
    }

    /// Mount the menu's app in the room the person works in, as an app the
    /// session launched, so its actions are published there.
    pub(super) fn mount_power_menu(&mut self) {
        let room = self.sheet_room();
        let apps = self.sheet_apps();
        self.layers
            .power_menu
            .mount(&room, |definition, room, instance, seat| {
                mount_sheet(apps.as_ref(), definition, room, instance, seat)
            });
    }

    /// Carry out what the person chose in the power menu, or an agent
    /// asked of it; whether anything changed on screen.
    pub(super) fn pump_power_menu(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let shown = self.layers.power_menu.update(doc, path);
        let asked = self.layers.power_menu.seat().take_asked();
        let acted = !asked.is_empty();
        for effect in asked {
            self.execute_session_action(session_action(effect));
        }
        shown | acted
    }

    /// Whether the power menu is up, for `state_dump` and tests.
    pub fn power_menu_open(&self) -> bool {
        self.layers.power_menu.requested()
    }

    /// Where each row of the open menu is, by the name scripts press it by.
    pub fn power_menu_semantic_rects(&self) -> Vec<(&'static str, Rect)> {
        let area = self.layers.power_menu.area();
        self.layers
            .power_menu
            .seat()
            .rows()
            .into_iter()
            .map(|(effect, rect)| {
                let placed = vello::kurbo::Affine::translate(area.origin().to_vec2());
                (effect.target(), placed.transform_rect_bbox(rect))
            })
            .collect()
    }

    /// Where the open menu's panel stands, its title and rows inside it.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Rect ± Vec2 is float geometry translation and cannot panic"
    )]
    pub fn power_menu_panel_rect(&self) -> Rect {
        let panel = self.layers.power_menu.seat().panel();
        if panel.is_zero_area() {
            return Rect::ZERO;
        }
        vello::kurbo::Affine::translate(self.layers.power_menu.area().origin().to_vec2())
            .transform_rect_bbox(panel)
    }

    /// Open the power menu for a capture of the desktop with it up.
    pub fn force_power_menu_for_visual_test(&mut self) {
        self.open_power_menu();
    }
}
