//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

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

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
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
