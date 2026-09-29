//! What this crate provides on each target, as data an app can query.

use std::fmt;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Durable per-app storage: [`AppPaths::data`](crate::AppPaths::data),
    /// `config`, `cache` and `state`.
    AppStorage,
    /// Scratch space for one session: [`AppPaths::runtime`](crate::AppPaths::runtime).
    Runtime,
    /// The user's content folders: [`AppPaths::user_dir`](crate::AppPaths::user_dir).
    UserDirs,
    /// The mounted collaborative workspace:
    /// [`AppPaths::workspace_root`](crate::AppPaths::workspace_root).
    WorkspaceRoot,
    /// A directory holding the font catalog:
    /// [`AppPaths::font_root`](crate::AppPaths::font_root).
    FontRoot,
    /// Locations that are operating-system paths:
    /// [`Location::native_path`](crate::Location::native_path).
    NativePaths,
}

/// Whether a [`Feature`] can be used on a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Never on this target; the reason is user-facing.
    Unsupported(&'static str),
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported {
    pub feature: Feature,
    /// User-facing.
    pub reason: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for Unsupported {}

pub(crate) const WEB_USER_DIRS: &str = "Browsers grant files through pickers, not folders.";
pub(crate) const WEB_WORKSPACE: &str = "Browsers have no mounted workspace.";
pub(crate) const WEB_FONTS: &str = "The web bundle delivers its fonts as data, not a folder.";
pub(crate) const WEB_NATIVE_PATHS: &str = "Browser storage has no file system paths.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. A host may provide less at run time (no `$XDG_RUNTIME_DIR`, no
/// workspace mounted), never more.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::AppStorage, [Available, Available, Available]),
        (Feature::Runtime, [Available, Available, Available]),
        (
            Feature::UserDirs,
            [Available, Available, Unsupported(WEB_USER_DIRS)],
        ),
        (
            Feature::WorkspaceRoot,
            [Available, Available, Unsupported(WEB_WORKSPACE)],
        ),
        (
            Feature::FontRoot,
            [Available, Available, Unsupported(WEB_FONTS)],
        ),
        (
            Feature::NativePaths,
            [Available, Available, Unsupported(WEB_NATIVE_PATHS)],
        ),
    ]
};

/// This target's column of [`CAPABILITIES`]. Native targets other than
/// macOS report the Linux column.
const COLUMN: usize = if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    CAPABILITIES
        .iter()
        .find(|(listed, _)| *listed == feature)
        .and_then(|(_, targets)| targets.get(COLUMN).copied())
        .unwrap_or(Availability::Unsupported(
            "This build does not know the feature.",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_is_listed_once() {
        for feature in [
            Feature::AppStorage,
            Feature::Runtime,
            Feature::UserDirs,
            Feature::WorkspaceRoot,
            Feature::FontRoot,
            Feature::NativePaths,
        ] {
            let rows = CAPABILITIES.iter().filter(|(listed, _)| *listed == feature);
            assert_eq!(rows.count(), 1, "{feature:?}");
        }
    }

    #[test]
    fn this_target_reads_its_own_column() {
        assert_eq!(availability(Feature::AppStorage), Availability::Available);
        let web = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::UserDirs)
            .map(|(_, targets)| targets[2]);
        assert_eq!(web, Some(Availability::Unsupported(WEB_USER_DIRS)));
        assert_eq!(availability(Feature::UserDirs), Availability::Available);
    }
}
