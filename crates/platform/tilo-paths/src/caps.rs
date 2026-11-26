//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    AppStorage,
    Runtime,
    UserDirs,
    WorkspaceRoot,
    FontRoot,
    NativePaths,
}

pub(crate) const WEB_USER_DIRS: &str = "Browsers grant files through pickers, not folders.";
pub(crate) const WEB_WORKSPACE: &str = "Browsers have no mounted workspace.";
pub(crate) const WEB_FONTS: &str = "The web bundle delivers its fonts as data, not a folder.";
pub(crate) const WEB_NATIVE_PATHS: &str = "Browser storage has no file system paths.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. A host may provide less at run time (no `$XDG_RUNTIME_DIR`, no
    /// workspace mounted), never more.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::AppStorage | Self::Runtime => [Available, Available, Available],
            Self::UserDirs => [Available, Available, Unsupported(WEB_USER_DIRS)],
            Self::WorkspaceRoot => [Available, Available, Unsupported(WEB_WORKSPACE)],
            Self::FontRoot => [Available, Available, Unsupported(WEB_FONTS)],
            Self::NativePaths => [Available, Available, Unsupported(WEB_NATIVE_PATHS)],
        }
    }
}
