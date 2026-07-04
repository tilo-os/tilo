//! Sidebar shortcuts builder — returns the tiered structure of labelled
//! destinations matching the `nirios-concept` FilesApp sidebar: a
//! **Favorites** group (user xdg-dirs) and a **System** group (root).
//!
//! Replaces the flat list that originally mirrored the iced
//! `render_sidebar` (AUDIT.md S1) so the quick-ui file-explorer renders
//! the same Favorites/System tier chrome as the TypeScript mockup.

use std::path::{Path, PathBuf};

/// Fixed sidebar width in logical pixels, mirroring the original iced app
/// (AUDIT.md S3).
pub const SIDEBAR_WIDTH_PX: f32 = 180.0;

/// One tier in the sidebar. The `title` is rendered as an uppercase-ish
/// 10 px label above the tier; `entries` are the clickable shortcut
/// rows within the tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarTier {
    pub title: &'static str,
    pub entries: Vec<(String, PathBuf)>,
}

/// Returns the sidebar structured as Favorites + System tiers, matching
/// `nirios-concept/src/components/apps/FilesApp.tsx` (lines 94/127).
///
/// - **Favorites**: Home, Documents, Downloads, Pictures, Music, Videos
///   (populated from `$HOME` when available).
/// - **System**: `/` (OS disk).
///
/// Present even when `home` is `None` so the UI always renders at least
/// the System tier.
pub fn build_tiers(home: Option<&Path>) -> Vec<SidebarTier> {
    let mut tiers = Vec::new();
    if let Some(home) = home {
        tiers.push(SidebarTier {
            title: "Favorites",
            entries: vec![
                ("󰋜 Home".into(), home.to_path_buf()),
                ("󰉋 Documents".into(), home.join("Documents")),
                ("󰉋 Downloads".into(), home.join("Downloads")),
                ("󰉋 Pictures".into(), home.join("Pictures")),
                ("󰉋 Music".into(), home.join("Music")),
                ("󰉋 Videos".into(), home.join("Videos")),
            ],
        });
    }
    tiers.push(SidebarTier {
        title: "System",
        entries: vec![("󰋊 Root".into(), PathBuf::from("/"))],
    });
    tiers
}

/// Flat shortcut list — Favorites' entries followed by System's. Kept as a
/// convenience for callers that still need a linear index (e.g.
/// hit-testing against `files.sidebar.<N>` ids).
pub fn build_shortcuts(home: Option<&Path>) -> Vec<(String, PathBuf)> {
    build_tiers(home)
        .into_iter()
        .flat_map(|t| t.entries.into_iter())
        .collect()
}

/// Returns the index of the shortcut whose path equals `current`, if any.
/// Used by list rendering to mark the active shortcut (AUDIT.md S2).
pub fn active_shortcut_index(
    shortcuts: &[(String, PathBuf)],
    current: &Path,
) -> Option<usize> {
    shortcuts.iter().position(|(_, p)| p == current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tiers_has_two_tiers_when_home_is_set() {
        let tiers = build_tiers(Some(Path::new("/home/user")));
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].title, "Favorites");
        assert_eq!(tiers[1].title, "System");
    }

    #[test]
    fn build_tiers_drops_favorites_when_home_is_none() {
        // Without a home path there are no xdg-user-dirs to list, so
        // the Favorites tier is omitted entirely; System stays.
        let tiers = build_tiers(None);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].title, "System");
    }

    #[test]
    fn favorites_order_matches_the_concept() {
        let tiers = build_tiers(Some(Path::new("/home/user")));
        let labels: Vec<&str> = tiers[0]
            .entries
            .iter()
            .map(|(l, _)| l.as_str())
            .collect();
        // The concept lists Home first followed by the xdg-user-dirs in
        // Documents→Videos order; keep the parity strict so a visual
        // diff against the mockup stays aligned.
        assert_eq!(
            labels,
            vec![
                "󰋜 Home",
                "󰉋 Documents",
                "󰉋 Downloads",
                "󰉋 Pictures",
                "󰉋 Music",
                "󰉋 Videos",
            ]
        );
    }

    #[test]
    fn system_tier_has_root_as_os_disk() {
        let tiers = build_tiers(None);
        let sys = &tiers[0];
        assert_eq!(sys.title, "System");
        assert_eq!(sys.entries.len(), 1);
        assert_eq!(sys.entries[0].1, PathBuf::from("/"));
    }

    #[test]
    fn build_shortcuts_is_the_flat_concatenation_of_tier_entries() {
        let tiers = build_tiers(Some(Path::new("/home/user")));
        let flat = build_shortcuts(Some(Path::new("/home/user")));
        let expected_len: usize = tiers.iter().map(|t| t.entries.len()).sum();
        assert_eq!(flat.len(), expected_len);
        // First entry must come from the first tier, last from the last.
        assert_eq!(flat.first().unwrap().0, tiers[0].entries[0].0);
        assert_eq!(
            flat.last().unwrap().0,
            tiers.last().unwrap().entries.last().unwrap().0
        );
    }
}
