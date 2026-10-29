//! One module per settings category content pane.
//!
//! Every pane is view code over the settings sections it binds and the
//! area it shows ([`view`]); [`PART`] brings the areas. The pane's
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

mod part;
pub use part::PART;

/// Every part of Settings: its own and the panes'.
pub const PARTS: &[&crate::area::Part] = &[&crate::effects::PART, &PART];

/// `category`'s pane as view code over the settings sections it binds
/// (`cx.record(&section, ..)`) and the area it shows; `None` where the
/// window holds no such area.
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
        crate::categories::Category::Storage => storage::view(cx, app),
        crate::categories::Category::Updates => updates::view(cx, app),
        crate::categories::Category::Permissions => permissions::view(cx, app),
        crate::categories::Category::Displays => display::view(cx, app),
        crate::categories::Category::DefaultApps => personal_apps::view(cx, app),
        crate::categories::Category::TimeDate => time_date::view(cx, app),
        crate::categories::Category::DeviceMesh => device_mesh::view(cx, app),
        crate::categories::Category::About => about::view(cx, app),
        crate::categories::Category::ExternalAccounts => accounts::view(cx, app),
        crate::categories::Category::AgentModels => agent_models::view(cx, app),
    }
}

/// Why this process cannot tell which programs the device has installed,
/// when it cannot: no directory on `PATH` is visible to it. A lookup would
/// then report every program as missing, which says nothing about the device.
pub(crate) fn installed_programs_unreadable() -> Option<String> {
    programs_unreadable_on(std::env::var_os("PATH").as_deref())
}

fn programs_unreadable_on(path: Option<&std::ffi::OsStr>) -> Option<String> {
    let Some(path) = path else {
        return Some("PATH is not set".to_string());
    };
    if std::env::split_paths(path).any(|dir| dir.is_dir()) {
        None
    } else {
        Some(format!(
            "none of {} is visible to Settings",
            path.to_string_lossy()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `PATH` none of whose directories is visible cannot say a program is
    /// missing; one visible directory can.
    #[test]
    fn programs_are_unreadable_only_when_no_path_directory_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join("usr-bin");
        let unreadable = programs_unreadable_on(Some(hidden.as_os_str()));
        assert!(
            unreadable
                .as_deref()
                .is_some_and(|error| error.ends_with("is visible to Settings")),
            "{unreadable:?}"
        );
        let mixed = std::env::join_paths([hidden.as_path(), dir.path()]).unwrap();
        assert_eq!(programs_unreadable_on(Some(&mixed)), None);
        assert_eq!(
            programs_unreadable_on(None).as_deref(),
            Some("PATH is not set")
        );
    }
}
