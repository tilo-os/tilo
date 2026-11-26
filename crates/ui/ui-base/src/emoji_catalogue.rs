//! The Unicode emoji the toolkit offers: the shared picker's and reaction strip's catalogue, the names
//! and keywords a text field resolves `:shortcode:` with, and how often the person used each emoji.

use std::cell::RefCell;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Mutex;

use tilo_lock::deferred::Deferred;

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
/// tone it has (`0` lightest to `4` darkest).
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

    /// `emoji` in each of its forms: without a tone, then each tone from the lightest.
    fn tones(&self, emoji: &str) -> Vec<&'static str>;
}

/// The catalogue in a language (a BCP 47 tag), as a program hands it over.
pub type CatalogueFor = fn(&str) -> &'static dyn EmojiCatalogue;

/// A catalogue a program handed over before first use, where it has one;
/// otherwise the toolkit is read.
pub static CATALOGUE: Deferred<CatalogueFor> = Deferred::new();

/// One `:shortcode:` the catalogue offers for what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcode {
    /// The name as a shortcode, without colons (`thumbs_up`).
    pub name: String,
    /// What accepting it puts in the text: the emoji character, or a workspace emoji written `:name:`.
    pub emoji: String,
}

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

/// The toolkit's catalogue in a language, one wrapper per locale ever asked for (a handful), kept for
/// the process like the catalogues themselves.
#[must_use]
pub fn builtin_catalogue(tag: &str) -> &'static dyn EmojiCatalogue {
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
/// handed over, or the toolkit's. Always present, so a text field's
/// `:name:` completion and the picker never wait for a catalogue.
#[must_use]
pub fn catalogue() -> &'static dyn EmojiCatalogue {
    let tag = crate::i18n::I18nManager::current_locale();
    match CATALOGUE.get() {
        Some(for_locale) => for_locale(&tag),
        None => builtin_catalogue(&tag),
    }
}

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

/// The emoji `name` (written between colons, with `_`, `-` or spaces) resolves to: a workspace emoji
/// first, then the catalogue name.
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
    let catalogue = catalogue();
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

/// Up to `limit` emoji whose name matches what was typed after the `:`, best first.
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
    let catalogue = catalogue();
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
    found.truncate(limit);
    found
}

/// The name of `emoji` in the interface's language: a workspace emoji's `:name:` as written, a Unicode
/// emoji's name from the catalogue (with its skin tone), or the emoji itself where the catalogue does
/// not know it.
#[must_use]
pub fn emoji_name(emoji: &str) -> String {
    if tilo_actions::custom_emoji_name(emoji).is_some() {
        return emoji.to_owned();
    }
    let Some(info) = catalogue().info(emoji) else {
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

/// How often the person used each emoji: the most recently used first, with how many times each
/// was. Two spellings of one emoji (a skin tone, with or without U+FE0F) are one, and the latest
/// spelling stands for it. What was used least lately falls off the end, so an emoji someone
/// stopped using leaves the frequent row in time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiUses {
    used: Vec<Used>,
}

/// One emoji the person used, and how many times.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Used {
    pub emoji: String,
    pub uses: u32,
}

fn identity(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|&c| c != '\u{FE0F}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(&c))
}

impl EmojiUses {
    /// How many the frequent row shows: four rows of the picker's grid.
    pub const SHOWN: usize = 32;
    /// How many are counted: what was used least lately past these is forgotten.
    pub const KEPT: usize = 96;

    /// `used` (most recently used first), normalised.
    #[must_use]
    pub fn from_used(used: Vec<Used>) -> Self {
        let mut uses = Self::default();
        for used in used {
            let emoji = used.emoji.trim();
            if emoji.is_empty() || uses.position(emoji).is_some() {
                continue;
            }
            uses.used.push(Used {
                emoji: emoji.to_owned(),
                uses: used.uses.max(1),
            });
        }
        uses.used.truncate(Self::KEPT);
        uses
    }

    fn position(&self, emoji: &str) -> Option<usize> {
        self.used
            .iter()
            .position(|used| identity(&used.emoji).eq(identity(emoji)))
    }

    /// `emoji` was used once more.
    pub fn record(&mut self, emoji: &str) {
        let emoji = emoji.trim();
        if emoji.is_empty() {
            return;
        }
        let uses = self
            .position(emoji)
            .map_or(0, |at| self.used.remove(at).uses);
        self.used.insert(
            0,
            Used {
                emoji: emoji.to_owned(),
                uses: uses.saturating_add(1),
            },
        );
        self.used.truncate(Self::KEPT);
    }

    /// Forget `emoji`, whatever its spelling: it was not used after all.
    pub fn forget(&mut self, emoji: &str) -> bool {
        let before = self.used.len();
        self.used
            .retain(|used| !identity(&used.emoji).eq(identity(emoji)));
        self.used.len() != before
    }

    /// What is counted, most recently used first: what a store keeps.
    #[must_use]
    pub fn used(&self) -> &[Used] {
        &self.used
    }

    /// The emoji used most, most first; of two used as often, the one used last first.
    #[must_use]
    pub fn frequent(&self) -> Vec<String> {
        let mut by_uses: Vec<&Used> = self.used.iter().collect();
        by_uses.sort_by_key(|used| std::cmp::Reverse(used.uses));
        by_uses
            .into_iter()
            .take(Self::SHOWN)
            .map(|used| used.emoji.clone())
            .collect()
    }
}

/// Where a program keeps how often the person used each emoji between runs: the person's own,
/// one for every app, never in a record the workspace's members read. Without one it lasts as
/// long as the program.
pub trait UsesStore {
    /// What is kept, most recently used first; nothing kept reads as none.
    fn load(&self) -> Vec<Used>;
    /// Keep `used` (most recently used first) in place of what was kept.
    fn save(&self, used: &[Used]);
}

/// The uses as a private file in the person's state directory
/// (`$XDG_STATE_HOME/tilo/emoji-recents.json`): what a program that keeps nothing else gets, so the
/// emoji someone uses most lead every picker of every app.
pub struct DeviceUses {
    path: PathBuf,
}

/// Most a kept file may hold: many times what is kept, never a disk.
const KEPT_LIMIT: u64 = 16 * 1024;

impl DeviceUses {
    /// The uses kept on this device, where the state directory is known.
    #[must_use]
    pub fn on_the_device() -> Option<Self> {
        let base = tilo_paths::state_dir()?;
        Some(Self::at(base.join("tilo").join("emoji-recents.json")))
    }

    /// The uses kept in `path`, whose directory is made when saving.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// An emoji as a kept file holds it: counted, or, as the file once held them, a bare emoji used
/// once.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Kept {
    Counted(Used),
    Bare(String),
}

impl UsesStore for DeviceUses {
    fn load(&self) -> Vec<Used> {
        let Ok(file) = tilo_files::native::private::read(&self.path) else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        if file.take(KEPT_LIMIT + 1).read_to_end(&mut bytes).is_err()
            || bytes.len() as u64 > KEPT_LIMIT
        {
            return Vec::new();
        }
        serde_json::from_slice::<Vec<Kept>>(&bytes)
            .unwrap_or_default()
            .into_iter()
            .map(|kept| match kept {
                Kept::Counted(used) => used,
                Kept::Bare(emoji) => Used { emoji, uses: 1 },
            })
            .collect()
    }

    fn save(&self, used: &[Used]) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(used) else {
            return;
        };
        if let Err(error) = tilo_files::native::private::directory(parent)
            .and_then(|()| tilo_files::native::private::write_atomic(&self.path, &bytes))
        {
            tracing::debug!(%error, "how often emoji were used is not kept on this device");
        }
    }
}

thread_local! {
    static USES: RefCell<Option<EmojiUses>> = const { RefCell::new(None) };
    static STORE: RefCell<Option<Box<dyn UsesStore>>> = const { RefCell::new(None) };
}

/// Keep the uses in [`DeviceUses::on_the_device`], unless the program handed its store over
/// already. A program calls this once as it starts; every picker then opens with the emoji the
/// person uses most.
pub fn keep_uses_on_the_device() {
    if STORE.with(|kept| kept.borrow().is_some()) {
        return;
    }
    if let Some(store) = DeviceUses::on_the_device() {
        keep_uses_in(Box::new(store));
    }
}

/// Keep the uses in `store` from now on, reading what it has.
pub fn keep_uses_in(store: Box<dyn UsesStore>) {
    let used = store.load();
    USES.with(|uses| *uses.borrow_mut() = Some(EmojiUses::from_used(used)));
    STORE.with(|kept| *kept.borrow_mut() = Some(store));
}

/// The emoji the person uses most, most first: every picker of the program shares them.
#[must_use]
pub fn frequent() -> Vec<String> {
    USES.with(|uses| {
        uses.borrow()
            .as_ref()
            .map(EmojiUses::frequent)
            .unwrap_or_default()
    })
}

/// `emoji` was just used (picked, or reacted with).
pub fn record_used(emoji: &str) {
    change_uses(|uses| {
        uses.record(emoji);
    });
}

/// `emoji` was not used after all — a pick that reached a field keeping its content secret — so it is
/// not counted.
pub fn forget_used(emoji: &str) {
    change_uses(|uses| {
        uses.forget(emoji);
    });
}

fn change_uses(change: impl FnOnce(&mut EmojiUses)) {
    let used = USES.with(|uses| {
        let mut uses = uses.borrow_mut();
        let uses = uses.get_or_insert_with(EmojiUses::default);
        change(uses);
        uses.used().to_vec()
    });
    STORE.with(|store| {
        if let Some(store) = store.borrow().as_ref() {
            store.save(&used);
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn emoji_names_resolve_through_the_catalogue;

        phase a_shortcode_names_an_emoji_and_an_unknown_name_does_not;
        {
            assert_eq!(shortcode_emoji("pizza"), Some("🍕".to_owned()));
            assert_eq!(shortcode_emoji(":pizza:"), Some("🍕".to_owned()));
            assert_eq!(shortcode_emoji("definitely_not_an_emoji"), None);
            assert_eq!(shortcode_emoji(""), None);
        }

        phase what_was_typed_finds_the_emoji_whose_name_starts_with_it;
        {
            let found = shortcode_matches("piz", 8);
            assert!(found.iter().any(|shortcode| shortcode.emoji == "🍕"));
            assert!(found.iter().all(|shortcode| !shortcode.name.is_empty()));
            assert!(shortcode_matches("zzzzzz", 8).is_empty());
        }

        phase a_name_without_a_catalogue_is_the_emoji_itself;
        {
            assert_eq!(emoji_name(":party_parrot:"), ":party_parrot:");
        }
    }

    tilo_test_env::scenario! {
        fn emoji_uses_rank_forget_and_persist;

        phase the_most_used_lead_and_spellings_count_as_one;
        {
            let mut uses = EmojiUses::default();
            uses.record("👍");
            uses.record(":party_parrot:");
            uses.record("👍🏽");
            uses.record("🎉");
            assert_eq!(
                uses.frequent(),
                ["👍🏽", "🎉", ":party_parrot:"],
                "two uses of one emoji in two tones lead, in the tone used last; then the latest"
            );
            for _ in 0..3 {
                uses.record(":party_parrot:");
            }
            assert_eq!(uses.frequent()[0], ":party_parrot:");
            for n in 0..200 {
                uses.record(&format!(":e{n}:"));
            }
            assert_eq!(uses.used().len(), EmojiUses::KEPT);
            assert_eq!(uses.frequent().len(), EmojiUses::SHOWN);
            assert!(
                !uses.frequent().iter().any(|emoji| emoji == ":party_parrot:"),
                "what was not used lately is forgotten, however often it was once"
            );
            let restored = EmojiUses::from_used(uses.used().to_vec());
            assert_eq!(restored, uses, "what is kept reads back as it was");
        }

        phase a_forgotten_emoji_is_gone_in_every_spelling;
        {
            let mut uses = EmojiUses::default();
            uses.record("👍");
            uses.record("🎉");
            uses.record("👍🏽");
            assert!(uses.forget("👍"), "its other spelling is there");
            assert_eq!(uses.frequent(), ["🎉"], "every spelling of it is gone");
            assert!(!uses.forget("👍🏻"), "a tone of it is no longer there");
            assert!(!uses.forget("🍕"), "what was never there is no change");
        }

        phase the_uses_are_kept_where_the_program_keeps_them;
        {
            use std::rc::Rc;
            struct Kept(Rc<RefCell<Vec<Used>>>);
            impl UsesStore for Kept {
                fn load(&self) -> Vec<Used> {
                    self.0.borrow().clone()
                }
                fn save(&self, used: &[Used]) {
                    *self.0.borrow_mut() = used.to_vec();
                }
            }
            let kept = Rc::new(RefCell::new(vec![Used {
                emoji: "🎉".to_owned(),
                uses: 2,
            }]));
            keep_uses_in(Box::new(Kept(Rc::clone(&kept))));
            assert_eq!(frequent(), ["🎉"], "what was kept reads back");
            record_used(":tilo:");
            assert_eq!(frequent(), ["🎉", ":tilo:"], "used twice leads used once");
            record_used(":tilo:");
            record_used(":tilo:");
            assert_eq!(frequent(), [":tilo:", "🎉"]);
            assert_eq!(
                *kept.borrow(),
                [
                    Used {
                        emoji: ":tilo:".to_owned(),
                        uses: 3
                    },
                    Used {
                        emoji: "🎉".to_owned(),
                        uses: 2
                    }
                ]
            );
        }

        phase the_uses_kept_on_the_device_come_back_next_time;
        {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let path = dir.path().join("state/tilo/emoji-recents.json");
            let store = DeviceUses::at(&path);
            assert!(store.load().is_empty(), "a device with nothing kept has none");

            let used = vec![
                Used {
                    emoji: "👍".to_owned(),
                    uses: 4,
                },
                Used {
                    emoji: ":party_parrot:".to_owned(),
                    uses: 1,
                },
            ];
            store.save(&used);
            assert_eq!(
                DeviceUses::at(&path).load(),
                used,
                "what was used comes back with how often"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(&path)
                    .expect("the kept file")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "only the person reads what they used");
            }

            std::fs::write(&path, r#"["🎉", {"emoji": "👍", "uses": 3}]"#)
                .expect("a file as it was once kept");
            assert_eq!(
                store.load(),
                [
                    Used {
                        emoji: "🎉".to_owned(),
                        uses: 1
                    },
                    Used {
                        emoji: "👍".to_owned(),
                        uses: 3
                    }
                ],
                "an emoji kept bare was used once"
            );

            std::fs::write(&path, b"{ not uses").expect("damage the kept file");
            assert!(
                store.load().is_empty(),
                "an unreadable file reads as none, never a failure"
            );
        }
    }
}
