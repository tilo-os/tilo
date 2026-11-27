//! The keyboard layouts Tilo offers: the XKB id each is typed with and the name a picker shows.
//!
//! One table, read by the desktop when it cannot read the machine's own xkeyboard-config, by the
//! installer's picker, and by the rules an install profile is checked with. It is standard
//! library only, so the media tools that take the profile model build without the desktop.
//! A layout is added or renamed here once.

/// One layout a person can type with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Its XKB source id: `de`, or `us:dvorak` for a variant of `us`.
    pub id: &'static str,
    /// What a picker calls it.
    pub label: &'static str,
}

impl Layout {
    /// The XKB layout the id sets: `us` for `us:dvorak`.
    #[must_use]
    pub fn xkb_layout(&self) -> &'static str {
        self.id
            .split_once(':')
            .map_or(self.id, |(layout, _)| layout)
    }

    /// The XKB variant it sets; empty when it sets none.
    #[must_use]
    pub fn xkb_variant(&self) -> &'static str {
        self.id.split_once(':').map_or("", |(_, variant)| variant)
    }

    /// A layout, by the id it is typed with and the name a picker shows it under.
    #[must_use]
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self { id, label }
    }
}

/// In picker order.
pub static LAYOUTS: &[Layout] = &[
    Layout::new("us", "English (US)"),
    Layout::new("gb", "English (UK)"),
    Layout::new("de", "German"),
    Layout::new("fr", "French"),
    Layout::new("es", "Spanish"),
    Layout::new("it", "Italian"),
    Layout::new("pt", "Portuguese"),
    Layout::new("br", "Portuguese (Brazil)"),
    Layout::new("ru", "Russian"),
    Layout::new("se", "Swedish"),
    Layout::new("no", "Norwegian"),
    Layout::new("dk", "Danish"),
    Layout::new("ch", "Swiss German"),
    Layout::new("pl", "Polish"),
    Layout::new("nl", "Dutch"),
    Layout::new("us:dvorak", "Dvorak"),
    Layout::new("us:colemak", "Colemak"),
];

/// The layout `id` names, when Tilo offers it.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|layout| layout.id == id)
}
