//! Who wrote a change, as data: an automerge actor id decoded to the handle
//! its process chose, the actors one device writes as, and the author a
//! history names, with the colour presence paints them in.
//!
//! Three shapes of actor occur in documents: a raw 32-byte public key (the
//! only cryptographically meaningful one); a self-declared handle
//! (`ActorId::from(app_id.as_bytes())`, **unverified**), read back by
//! [`actor_handle`]; and a random id, which resolves to nobody. Naming
//! someone in the reader's words (the toolkit's `AuthorDirectory`) is
//! presentation and stays above; a revert asks for names through
//! [`ResolveAuthor`].

/// Resolves an actor to the author history names: what a revert says when
/// somebody's later edits held part of it back.
pub trait ResolveAuthor {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor;
}

/// Display name used for an actor with no registered identity and no
/// self-declared handle. Deliberately not a person's name.
pub const UNKNOWN_AUTHOR_NAME: &str = "Unknown participant";

/// Number of hex characters of the actor id shown next to a name. Eight is the
/// convention `HistoryAuthor::identity_short` documents.
#[doc(hidden)]
pub const SHORT_LEN: usize = 8;

/// The namespace of the principal an application acts under (`app:so.tilo.notes`).
pub const APP_NAMESPACE: &str = "app:";
/// The namespace of the principal an agent acts under (`agent:scribe`).
pub const AGENT_NAMESPACE: &str = "agent:";
/// The namespace of the principal an installed bot acts under (`bot:status`).
pub const BOT_NAMESPACE: &str = "bot:";
/// The namespace of the principal a Shortcut's run acts under (`shortcut:morning`).
pub const SHORTCUT_NAMESPACE: &str = "shortcut:";

/// Where the words a reader names software by come from — a history row, a presence mark, a
/// catch-up line: every surface that says who made a change.
pub trait ParticipantNames {
    /// What the reader calls the person `id`, when they know a name for them. A person a reader
    /// has no name for is [`UNKNOWN_AUTHOR_NAME`], never their key.
    fn person(&self, _id: &str) -> Option<String> {
        None
    }

    /// What the reader calls the app `app_id`: the session's name for it in this window's
    /// language ([`installed_app_name`]), else the words its id makes ("Today") — an app
    /// installed nowhere near this device.
    fn app(&self, app_id: &str) -> String {
        installed_app_name(app_id).unwrap_or_else(|| app_words(app_id))
    }

    /// What the reader calls the agent whose id is `name` ([`name_words`]): its name, and — for
    /// a reader whose surfaces mark software in words rather than with an agent badge — that it
    /// is one.
    fn agent(&self, name: &str) -> String {
        name_words(name)
    }
}

/// What the actor `id` is called to the person reading `names`: a person by their name, an app
/// by its own name in their language, an agent by the name it goes by. The id itself is never
/// shown, and never cut short: a reader who knows no name reads [`UNKNOWN_AUTHOR_NAME`].
pub fn actor_name(id: &str, names: &dyn ParticipantNames) -> String {
    let id = id.trim();
    if let Some(app) = id.strip_prefix(APP_NAMESPACE) {
        return names.app(app.trim());
    }
    if let Some(agent) = id.strip_prefix(AGENT_NAMESPACE) {
        return names.agent(agent);
    }
    if let Some(bot) = id.strip_prefix(BOT_NAMESPACE) {
        return names.person(bot).unwrap_or_else(|| name_words(bot));
    }
    if let Some(shortcut) = id.strip_prefix(SHORTCUT_NAMESPACE) {
        return name_words(shortcut);
    }
    names
        .person(id)
        .unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned())
}

/// The words a self-declared name is shown in: the name a person reads, its first letter
/// capitalized ("scribe" → "Scribe"). A name is what it is: nothing is derived from an id here.
#[must_use]
pub fn name_words(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return UNKNOWN_AUTHOR_NAME.to_owned();
    }
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned())
}

thread_local! {
    /// What the session said each app installed on this device is called, and the language this
    /// process reads names in: both ride with the workspace's roster names, so a window names
    /// another app by the same feed it names people. A process no session serves names an app by
    /// the words its id makes.
    static INSTALLED_APPS: std::cell::RefCell<(String, Option<Vec<collab_types::history::NamedApp>>)> =
        const { std::cell::RefCell::new((String::new(), None)) };
}

/// Say the language this process reads names in (`de-DE`), for the app names the session sends:
/// a window names an app in its own language, whatever language its people's names arrive in.
/// Whether this thread's names changed.
pub fn set_names_language(language: &str) -> bool {
    INSTALLED_APPS.with(|held| {
        let mut held = held.borrow_mut();
        if held.0 == language {
            return false;
        }
        held.0 = language.to_owned();
        true
    })
}

/// Take the installed apps the session sent (`CollabEvent::Context`), with what each is called in
/// every language its package names it in. Whether this thread's names changed.
pub fn set_installed_apps(apps: Vec<collab_types::history::NamedApp>) -> bool {
    INSTALLED_APPS.with(|held| {
        let mut held = held.borrow_mut();
        let changed = held.1.as_ref() != Some(&apps);
        held.1 = Some(apps);
        changed
    })
}

/// What the app `app_id` is called in the language this process reads (`set_names_language`), as
/// the session sent it (`set_installed_apps`): `None` for an app installed nowhere near this
/// device.
#[must_use]
pub fn installed_app_name(app_id: &str) -> Option<String> {
    let app_id = app_id.trim();
    INSTALLED_APPS.with(|held| {
        let held = held.borrow();
        let (language, apps) = (&held.0, held.1.as_ref()?);
        apps.iter()
            .find(|app| app.id == app_id)
            .map(|app| app.name_in(language).to_owned())
    })
}

/// What the app `app_id` is called to a person reading `locale`: the session's name for it in
/// that language, else [`app_words`] — an app installed nowhere near this device. Never an id.
#[must_use]
pub fn app_name(app_id: &str, locale: &str) -> String {
    installed_app_name_in(app_id, locale).unwrap_or_else(|| app_words(app_id))
}

/// [`installed_app_name`] in `locale`, whatever language this process reads.
#[must_use]
pub fn installed_app_name_in(app_id: &str, locale: &str) -> Option<String> {
    let app_id = app_id.trim();
    INSTALLED_APPS.with(|held| {
        let held = held.borrow();
        held.1.as_ref().and_then(|apps| {
            apps.iter()
                .find(|app| app.id == app_id)
                .map(|app| app.name_in(locale).to_owned())
        })
    })
}

/// An app id written as words for a person: the id's last dotted part, its `-`/`_` breaks made
/// spaces, and a camel-case hump a space too — "so.tilo.lang.today" → "Today",
/// "org.gnome.TextEditor" → "Text Editor".
#[must_use]
pub fn app_words(app_id: &str) -> String {
    let last = app_id
        .rsplit('.')
        .find(|part| !part.is_empty())
        .unwrap_or(app_id);
    let spread = last.replace(['-', '_'], " ");
    let mut broken = String::new();
    let mut before: Option<char> = None;
    for ch in spread.chars() {
        if before.is_some_and(|prev| prev.is_lowercase() || prev.is_ascii_digit())
            && ch.is_uppercase()
        {
            broken.push(' ');
        }
        before = Some(ch);
        broken.push(ch);
    }
    let said = broken
        .split_whitespace()
        .map(|word| {
            let mut letters = word.chars();
            let Some(first) = letters.next() else {
                return String::new();
            };
            first.to_uppercase().collect::<String>() + letters.as_str()
        })
        .collect::<Vec<_>>()
        .join(" ");
    if said.trim().is_empty() {
        UNKNOWN_AUTHOR_NAME.to_owned()
    } else {
        said
    }
}

/// Longest self-declared handle we will render. Anything longer is almost
/// certainly not a handle (it is a key, a blob, or an attempt to blow out the
/// row) and falls through to the unknown-participant path.
const MAX_HANDLE_LEN: usize = 64;

/// History row whose colour is already decided (a person's own colour).
pub fn author_coloured(
    name: impl Into<String>,
    identity_short: impl Into<String>,
    rgb: (u8, u8, u8),
) -> HistoryAuthor {
    HistoryAuthor::new(name, rgb_to_hue([rgb.0, rgb.1, rgb.2]), identity_short).with_rgb(rgb)
}

/// Decode an actor id back to the handle the authoring process chose for
/// itself, when it did. `ActorId::from(&[u8])` stores the bytes verbatim, so
/// `set_actor(ActorId::from(app_id.as_bytes()))` leaves the UTF-8 of `app_id`
/// in the hex. Returns `None` for anything not a plausible handle (odd-length
/// /non-hex input, non-UTF-8, control chars, empty, or too long); a 32-byte
/// public key almost never survives this filter.
pub fn actor_handle(actor_hex: &str) -> Option<String> {
    let bytes = hex::decode(actor_hex).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    let handle = s.split(DEVICE_SEPARATOR).next().unwrap_or_default();
    if handle.is_empty() || handle.len() > MAX_HANDLE_LEN || handle.chars().any(char::is_control) {
        return None;
    }
    let trimmed = handle.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Separates the handle of a [`device_actor`] from its device part.
const DEVICE_SEPARATOR: char = '\u{1f}';

/// The actor `handle` writes a document as from the device whose record
/// writer is `device`: the handle, which [`actor_handle`] reads back, and a
/// part only that device has. Automerge numbers each actor's changes, so
/// one person writing from two devices under the bare handle would make two
/// different changes with the same number, which no replica can merge.
pub fn device_actor(handle: &str, device: &automerge::ActorId) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(device.to_bytes());
    let mut actor = format!("{handle}{DEVICE_SEPARATOR}").into_bytes();
    actor.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
    actor
}

/// The actors a device writes as, to tell its changes apart: its record
/// writer, and the [`device_actor`] of any handle on it.
#[derive(Clone, Debug)]
pub struct DeviceActors {
    writer: String,
    suffix: Vec<u8>,
}

impl DeviceActors {
    /// The actors of the device whose record writer is `device`.
    pub fn of(device: &automerge::ActorId) -> Self {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(device.to_bytes());
        let mut suffix = vec![DEVICE_SEPARATOR as u8];
        suffix.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
        Self {
            writer: device.to_hex_string(),
            suffix,
        }
    }

    pub fn wrote(&self, actor_hex: &str) -> bool {
        actor_hex == self.writer
            || hex::decode(actor_hex).is_ok_and(|bytes| bytes.ends_with(&self.suffix))
    }
}

/// The bare handle's actor of a [`device_actor`], hex-encoded.
#[doc(hidden)]
pub fn handle_actor_hex(actor_hex: &str) -> Option<String> {
    let bytes = hex::decode(actor_hex).ok()?;
    let at = bytes.iter().position(|&b| b == DEVICE_SEPARATOR as u8)?;
    Some(hex::encode(bytes.get(..at)?))
}

/// First `SHORT_LEN` hex characters of an actor id, for the dim monospace
/// label next to the name. Short ids are returned whole.
pub fn short_id(actor_hex: &str) -> String {
    actor_hex.chars().take(SHORT_LEN).collect()
}

/// Deterministic avatar hue for an actor with no registered colour: FNV-1a
/// over the actor hex, folded into `[0, 360)`. Same actor, same hue in every
/// process and on every peer.
pub fn stable_hue(actor_hex: &str) -> f32 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, actor_hex.as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    // `hash % 360` is always `0..359`, which fits a `u16` exactly and so
    // converts to `f32` without any precision loss.
    let bucket = u16::try_from(hash % 360).unwrap_or(0);
    f32::from(bucket)
}

/// RGB → hue in degrees. Saturation/value are fixed by
/// [`hue_to_rgb`], so only the hue survives —
/// deliberate, it keeps the dots a coherent family.
#[doc(hidden)]
pub fn rgb_to_hue(rgb: [u8; 3]) -> f32 {
    let red = f32::from(rgb[0]) / 255.0;
    let green = f32::from(rgb[1]) / 255.0;
    let blue = f32::from(rgb[2]) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let range = max - min;
    if range <= f32::EPSILON {
        // Grey has no hue. First boot writes [128, 128, 128], so this is the
        // common case until the user picks a colour: give it a fixed,
        // recognisable slot rather than a random one.
        return 0.0;
    }
    // `max` is always the literal value of one of `red`/`green`/`blue` (never
    // a computed value of its own), so an epsilon compare here agrees with
    // exact equality in every case and only avoids the float-cmp lint.
    let hue = if (max - red).abs() <= f32::EPSILON {
        ((green - blue) / range).rem_euclid(6.0)
    } else if (max - green).abs() <= f32::EPSILON {
        (blue - red) / range + 2.0
    } else {
        (red - green) / range + 4.0
    };
    (hue * 60.0).rem_euclid(360.0)
}

/// Not crypto material — just enough metadata to render a per-author
/// colour dot, display name, and a short identity hash for
/// disambiguation when two authors share a name.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryAuthor {
    /// The name it goes by: a person's, an agent's own — the reader's account
    /// name too, since the line that names the author words the reader as
    /// "You" from [`Self::relation`]. A name, never a sentence: a row that says
    /// whom software acted for words that line once, from the facts beside it.
    pub name: String,
    /// Degrees `[0, 360)`. Kept for fixtures and any surface that still
    /// reads hue; paint uses [`HistoryAuthor::rgb`].
    pub avatar_hue: f32,
    /// Short hex fragment of the identity pubkey; the producer decides
    /// the length, the widget treats it as an opaque label.
    pub identity_short: String,
    /// The colour presence paints this identity in. Defaults to
    /// [`hue_to_rgb`] of [`Self::avatar_hue`] so existing constructors
    /// keep their look; [`Self::with_rgb`] and identity resolution set
    /// the person's colour or the shared presence palette slot
    /// (`ui::presence::PEER_PALETTE`).
    avatar_rgb: (u8, u8, u8),
    /// Who the author is to whoever reads the history: themselves, another
    /// person, or software acting for someone.
    pub relation: AuthorRelation,
    /// Who acted and for whom, when the author is an agent, a Shortcut or an
    /// app acting for a person: the facts the line that names it words.
    pub attribution: Option<HistoryAttribution>,
    /// The person the author acted for is the reader: the line says "for you".
    pub for_reader: bool,
    /// The task that named the author, as it was called then: the line reads
    /// "Ada via task ‘Prepare Q3 financials’".
    pub task: Option<String>,
}

/// An author that acted for someone: an agent's change is the agent's, on
/// behalf of its person, in every history (an app's, the Files history, and
/// what the history actions return).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAttribution {
    /// The acting principal as it was authenticated (`agent:scribe`).
    pub actor: String,
    /// What the actor is.
    pub kind: tilo_actions::ActorKind,
    /// The principal of the person it acted for, when it acted for one.
    pub on_behalf_of: Option<String>,
    /// That person's display name.
    pub on_behalf_of_name: Option<String>,
}

/// Who an author is to the person reading the history. Rows draw the
/// reader's own changes, a collaborator's and an agent's differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorRelation {
    /// The reader's own change.
    Reader,
    /// Another person's.
    #[default]
    Person,
    /// An agent's, a Shortcut's or an app's, acting for someone.
    Software,
}

impl HistoryAuthor {
    pub fn new(
        name: impl Into<String>,
        avatar_hue: f32,
        identity_short: impl Into<String>,
    ) -> Self {
        let avatar_hue = avatar_hue.rem_euclid(360.0);
        Self {
            name: name.into(),
            avatar_hue,
            identity_short: identity_short.into(),
            avatar_rgb: hue_to_rgb(avatar_hue),
            relation: AuthorRelation::Person,
            attribution: None,
            for_reader: false,
            task: None,
        }
    }

    #[must_use]
    pub fn with_relation(mut self, relation: AuthorRelation) -> Self {
        self.relation = relation;
        self
    }

    /// Record who acted and for whom.
    #[must_use]
    pub fn with_attribution(mut self, attribution: HistoryAttribution) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Use this exact RGB for the avatar, cursor and history row.
    #[must_use]
    pub fn with_rgb(mut self, rgb: (u8, u8, u8)) -> Self {
        self.avatar_rgb = rgb;
        self
    }

    pub fn rgb(&self) -> (u8, u8, u8) {
        self.avatar_rgb
    }
}

/// Rounds a normalized `[0, 1]` colour channel to a `u8` byte. The `clamp`
/// makes the truncation and sign-loss the lint warns about impossible —
/// clippy just can't see through it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0.0, 255.0] immediately above, so this can only \
              produce a value in u8's range"
)]
fn channel_u8(unit: f32) -> u8 {
    (unit * 255.0).round().clamp(0.0, 255.0) as u8
}

/// `HSV(h, 0.65, 0.95)` → RGB. Saturation and value held constant so
/// generated author colours stay visually coherent (no neon outliers,
/// no muddy ones).
pub fn hue_to_rgb(hue_deg: f32) -> (u8, u8, u8) {
    let sector = hue_deg.rem_euclid(360.0) / 60.0;
    let sat = 0.65_f32;
    let val = 0.95_f32;
    let chroma = val * sat;
    let x2 = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let min_add = val - chroma;
    let (red, green, blue) = if sector < 1.0 {
        (chroma, x2, 0.0)
    } else if sector < 2.0 {
        (x2, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, x2)
    } else if sector < 4.0 {
        (0.0, x2, chroma)
    } else if sector < 5.0 {
        (x2, 0.0, chroma)
    } else {
        (chroma, 0.0, x2)
    };
    (
        channel_u8(red + min_add),
        channel_u8(green + min_add),
        channel_u8(blue + min_add),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader whose words are the source language's, and who knows one person by name.
    struct Reader;

    impl ParticipantNames for Reader {
        fn person(&self, id: &str) -> Option<String> {
            (id == "ada").then(|| "Ada Lovelace".to_owned())
        }
    }

    #[test]
    fn every_kind_of_actor_is_named_by_what_it_is_never_by_an_id() {
        assert_eq!(actor_name("ada", &Reader), "Ada Lovelace");
        assert_eq!(
            actor_name("9f2c1a34b7", &Reader),
            UNKNOWN_AUTHOR_NAME,
            "a person nobody named is never shown by their key"
        );
        assert_eq!(actor_name("agent:scribe", &Reader), "Scribe");
        assert_eq!(actor_name("shortcut:morning", &Reader), "Morning");
        assert_eq!(actor_name("bot:status", &Reader), "Status");
        assert_eq!(
            actor_name("app:so.tilo.lang.today", &Reader),
            "Today",
            "an app is its own name, whole: never its id, never part of one"
        );
    }

    #[test]
    fn an_app_name_is_the_one_the_session_sent_in_the_reader_s_language() {
        // The session knows what each installed app is called, in every language its package
        // names it in: a reader names it so, and an app this device does not have by the words
        // its id makes — never by its id.
        assert!(set_installed_apps(vec![collab_types::history::NamedApp {
            id: "so.tilo.lang.today".to_owned(),
            name: "Today".to_owned(),
            names: [("de-DE".to_owned(), "Heute".to_owned())]
                .into_iter()
                .collect(),
        }]));
        assert_eq!(app_name("so.tilo.lang.today", "de-DE"), "Heute");
        assert_eq!(app_name("so.tilo.lang.today", "en-US"), "Today");
        assert_eq!(
            app_name("so.tilo.lang.today", ""),
            "Today",
            "a reader with no language reads the name the package gives itself"
        );
        assert_eq!(
            app_name("org.example.Editor", "de-DE"),
            "Editor",
            "an app installed nowhere near this device reads as the words its id makes"
        );
        assert!(set_installed_apps(Vec::new()), "the apps are gone");
        assert!(!set_installed_apps(Vec::new()), "the same names read once");
        assert_eq!(
            app_name("so.tilo.lang.today", "de-DE"),
            "Today",
            "with no session's names, the words its id makes"
        );
    }

    #[test]
    fn an_app_id_is_written_as_words_a_person_reads() {
        assert_eq!(app_words("so.tilo.lang.today"), "Today");
        assert_eq!(app_words("so.tilo.file-explorer"), "File Explorer");
        assert_eq!(app_words("org.gnome.TextEditor"), "Text Editor");
    }
}
