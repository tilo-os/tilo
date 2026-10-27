//! Tilo's emoji catalogue, shared by the on-screen keyboard, the system emoji
//! picker and apps such as chat.
//!
//! The data is the fully-qualified set from Unicode's `emoji-test.txt`, in
//! Unicode order, with names and keywords from CLDR. It is checked in under
//! `data/` and regenerated deterministically by `tools/regenerate.py`.
//!
//! - [`Catalogue::for_locale`] names and searches in de, es, fr, it, ja, zh or
//!   zh-Hant, keeping English for anything CLDR leaves untranslated; English
//!   names and keywords still match, ranked after native ones.
//! - [`Catalogue::groups`] / [`Catalogue::subgroups`] walk the picker tabs.
//! - [`Catalogue::search`] ranks by name and keyword (prefix > word > substring).
//! - [`Catalogue::with_skin_tone`] tones only emoji that take a tone; a
//!   multi-person sequence gets the same tone on every person.
//! - [`Catalogue::gender_variants`] maps person / man / woman forms.
//! - [`Catalogue::suggest`] turns a typed word into emoji ("pizza" → 🍕).
//! - [`Recents`] is the most-recent-first list a user syncs privately.
//!
//! Only toneless base forms are listed; toned forms come from
//! [`Catalogue::with_skin_tone`], and [`Catalogue::lookup`] resolves either.
//!
//! ```
//! use emoji::{Catalogue, SkinTone};
//!
//! let cat = Catalogue::english();
//! assert_eq!(cat.search("thumbs", 1)[0].text(), "👍");
//! assert_eq!(cat.with_skin_tone("👍", Some(SkinTone::Medium)), Some("👍🏽"));
//! assert_eq!(cat.with_skin_tone("🍕", Some(SkinTone::Medium)), Some("🍕"));
//! ```

mod data;
mod recents;
mod search;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

pub use recents::Recents;

/// English names and keywords: the default catalogue and every other
/// locale's fallback for an emoji or group it does not name.
const ENGLISH: &str = include_str!("../data/en.txt");

/// Every other locale's names and keywords, as `(tag, deflated data)`,
/// deflated by build.rs from `data/<tag>.txt`.
const TRANSLATIONS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/translations.rs"));

const STRUCTURE: &str = include_str!("../data/emoji.txt");

/// The Fitzpatrick skin tone modifiers, lightest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkinTone {
    Light,
    MediumLight,
    Medium,
    MediumDark,
    Dark,
}

impl SkinTone {
    /// Every tone, lightest first.
    pub const ALL: [Self; 5] = [
        Self::Light,
        Self::MediumLight,
        Self::Medium,
        Self::MediumDark,
        Self::Dark,
    ];

    /// The modifier code point (U+1F3FB..=U+1F3FF).
    #[must_use]
    pub const fn modifier(self) -> char {
        match self {
            Self::Light => '\u{1F3FB}',
            Self::MediumLight => '\u{1F3FC}',
            Self::Medium => '\u{1F3FD}',
            Self::MediumDark => '\u{1F3FE}',
            Self::Dark => '\u{1F3FF}',
        }
    }

    /// The tone a modifier code point stands for.
    #[must_use]
    pub const fn from_modifier(c: char) -> Option<Self> {
        match c {
            '\u{1F3FB}' => Some(Self::Light),
            '\u{1F3FC}' => Some(Self::MediumLight),
            '\u{1F3FD}' => Some(Self::Medium),
            '\u{1F3FE}' => Some(Self::MediumDark),
            '\u{1F3FF}' => Some(Self::Dark),
            _ => None,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Which of a person / man / woman triple an emoji is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gender {
    Neutral,
    Man,
    Woman,
}

/// One fully-qualified, toneless emoji.
#[derive(Debug, Clone)]
pub struct Emoji {
    text: &'static str,
    name: &'static str,
    keywords: &'static str,
    group: usize,
    subgroup: usize,
    takes_skin_tone: bool,
    gender: Option<Gender>,
}

impl Emoji {
    /// The emoji itself, fully qualified.
    #[must_use]
    pub const fn text(&self) -> &'static str {
        self.text
    }

    /// The localised short name ("thumbs up"), also its accessible label.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Localised search keywords, excluding the name.
    pub fn keywords(&self) -> impl Iterator<Item = &'static str> {
        self.keywords.split('|').filter(|k| !k.is_empty())
    }

    /// Index into [`Catalogue::groups`].
    #[must_use]
    pub const fn group(&self) -> usize {
        self.group
    }

    /// Index into the catalogue's subgroups (see [`Catalogue::subgroup`]).
    #[must_use]
    pub const fn subgroup(&self) -> usize {
        self.subgroup
    }

    /// Whether [`Catalogue::with_skin_tone`] changes this emoji.
    #[must_use]
    pub const fn takes_skin_tone(&self) -> bool {
        self.takes_skin_tone
    }

    /// Its place in a person / man / woman triple, if it belongs to one.
    #[must_use]
    pub const fn gender(&self) -> Option<Gender> {
        self.gender
    }
}

/// A top-level Unicode emoji group ("Smileys & emotion"): a picker tab.
#[derive(Debug, Clone)]
pub struct Group {
    id: &'static str,
    label: &'static str,
    first: &'static str,
    subgroups: Range<usize>,
    emoji: Range<usize>,
}

impl Group {
    /// Stable, locale-independent id ("smileys-emotion"), for icons and state.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Localised label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }

    /// A representative emoji for the group's tab, where a label would not
    /// fit. Unicode's first entry is not always recognisable (Symbols opens
    /// with 🏧), so the common picker glyphs are chosen by id.
    #[must_use]
    pub fn icon(&self) -> &'static str {
        match self.id {
            "smileys-emotion" => "😀",
            "people-body" => "👋",
            "animals-nature" => "🐻",
            "food-drink" => "🍔",
            "travel-places" => "✈\u{FE0F}",
            "activities" => "⚽",
            "objects" => "💡",
            "symbols" => "❤\u{FE0F}",
            "flags" => "🏁",
            _ => self.first,
        }
    }
}

/// A Unicode emoji subgroup ("face-smiling") inside a [`Group`].
#[derive(Debug, Clone)]
pub struct Subgroup {
    id: &'static str,
    group: usize,
    emoji: Range<usize>,
}

impl Subgroup {
    /// Unicode's subgroup id.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    #[must_use]
    pub const fn group(&self) -> usize {
        self.group
    }
}

/// The man and woman forms that go with a gender-neutral emoji.
#[derive(Debug, Clone, Copy)]
pub struct GenderVariants<'a> {
    pub neutral: &'a Emoji,
    pub man: Option<&'a Emoji>,
    pub woman: Option<&'a Emoji>,
}

impl<'a> GenderVariants<'a> {
    /// The form for `gender`, if Unicode defines one.
    #[must_use]
    pub const fn get(&self, gender: Gender) -> Option<&'a Emoji> {
        match gender {
            Gender::Neutral => Some(self.neutral),
            Gender::Man => self.man,
            Gender::Woman => self.woman,
        }
    }
}

/// An emoji resolved from text: its toneless entry and the tone it carried.
#[derive(Debug, Clone, Copy)]
pub struct Found<'a> {
    pub emoji: &'a Emoji,
    pub tone: Option<SkinTone>,
}

/// The whole catalogue for one locale.
#[derive(Debug)]
pub struct Catalogue {
    locale: &'static str,
    emoji: Vec<Emoji>,
    groups: Vec<Group>,
    subgroups: Vec<Subgroup>,
    /// Key (text without U+FE0F) → index, for toneless and toned forms.
    index: HashMap<String, (usize, Option<SkinTone>)>,
    /// Index → its five toned forms, for emoji that take a tone.
    toned: HashMap<usize, [String; 5]>,
    /// Neutral index → (man, woman).
    genders: HashMap<usize, (Option<usize>, Option<usize>)>,
    /// Gendered index → its neutral index.
    neutral_of: HashMap<usize, usize>,
    /// Names and keywords, indexed on the first search.
    search: OnceLock<tilo_search::SearchIndex>,
}

static ENGLISH_CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
static TRANSLATED: [OnceLock<Catalogue>; TRANSLATIONS.len()] =
    [const { OnceLock::new() }; TRANSLATIONS.len()];
/// Inflated translations, kept for the process so catalogue entries can
/// borrow their names.
static TRANSLATED_TEXT: [OnceLock<String>; TRANSLATIONS.len()] =
    [const { OnceLock::new() }; TRANSLATIONS.len()];

impl Catalogue {
    #[must_use]
    pub fn english() -> &'static Self {
        ENGLISH_CATALOGUE.get_or_init(|| data::build("en", STRUCTURE, ENGLISH, None))
    }

    /// The catalogue for a BCP 47 or POSIX tag ("de-DE", "de_AT.UTF-8" →
    /// "de"), falling back to English for a language without data. An emoji
    /// or group the language does not name keeps its English name.
    #[must_use]
    pub fn for_locale(tag: &str) -> &'static Self {
        let tag = tag.split(['.', '@']).next().unwrap_or_default();
        let primary = tag.split(['-', '_']).next().unwrap_or_default();
        let has = |wanted: &[&str]| {
            tag.split(['-', '_'])
                .skip(1)
                .any(|s| wanted.iter().any(|w| s.eq_ignore_ascii_case(w)))
        };
        // Chinese splits by script: Traditional for an explicit Hant script
        // or, without an explicit Hans, the regions that write it.
        let traditional = primary.eq_ignore_ascii_case("zh")
            && (has(&["hant"]) || (!has(&["hans"]) && has(&["tw", "hk", "mo"])));
        let wanted = if traditional { "zh_Hant" } else { primary };
        TRANSLATIONS
            .iter()
            .position(|(l, _)| l.eq_ignore_ascii_case(wanted))
            .map_or_else(Self::english, Self::translated)
    }

    /// Locale tags with checked-in data, English first.
    pub fn locales() -> impl Iterator<Item = &'static str> {
        std::iter::once("en").chain(TRANSLATIONS.iter().map(|(l, _)| *l))
    }

    /// The catalogue for `TRANSLATIONS[slot]`, inflated and built on first
    /// use. Data that fails to inflate names nothing, so every emoji falls
    /// back to English.
    fn translated(slot: usize) -> &'static Self {
        let (Some(cell), Some(text), Some((locale, packed))) = (
            TRANSLATED.get(slot),
            TRANSLATED_TEXT.get(slot),
            TRANSLATIONS.get(slot),
        ) else {
            return Self::english();
        };
        cell.get_or_init(|| {
            let text = text.get_or_init(|| {
                miniz_oxide::inflate::decompress_to_vec(packed)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default()
            });
            data::build(locale, STRUCTURE, text, Some(ENGLISH))
        })
    }

    /// The locale this catalogue's names are in.
    #[must_use]
    pub const fn locale(&self) -> &'static str {
        self.locale
    }

    /// Every toneless emoji in Unicode order.
    #[must_use]
    pub fn emoji(&self) -> &[Emoji] {
        &self.emoji
    }

    /// The groups in Unicode order (no "Component" group).
    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// A group's emoji in Unicode order.
    #[must_use]
    pub fn group_emoji(&self, group: &Group) -> &[Emoji] {
        self.emoji.get(group.emoji.clone()).unwrap_or_default()
    }

    /// A group's subgroups in Unicode order.
    #[must_use]
    pub fn subgroups(&self, group: &Group) -> &[Subgroup] {
        self.subgroups
            .get(group.subgroups.clone())
            .unwrap_or_default()
    }

    /// A subgroup by the index [`Emoji::subgroup`] returns.
    #[must_use]
    pub fn subgroup(&self, index: usize) -> Option<&Subgroup> {
        self.subgroups.get(index)
    }

    /// A subgroup's emoji in Unicode order.
    #[must_use]
    pub fn subgroup_emoji(&self, subgroup: &Subgroup) -> &[Emoji] {
        self.emoji.get(subgroup.emoji.clone()).unwrap_or_default()
    }

    /// Resolve `text` — toneless or toned, with or without U+FE0F.
    #[must_use]
    pub fn lookup(&self, text: &str) -> Option<Found<'_>> {
        let (i, tone) = *self.index.get(&data::key(text))?;
        Some(Found {
            emoji: self.emoji.get(i)?,
            tone,
        })
    }

    /// The toneless entry for `text`.
    #[must_use]
    pub fn get(&self, text: &str) -> Option<&Emoji> {
        self.lookup(text).map(|f| f.emoji)
    }

    /// `text` in `tone` (`None` is the default yellow). Emoji that take no
    /// tone come back unchanged; a toned input is re-toned. `None` when
    /// `text` is not a known emoji.
    #[must_use]
    pub fn with_skin_tone(&self, text: &str, tone: Option<SkinTone>) -> Option<&str> {
        let (i, _) = *self.index.get(&data::key(text))?;
        let base = self.emoji.get(i)?;
        match (tone, self.toned.get(&i)) {
            (Some(tone), Some(forms)) => forms.get(tone.index()).map(String::as_str),
            _ => Some(base.text),
        }
    }

    /// The person / man / woman triple `text` belongs to.
    #[must_use]
    pub fn gender_variants(&self, text: &str) -> Option<GenderVariants<'_>> {
        let i = self.lookup(text)?;
        let i = self.position(i.emoji)?;
        let neutral = self.neutral_of.get(&i).copied().unwrap_or(i);
        let (man, woman) = *self.genders.get(&neutral)?;
        Some(GenderVariants {
            neutral: self.emoji.get(neutral)?,
            man: man.and_then(|m| self.emoji.get(m)),
            woman: woman.and_then(|w| self.emoji.get(w)),
        })
    }

    /// `text` switched to `gender`, keeping its skin tone.
    #[must_use]
    pub fn with_gender(&self, text: &str, gender: Gender) -> Option<&str> {
        let tone = self.lookup(text)?.tone;
        let target = self.gender_variants(text)?.get(gender)?;
        self.with_skin_tone(target.text, tone)
    }

    /// Up to `limit` emoji matching `query`, best first. Every whitespace
    /// separated term must match the name or a keyword; the name ranks above
    /// a keyword, and a whole word above the start of a longer word, above a
    /// substring. Whole-word ties go to the shorter name; others keep Unicode
    /// order. In a
    /// translated catalogue, emoji that match only in English follow every
    /// native match, ranked the same way.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Emoji> {
        search::search(self, query, limit)
    }

    /// Up to `limit` emoji a typed word stands for ("pizza" → 🍕), for
    /// keyboard and composer suggestions. Exact word matches only.
    #[must_use]
    pub fn suggest(&self, word: &str, limit: usize) -> Vec<&Emoji> {
        search::suggest(self, word, limit)
    }

    fn search_index(&self) -> &tilo_search::SearchIndex {
        self.search.get_or_init(|| search::index(&self.emoji))
    }

    fn position(&self, emoji: &Emoji) -> Option<usize> {
        self.index.get(&data::key(emoji.text)).map(|(i, _)| *i)
    }
}

#[cfg(test)]
mod tests;
