//! The Unicode emoji the shared emoji picker offers, and the person's
//! recently used emoji.
//!
//! The catalogue itself (every emoji with its names and keywords in every
//! language Tilo speaks, about half a megabyte) is not part of the toolkit:
//! most programs never pick an emoji, and the browser desktop keeps it out
//! of the module it starts with. A program that offers the picker hands a
//! catalogue over ([`CATALOGUE`]): an app that links the `emoji` crate
//! installs it before its first frame, a program that fetches code on first
//! use registers the fetch ([`crate::deferred::Deferred::defer`]) and
//! installs it once it arrived. Until then the picker offers the
//! workspace's emoji and the recents, and says the rest is on its way.
//!
//! Recents are the person's own and stay on the device ([`EmojiRecents`]):
//! a workspace emoji (`:name:`) is remembered like any other.

use std::cell::RefCell;

use crate::deferred::Deferred;

/// A Unicode group of the catalogue: a picker tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogueGroup {
    /// Stable, locale-independent id (`smileys-emotion`).
    pub id: &'static str,
    /// Its name in the catalogue's language.
    pub label: &'static str,
    /// The emoji its tab shows.
    pub icon: &'static str,
}

/// What an emoji is: its name in the catalogue's language and the skin
/// tone it carries (`0` lightest to `4` darkest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmojiInfo {
    pub name: &'static str,
    pub tone: Option<u8>,
    /// Whether it comes in skin tones.
    pub takes_tone: bool,
}

/// The emoji catalogue in one language, as the picker reads it.
pub trait EmojiCatalogue: Sync {
    /// The groups, in Unicode order.
    fn groups(&self) -> Vec<CatalogueGroup>;

    /// The emoji of group `index`, in Unicode order, without skin tones.
    fn group_emoji(&self, index: usize) -> Vec<&'static str>;

    /// Up to `limit` emoji matching `query` by name or keyword, best first.
    fn search(&self, query: &str, limit: usize) -> Vec<&'static str>;

    /// What `emoji` (with or without a tone) is, when the catalogue has it.
    fn info(&self, emoji: &str) -> Option<EmojiInfo>;

    /// `emoji` in each of its forms: without a tone, then each tone from
    /// the lightest. Empty for an emoji that takes no tone.
    fn tones(&self, emoji: &str) -> Vec<&'static str>;
}

/// The catalogue in a language (a BCP 47 tag), as a program hands it over.
pub type CatalogueFor = fn(&str) -> &'static dyn EmojiCatalogue;

/// The catalogue, once a program handed it over.
pub static CATALOGUE: Deferred<CatalogueFor> = Deferred::new();

/// The catalogue in the language the interface speaks now, fetching it the
/// first time where the program registered a fetch. `None` while it is on
/// its way, or where the program offers none.
#[must_use]
pub fn catalogue() -> Option<&'static dyn EmojiCatalogue> {
    CATALOGUE
        .get()
        .map(|for_locale| for_locale(&crate::i18n::I18nManager::current_locale()))
}

/// The name of `emoji` in the interface's language: a workspace emoji's
/// `:name:` as written, a Unicode emoji's name from the catalogue (with its
/// skin tone), or the emoji itself while the catalogue is not in.
#[must_use]
pub fn emoji_name(emoji: &str) -> String {
    if tilo_resource::custom_emoji_name(emoji).is_some() {
        return emoji.to_owned();
    }
    let Some(info) = catalogue().and_then(|catalogue| catalogue.info(emoji)) else {
        return emoji.to_owned();
    };
    match info.tone.and_then(tone_key) {
        None => info.name.to_owned(),
        Some(tone) => crate::i18n::widget_format(
            "emoji-chooser-toned",
            &[
                ("name", info.name.into()),
                ("tone", crate::i18n::widget_format(tone, &[]).into()),
            ],
        ),
    }
}

fn tone_key(tone: u8) -> Option<&'static str> {
    Some(match tone {
        0 => "emoji-chooser-tone-light",
        1 => "emoji-chooser-tone-medium-light",
        2 => "emoji-chooser-tone-medium",
        3 => "emoji-chooser-tone-medium-dark",
        4 => "emoji-chooser-tone-dark",
        _ => return None,
    })
}

/// Recently used emoji, most recent first, deduplicated and capped.
///
/// Two spellings of one emoji (another skin tone, with or without
/// U+FE0F) count as one entry, and the latest spelling wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiRecents {
    items: Vec<String>,
}

impl Default for EmojiRecents {
    fn default() -> Self {
        Self::from_items(Vec::new())
    }
}

/// Identity for dedup: no tone modifiers, no presentation selectors.
fn identity(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|&c| c != '\u{FE0F}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(&c))
}

impl EmojiRecents {
    /// Four rows of the picker's grid.
    pub const CAPACITY: usize = 32;

    /// `items` (most recent first), normalised.
    #[must_use]
    pub fn from_items(items: Vec<String>) -> Self {
        let mut recents = Self { items: Vec::new() };
        for item in items.iter().rev() {
            recents.record(item);
        }
        recents
    }

    /// Mark `emoji` as just used.
    pub fn record(&mut self, emoji: &str) {
        let emoji = emoji.trim();
        if emoji.is_empty() {
            return;
        }
        self.items
            .retain(|item| !identity(item).eq(identity(emoji)));
        self.items.insert(0, emoji.to_owned());
        self.items.truncate(Self::CAPACITY);
    }

    /// The emoji, most recent first.
    #[must_use]
    pub fn items(&self) -> &[String] {
        &self.items
    }
}

/// Where a program keeps the person's recents between runs: on the device
/// only, never in a shared record. Without one they last as long as the
/// program.
pub trait RecentsStore {
    /// What is kept, most recent first; nothing kept reads as none.
    fn load(&self) -> Vec<String>;
    /// Keep `items` (most recent first) in place of what was kept.
    fn save(&self, items: &[String]);
}

thread_local! {
    static RECENTS: RefCell<Option<EmojiRecents>> = const { RefCell::new(None) };
    static STORE: RefCell<Option<Box<dyn RecentsStore>>> = const { RefCell::new(None) };
}

/// Keep the recents in `store` from now on, reading what it holds.
pub fn keep_recents_in(store: Box<dyn RecentsStore>) {
    let items = store.load();
    RECENTS.with(|recents| *recents.borrow_mut() = Some(EmojiRecents::from_items(items)));
    STORE.with(|kept| *kept.borrow_mut() = Some(store));
}

/// The person's recently used emoji, most recent first: every picker of the
/// program shares them.
#[must_use]
pub fn recents() -> Vec<String> {
    RECENTS.with(|recents| {
        recents
            .borrow()
            .as_ref()
            .map(|recents| recents.items().to_vec())
            .unwrap_or_default()
    })
}

/// `emoji` was just used (picked, or reacted with).
pub fn record_recent(emoji: &str) {
    let items = RECENTS.with(|recents| {
        let mut recents = recents.borrow_mut();
        let recents = recents.get_or_insert_with(EmojiRecents::default);
        recents.record(emoji);
        recents.items().to_vec()
    });
    STORE.with(|store| {
        if let Some(store) = store.borrow().as_ref() {
            store.save(&items);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_dedup_spellings_keep_custom_emoji_and_cap() {
        let mut recents = EmojiRecents::default();
        recents.record("👍");
        recents.record(":party_parrot:");
        recents.record("👍🏽");
        assert_eq!(recents.items(), ["👍🏽", ":party_parrot:"]);
        for n in 0..40 {
            recents.record(&format!(":e{n}:"));
        }
        assert_eq!(recents.items().len(), EmojiRecents::CAPACITY);
        assert_eq!(recents.items()[0], ":e39:");
        let restored = EmojiRecents::from_items(recents.items().to_vec());
        assert_eq!(restored, recents, "a kept list reads back in its order");
    }

    #[test]
    fn the_recents_are_kept_where_the_program_keeps_them() {
        use std::rc::Rc;
        struct Kept(Rc<RefCell<Vec<String>>>);
        impl RecentsStore for Kept {
            fn load(&self) -> Vec<String> {
                self.0.borrow().clone()
            }
            fn save(&self, items: &[String]) {
                *self.0.borrow_mut() = items.to_vec();
            }
        }
        let kept = Rc::new(RefCell::new(vec!["🎉".to_owned()]));
        keep_recents_in(Box::new(Kept(Rc::clone(&kept))));
        assert_eq!(recents(), ["🎉"], "what was kept reads back");
        record_recent(":tilo:");
        assert_eq!(*kept.borrow(), [":tilo:", "🎉"]);
        assert_eq!(recents(), [":tilo:", "🎉"]);
    }

    #[test]
    fn a_name_without_a_catalogue_is_the_emoji_itself() {
        assert_eq!(emoji_name(":party_parrot:"), ":party_parrot:");
        if CATALOGUE.get().is_none() {
            assert_eq!(emoji_name("👍"), "👍");
        }
    }
}
