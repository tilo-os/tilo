//! Built-in [`SearchProvider`] implementations, one per submodule.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod applications;
pub(crate) mod authentication_prompts;
mod background_index;
pub mod continuity;
pub mod desktop_windows;
pub mod file_picker;
pub mod files;
pub mod host_applications;
pub mod live_app_actions;
pub mod now_playing;
pub mod people;
pub mod permission_prompts;
pub mod quick_answers;
pub mod records;
pub mod session_actions;
pub mod settings_pages;
pub mod shortcuts;
pub mod system_settings;
pub(crate) mod task_review;
pub mod terminal_device_shell;
pub mod tray_items;
pub mod unavailable_applications;

pub use actions::ActionsProvider;
pub use applications::{ApplicationsProvider, NativeApplicationsProvider};
pub use continuity::{
    CallContinuityActionsProvider, ContinueFromDeviceProvider, DeviceContinuityActionsProvider,
};
pub use desktop_windows::DesktopWindowsProvider;
pub use files::FilesProvider;
pub use host_applications::HostApplicationsProvider;
pub use live_app_actions::LiveAppActionsProvider;
pub use people::PeopleProvider;
pub use quick_answers::QuickAnswersProvider;
pub use records::RecordsProvider;
pub use session_actions::{SessionActionPolicy, SessionActionsProvider};
pub use settings_pages::SettingsPagesProvider;
pub use shortcuts::ShortcutsProvider;
pub use system_settings::SystemSettingsProvider;
pub use unavailable_applications::UnavailableApplicationsProvider;
