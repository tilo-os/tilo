//! Enumerates the top-level settings categories shown in the left sidebar.
//!
//! Each `Category` carries a glyph (Nerd Font icon, matching the style the
//! file-explorer sidebar uses) plus a user-facing label. The order here is
//! the order the sidebar renders.

/// One entry in the settings sidebar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Display,
    Audio,
    Network,
    Bluetooth,
    Workspaces,
    Collaboration,
    About,
}

impl Category {
    /// Canonical ordering used by the sidebar. About is intentionally last
    /// to match the convention "system info trailing the configurable
    /// surfaces", consistent with the file-explorer sidebar's Favorites →
    /// System layering.
    pub const ALL: &'static [Category] = &[
        Category::Display,
        Category::Audio,
        Category::Network,
        Category::Bluetooth,
        Category::Workspaces,
        Category::Collaboration,
        Category::About,
    ];

    /// User-facing label rendered in the sidebar and reused as the pane
    /// header. Plain English for now — i18n keys will be threaded through
    /// once the rest of the chrome moves to fluent (parity with the other
    /// native apps).
    pub fn label(self) -> &'static str {
        match self {
            Category::Display => "Display",
            Category::Audio => "Audio",
            Category::Network => "Network",
            Category::Bluetooth => "Bluetooth",
            Category::Workspaces => "Workspaces",
            Category::Collaboration => "Collaboration",
            Category::About => "About",
        }
    }

    /// Nerd Font glyph rendered before the label. Picked to match the
    /// visual language of the file-explorer sidebar (single-codepoint
    /// glyphs from the Nerd Font private-use area).
    pub fn glyph(self) -> &'static str {
        match self {
            Category::Display => "󰍹",
            Category::Audio => "󰕾",
            Category::Network => "󰖩",
            Category::Bluetooth => "󰂯",
            Category::Workspaces => "󰕮",
            Category::Collaboration => "󰡉",
            Category::About => "󰋽",
        }
    }

    /// Stable identifier suffix used to compose CRDT paths and
    /// `settings.sidebar.<slug>` test ids. Matches the snake_case
    /// convention `ui`'s e2e harness uses for `test_id` values.
    pub fn slug(self) -> &'static str {
        match self {
            Category::Display => "display",
            Category::Audio => "audio",
            Category::Network => "network",
            Category::Bluetooth => "bluetooth",
            Category::Workspaces => "workspaces",
            Category::Collaboration => "collaboration",
            Category::About => "about",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_categories_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in Category::ALL {
            assert!(seen.insert(*c), "duplicate category {:?}", c);
        }
        assert_eq!(seen.len(), Category::ALL.len());
    }

    #[test]
    fn every_category_has_a_label_and_slug() {
        for c in Category::ALL {
            assert!(!c.label().is_empty(), "{c:?} missing label");
            assert!(!c.slug().is_empty(), "{c:?} missing slug");
        }
    }
}
