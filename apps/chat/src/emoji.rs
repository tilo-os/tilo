//! Emoji catalogue for the reaction picker + the quick-reaction bar.
//!
//! Kept tiny and curated — a real picker would page a full Unicode set,
//! but the quick bar plus a couple of category rows is enough for a deep
//! vertical slice and keeps the offline screenshot legible.

/// The six one-tap reactions shown inline on every message's hover bar.
pub const QUICK: [&str; 6] = ["👍", "🎉", "❤️", "😂", "🚀", "👀"];

/// A labelled category in the full emoji picker popover.
pub struct Category {
    pub name: &'static str,
    pub emoji: &'static [&'static str],
}

/// The full picker content: a handful of categories, each a grid row.
pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Smileys",
        emoji: &[
            "😀", "😄", "😁", "😂", "🙂", "😉", "😍", "😎", "🤔", "😇", "🥳", "😴",
        ],
    },
    Category {
        name: "Gestures",
        emoji: &[
            "👍", "👎", "👏", "🙌", "🙏", "💪", "🤝", "👋", "✌️", "🤙", "👀", "🫶",
        ],
    },
    Category {
        name: "Symbols",
        emoji: &[
            "❤️", "🔥", "⭐", "✨", "🎉", "🚀", "💯", "✅", "❌", "⚡", "💡", "📌",
        ],
    },
    Category {
        name: "Objects",
        emoji: &[
            "☕", "🍕", "🎧", "📷", "🖥️", "📝", "🐛", "🌈", "🌙", "☀️", "🎈", "🧩",
        ],
    },
];
