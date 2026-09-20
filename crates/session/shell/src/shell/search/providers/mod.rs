//! Built-in [`SearchProvider`] implementations, one per submodule.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod applications;
pub mod continuity;
pub mod desktop_windows;
pub mod editor;
pub mod files;
pub mod host_applications;
pub mod live_app_actions;
pub mod notes;
pub mod records;
pub mod session_actions;
pub mod text_app;

pub use actions::ActionsProvider;
pub use applications::ApplicationsProvider;
pub use continuity::{CallContinuityActionsProvider, DeviceContinuityActionsProvider};
pub use desktop_windows::DesktopWindowsProvider;
pub use editor::EditorProvider;
pub use files::FilesProvider;
pub use host_applications::HostApplicationsProvider;
pub use live_app_actions::LiveAppActionsProvider;
pub use notes::NotesProvider;
pub use records::RecordsProvider;
pub use session_actions::SessionActionsProvider;
