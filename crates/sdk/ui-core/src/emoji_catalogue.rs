//! The Unicode emoji the toolkit offers: the shared picker's and reaction
//! strip's catalogue, the names and keywords a text field resolves
//! `:shortcode:` with, and the person's recently used emoji.
//!
//! The catalogue is the toolkit's own dependency: every program that uses
//! `ui` has it, so an app never names a catalogue crate simply to have
//! emoji, and every text field can resolve `:name:` without knowing that a
//! catalogue exists. A workspace's own emoji are *added* to it
//! ([`crate::workspace_emoji`]), never a replacement for it; a program may
//! still hand over a catalogue of its own before first use ([`CATALOGUE`]),
//! which then wins over the built-in one.
//!
//! Recents are the person's own and stay on the device ([`EmojiRecents`]):
//! a workspace emoji (`:name:`) is remembered like any other.

use std::cell::RefCell;
use std::sync::Mutex;

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

/// A catalogue a program handed over before first use, where it has one;
/// otherwise the toolkit's own is read.
pub static CATALOGUE: Deferred<CatalogueFor> = Deferred::new();

/// One `:shortcode:` the catalogue offers for what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcode {
    /// The name as a shortcode, without colons (`thumbs_up`).
    pub name: String,
    /// What accepting it puts in the text: the emoji character, or a
    /// workspace emoji written `:name:`.
    pub emoji: String,
}

/// The toolkit's catalogue in one language: the `emoji` crate's names and
/// keywords, wrapped as the picker's [`EmojiCatalogue`].
struct Builtin(&'static emoji::Catalogue);

impl EmojiCatalogue for Builtin {
    fn groups(&self) -> Vec<CatalogueGroup> {
        self.0
            .groups()
            .iter()
            .map(|group| CatalogueGroup {
                id: group.id(),
                label: group.label(),
                icon: group.icon(),
            })
            .collect()
    }

    fn group_emoji(&self, index: usize) -> Vec<&'static str> {
        self.0.groups().get(index).map_or_else(Vec::new, |group| {
            self.0
                .group_emoji(group)
                .iter()
                .map(emoji::Emoji::text)
                .collect()
        })
    }

    fn search(&self, query: &str, limit: usize) -> Vec<&'static str> {
        self.0
            .search(query, limit)
            .into_iter()
            .map(emoji::Emoji::text)
            .collect()
    }

    fn info(&self, emoji: &str) -> Option<EmojiInfo> {
        let found = self.0.lookup(emoji)?;
        Some(EmojiInfo {
            name: found.emoji.name(),
            tone: found
                .tone
                .and_then(|tone| emoji::SkinTone::ALL.iter().position(|t| *t == tone))
                .and_then(|at| u8::try_from(at).ok()),
            takes_tone: found.emoji.takes_skin_tone(),
        })
    }

    fn tones(&self, emoji: &str) -> Vec<&'static str> {
        let Some(found) = self.0.lookup(emoji) else {
            return Vec::new();
        };
        if !found.emoji.takes_skin_tone() {
            return Vec::new();
        }
        std::iter::once(None)
            .chain(emoji::SkinTone::ALL.into_iter().map(Some))
            .filter_map(|tone| self.0.with_skin_tone(found.emoji.text(), tone))
            .collect()
    }
}

/// The toolkit's catalogue in a language, one wrapper per locale ever asked
/// for (a handful), kept for the process like the catalogues themselves.
fn builtin(tag: &str) -> &'static dyn EmojiCatalogue {
    static SHARED: Mutex<Vec<&'static Builtin>> = Mutex::new(Vec::new());
    let catalogue = emoji::Catalogue::for_locale(tag);
    let mut shared = SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = shared.iter().find(|s| std::ptr::eq(s.0, catalogue)) {
        return *found;
    }
    let made: &'static Builtin = Box::leak(Box::new(Builtin(catalogue)));
    shared.push(made);
    made
}

/// The catalogue in the language the interface speaks now: the one a program
/// handed over, or the toolkit's own. Always present, so a text field's
/// `:name:` completion and the picker never wait for a catalogue.
#[must_use]
pub fn catalogue() -> Option<&'static dyn EmojiCatalogue> {
    let tag = crate::i18n::I18nManager::current_locale();
    Some(match CATALOGUE.get() {
        Some(for_locale) => for_locale(&tag),
        None => builtin(&tag),
    })
}

/// `name` as a shortcode writes it: lowercase, runs of anything that is not
/// a letter or digit folded to one `_`.
fn shortcode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_alphanumeric() {
            out.extend(character.to_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// The emoji `name` (written between colons, with `_`, `-` or spaces)
/// resolves to: a workspace emoji first, then the catalogue's own name.
/// `None` for a name neither has.
#[must_use]
pub fn shortcode_emoji(name: &str) -> Option<String> {
    let name = shortcode(name);
    if name.is_empty() {
        return None;
    }
    let workspace = crate::workspace_emoji::emoji();
    if let Some(custom) = workspace
        .get(&name)
        .or_else(|| workspace.search(&name).into_iter().next())
        .filter(|custom| shortcode(&custom.name) == name)
    {
        return Some(format!(":{}:", custom.name));
    }
    let catalogue = catalogue()?;
    catalogue
        .search(name.replace('_', " ").as_str(), 256)
        .into_iter()
        .find(|emoji| {
            catalogue
                .info(emoji)
                .is_some_and(|info| shortcode(info.name) == name)
        })
        .map(str::to_owned)
}

/// Up to `limit` emoji whose name matches what was typed after the `:`, best
/// first. A workspace's own emoji come first, as they do in the picker's
/// search; the catalogue's follow in its own ranking.
#[must_use]
pub fn shortcode_matches(query: &str, limit: usize) -> Vec<Shortcode> {
    let typed = shortcode(query);
    let mut found: Vec<Shortcode> = crate::workspace_emoji::emoji()
        .search(&typed)
        .into_iter()
        .map(|custom| Shortcode {
            name: shortcode(&custom.name),
            emoji: format!(":{}:", custom.name),
        })
        .collect();
    if let Some(catalogue) = catalogue() {
        for emoji in catalogue.search(&typed.replace('_', " "), limit.saturating_add(found.len())) {
            if found.len() >= limit {
                break;
            }
            let Some(info) = catalogue.info(emoji) else {
                continue;
            };
            if found.iter().any(|known| known.emoji == emoji) {
                continue;
            }
            found.push(Shortcode {
                name: shortcode(info.name),
                emoji: emoji.to_owned(),
            });
        }
    }
    found.truncate(limit);
    found
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
    fn a_shortcode_names_an_emoji_and_an_unknown_name_does_not() {
        assert_eq!(shortcode_emoji("pizza"), Some("🍕".to_owned()));
        assert_eq!(shortcode_emoji(":pizza:"), Some("🍕".to_owned()));
        assert_eq!(shortcode_emoji("definitely_not_an_emoji"), None);
        assert_eq!(shortcode_emoji(""), None);
    }

    #[test]
    fn what_was_typed_finds_the_emoji_whose_name_starts_with_it() {
        let found = shortcode_matches("piz", 8);
        assert!(found.iter().any(|shortcode| shortcode.emoji == "🍕"));
        assert!(found.iter().all(|shortcode| !shortcode.name.is_empty()));
        assert!(shortcode_matches("zzzzzz", 8).is_empty());
    }

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
    }
}
