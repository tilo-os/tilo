//! One module per settings category content pane.
//!
//! Each pane exposes `build() -> Box<dyn Widget>` returning that category's
//! body. The pane header (icon + title + subtitle) is owned by
//! `settings_app`, so the modules here focus on the category-specific
//! content.
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
pub mod appearance;
pub mod audio;
pub mod bluetooth;
pub mod device_mesh;
pub mod display;
pub mod display_arrangement;
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

/// Semantic provenance for a pane whose bound controls exclusively edit
/// durable preferences. Callers pass a latch set only while that pane handles
/// an input event, so initial seeding, status refreshes, and passive
/// synchronisation remain unstamped.
///
/// Mixed command/status panes deliberately do not use this helper: accounts,
/// users, network, Bluetooth, audio, displays, printers, storage, and updates
/// combine durable operations with transient form, progress, refresh, or
/// confirmation keys. Notifications already dispatches durable mutations via
/// `SettingsAction`. About and Permissions have no preference mutations.
pub(crate) fn preference_commit_context(
    pending: bool,
    slug: &str,
    label: &str,
    action_label: &str,
) -> Option<ui::tilo_actions::ActionContext> {
    pending.then(|| {
        ui::tilo_actions::ActionContext::for_resource(
            action_label,
            format!("settings.preference:{slug}"),
            "settings.preference-pane",
            label,
        )
        .with_app("so.tilo.settings", "Settings")
    })
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

/// The "Sound" category renders the audio pane.
pub mod sound {
    pub use super::audio::build;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_context_exists_only_for_a_pending_user_edit() {
        assert!(
            preference_commit_context(false, "power", "Power", "Change power preferences")
                .is_none(),
            "seeding and passive refreshes stay out of semantic history"
        );
        let context = preference_commit_context(true, "power", "Power", "Change power preferences")
            .expect("pending preference context");
        assert_eq!(context.action_label, "Change power preferences");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("settings.preference:power")
        );
        assert_eq!(
            context.app.as_ref().map(|app| app.id.as_str()),
            Some("so.tilo.settings")
        );
    }

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
