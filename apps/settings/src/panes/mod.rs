//! One module per settings category content pane.
//!
//! A pane bound to its settings sections is view code over them
//! ([`view`]); the others are built by their area ([`PART`]). The pane's
//! title is the window's ([`crate::window`]), so the modules here focus on
//! the category-specific content.
//!
//! Panes fall into two groups:
//! - **Hardware panes** (network, bluetooth, sound, displays) own an
//!   `os_actions::Dispatcher` and drive real `nmcli`/`wpctl`/`brightnessctl`/
//!   `bluetoothctl` bindings — the same ones the compositor's quick-settings
//!   panel uses.
//! - **Information / editor panes** (accounts, region, users, printers,
//!   time & date, mouse & keyboard, permissions, updates, about) render
//!   real system state where it's cheaply available, and otherwise a
//!   populated, realistic sample so the surface reads as designed. Each
//!   pane's docstring states which parts are live vs. sample.

pub mod about;
pub mod accessibility;
pub mod accounts;
pub mod agent_models;
pub mod agent_sign_in;
pub mod agent_vertex;
pub mod appearance;
pub mod audio;
pub mod bluetooth;
pub mod device_mesh;
pub mod disk_unlock;
pub mod display;
pub mod display_arrangement;
pub mod drives;
pub mod keyboard_shortcuts;
pub mod mouse_keyboard;
pub mod network;
pub mod notifications;
pub mod permissions;
pub mod personal_apps;
pub mod power;
pub mod printers;
pub mod region;
pub mod speakers;
pub mod storage;
pub mod time_date;
pub mod updates;
pub mod users;

pub use crate::pane_model as model;
pub(crate) use crate::pane_model::{composed_pane, installed_programs_unreadable};

mod part;
pub use part::PART;

/// Every part of Settings: its own and the panes'.
pub const PARTS: &[&crate::area::Part] = &[&crate::effects::PART, &PART];

/// `category`'s pane as view code over the settings sections it binds
/// (`cx.record(&section, ..)`), for the panes built that way; `None` for a
/// pane its area builds ([`crate::area::PaneEntry`]).
pub(crate) fn view(
    cx: &ui::framework::ViewCx<crate::resource::Settings>,
    app: &std::rc::Rc<crate::settings_app::SettingsApp>,
    category: crate::categories::Category,
) -> Option<ui::widget::Element> {
    match category {
        crate::categories::Category::Appearance => appearance::view(cx, app),
        crate::categories::Category::Accessibility => accessibility::view(cx, app),
        crate::categories::Category::Notifications => notifications::view(cx, app),
        crate::categories::Category::Power => power::view(cx, app),
        crate::categories::Category::RegionFormats => region::view(cx, app),
        crate::categories::Category::KeyboardShortcuts => keyboard_shortcuts::view(cx, app),
        crate::categories::Category::Sound => audio::view(cx, app),
        crate::categories::Category::Speakers => speakers::view(cx, app),
        crate::categories::Category::Bluetooth => bluetooth::view(cx, app),
        crate::categories::Category::Network => network::view(cx, app),
        crate::categories::Category::Keyboard => {
            mouse_keyboard::view(cx, app, mouse_keyboard::InputPage::Keyboard)
        }
        crate::categories::Category::Pointing => {
            mouse_keyboard::view(cx, app, mouse_keyboard::InputPage::Pointing)
        }
        crate::categories::Category::Spelling => {
            mouse_keyboard::view(cx, app, mouse_keyboard::InputPage::Spelling)
        }
        crate::categories::Category::Users => users::view(cx, app),
        crate::categories::Category::Printers => printers::view(cx, app),
        _ => None,
    }
}
