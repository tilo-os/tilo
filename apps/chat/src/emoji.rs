//! Emoji catalogue for the reaction picker + the quick-reaction bar.
//!
//! Kept tiny and curated — a real picker would page a full Unicode set,
//! but the quick bar plus a couple of category rows is enough for a deep
//! vertical slice and keeps the offline screenshot legible.

/// The six one-tap reactions shown inline on every message's hover bar.
pub const QUICK: [&str; 6] = ["👍", "🎉", "❤️", "😂", "🚀", "👀"];

/// A labelled category in the full emoji picker popover.
pub struct Category {
    /// The catalogue key of the category's name; see [`Category::name`].
    pub name_key: &'static str,
    pub emoji: &'static [&'static str],
}

impl Category {
    /// The category's name, in the reader's words.
    pub fn name(&self) -> String {
        crate::i18n::t(self.name_key)
    }
}

/// The full picker content: a handful of categories, each a grid row.
pub const CATEGORIES: &[Category] = &[
    Category {
        name_key: "chat-emoji-category-smileys",
        emoji: &[
            "😀", "😄", "😁", "😂", "🙂", "😉", "😍", "😎", "🤔", "😇", "🥳", "😴",
        ],
    },
    Category {
        name_key: "chat-emoji-category-gestures",
        emoji: &[
            "👍", "👎", "👏", "🙌", "🙏", "💪", "🤝", "👋", "✌️", "🤙", "👀", "🫶",
        ],
    },
    Category {
        name_key: "chat-emoji-category-symbols",
        emoji: &[
            "❤️", "🔥", "⭐", "✨", "🎉", "🚀", "💯", "✅", "❌", "⚡", "💡", "📌",
        ],
    },
    Category {
        name_key: "chat-emoji-category-objects",
        emoji: &[
            "☕", "🍕", "🎧", "📷", "🖥️", "📝", "🐛", "🌈", "🌙", "☀️", "🎈", "🧩",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::CATEGORIES;

    #[test]
    fn every_category_is_named_from_the_catalogue() {
        for category in CATEGORIES {
            let name = category.name();
            assert!(!name.is_empty() && name != category.name_key, "{name}");
        }
        assert_eq!(CATEGORIES[0].name(), "Smileys");
    }
}
