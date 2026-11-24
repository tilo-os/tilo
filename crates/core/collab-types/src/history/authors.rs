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

/// Names an actor by the handle it declares, else as nobody known: what history writes with
/// where no person's names are at hand (a store taking a step back).
pub struct Unnamed;

impl ResolveAuthor for Unnamed {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let name = actor_handle(actor_hex).unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned());
        author_coloured(
            name,
            short_id(actor_hex),
            crate::presence::user_color(actor_hex),
        )
    }
}

/// Display name used for an actor with no registered identity and no
/// self-declared handle. Deliberately not a person's name.
pub const UNKNOWN_AUTHOR_NAME: &str = "Unknown member";

/// Number of hex characters of the actor id shown next to a name. Eight is the
/// convention `HistoryAuthor::identity_short` documents.
#[doc(hidden)]
pub const SHORT_LEN: usize = 8;

/// Where the words a reader names software by come from — a history row, a presence mark, a
/// catch-up line: every surface that says who made a change.
pub trait MemberNames {
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

/// The words a self-declared name is shown in: the name a person reads, its first letter
/// capitalized ("scribe" → "Scribe"). A name is what it is: nothing is derived from an id here.
#[must_use]
pub fn name_words(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return UNKNOWN_AUTHOR_NAME.to_owned();
    }
    let mut chars = name.chars();
    chars.next().map_or_else(
        || UNKNOWN_AUTHOR_NAME.to_owned(),
        |first| first.to_uppercase().chain(chars).collect(),
    )
}

thread_local! {
    /// What the session said each app installed on this device is called, and the language this
    /// process reads names in: both ride with the workspace's roster names, so a window names
    /// another app by the same feed it names people. A process no session serves names an app by
    /// the words its id makes.
    static INSTALLED_APPS: std::cell::RefCell<(String, Option<Vec<crate::history::NamedApp>>)> =
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
        language.clone_into(&mut held.0);
        true
    })
}

/// Take the installed apps the session sent (`CollabEvent::Context`), with what each is called in
/// every language its package names it in. Whether this thread's names changed.
pub fn set_installed_apps(apps: Vec<crate::history::NamedApp>) -> bool {
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
/// row) and falls through to the unknown-member path.
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

pub use crate::actors::{DEVICE_SEPARATOR, copy_writer, device_actor};

/// The actors a copy of a record writes as, to tell its changes apart: its writer, and the
/// [`device_actor`] of any handle on it.
#[derive(Clone, Debug)]
pub struct DeviceActors {
    writer: String,
    suffix: Vec<u8>,
    /// What every writer made on this copy's device starts with ([`copy_writer`]).
    device: String,
}

impl DeviceActors {
    /// The actors of the copy whose writer is `device`.
    pub fn of(device: &automerge::ActorId) -> Self {
        let mut suffix = device_actor(b"", device);
        suffix.remove(0);
        let mut separated = vec![DEVICE_SEPARATOR as u8];
        separated.extend(suffix);
        Self {
            writer: device.to_hex_string(),
            suffix: separated,
            device: crate::actors::device_part(device),
        }
    }

    /// Whether this copy wrote as `actor_hex`.
    pub fn wrote(&self, actor_hex: &str) -> bool {
        actor_hex == self.writer
            || hex::decode(actor_hex).is_ok_and(|bytes| bytes.ends_with(&self.suffix))
    }

    /// Whether any copy of the record on this copy's device wrote as `actor_hex`: this one, or
    /// one opened there before it (before a restart, or in another of the app's processes).
    pub fn wrote_here(&self, actor_hex: &str) -> bool {
        self.wrote(actor_hex)
            || actor_hex.starts_with(&self.device)
            || hex::decode(actor_hex).is_ok_and(|bytes| {
                bytes
                    .iter()
                    .position(|&byte| byte == DEVICE_SEPARATOR as u8)
                    .and_then(|at| bytes.get(at.saturating_add(1)..))
                    .is_some_and(|part| part.starts_with(self.device.as_bytes()))
            })
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

/// What wrote a change for a person: an agent, in one of its sessions or not, a Tilo app, or a
/// third-party program. Presentation only: it grants nothing.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AttributedWriter {
    /// An agent acting through Tilo actions, in one of its sessions or not.
    Agent {
        agent: String,
        session: Option<String>,
    },
    /// A Tilo app acting as itself, by its id.
    App(String),
    /// A third-party desktop program, by its desktop id.
    Program(String),
}

impl AttributedWriter {
    #[must_use]
    pub fn agent(agent: impl Into<String>) -> Self {
        Self::Agent {
            agent: agent.into(),
            session: None,
        }
    }

    /// One session of the agent `agent`.
    #[must_use]
    pub fn agent_session(agent: impl Into<String>, session: impl Into<String>) -> Self {
        Self::Agent {
            agent: agent.into(),
            session: Some(session.into()),
        }
    }

    /// Who acted: the agent, in its session when it has one. `None` for an
    /// app or a program, which write as the person.
    #[must_use]
    pub fn acting(&self) -> Option<tilo_actions::Acting> {
        let Self::Agent { agent, session } = self else {
            return None;
        };
        let acting = tilo_actions::Acting::agent(agent.clone());
        Some(match session {
            Some(session) => acting.in_session(session.clone()),
            None => acting,
        })
    }
}

/// An author that acted for someone: an agent's change is the agent's, on
/// behalf of its person, in every history (an app's, the Files history, and
/// what the history actions return).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAttribution {
    /// Who acted, and who for, as it was authenticated.
    pub acting: tilo_actions::Acting,
    /// That person's display name.
    pub on_behalf_of_name: Option<String>,
    /// What wrote the change, when the device recorded a writer beside the
    /// version rather than only a delegated agent: an agent session, an app
    /// or a third-party program. The device asserts it; it grants nothing.
    pub writer: Option<AttributedWriter>,
    /// The writer's own name: the agent's, the app's or the program's.
    pub writer_label: Option<String>,
    /// The device whose session vouched for the writer, by the name its
    /// person gave it.
    pub device: Option<String>,
    /// The folder inside the workspace the change is in.
    pub folder: Option<String>,
}

impl HistoryAttribution {
    /// An attribution of an action's invoker acting for a person, as it was
    /// before a device began recording the writer beside the version.
    #[must_use]
    pub fn new(acting: tilo_actions::Acting) -> Self {
        Self {
            acting,
            on_behalf_of_name: None,
            writer: None,
            writer_label: None,
            device: None,
            folder: None,
        }
    }

    /// Record whom it acted for.
    #[must_use]
    pub fn acting_for(mut self, actor: impl Into<String>, name: impl Into<String>) -> Self {
        let person = actor.into();
        if self.acting.person_id() != Some(person.as_str()) {
            self.acting.acted_for = vec![tilo_actions::Acting::person(person)];
        }
        self.on_behalf_of_name = Some(name.into());
        self
    }

    /// Record what wrote the change and the name it goes by.
    #[must_use]
    pub fn written_by(mut self, writer: AttributedWriter, label: impl Into<String>) -> Self {
        self.writer = Some(writer);
        self.writer_label = Some(label.into());
        self
    }

    /// Record the device whose session vouched for the writer.
    #[must_use]
    pub fn from_device(mut self, device: Option<String>) -> Self {
        self.device = device;
        self
    }

    /// Record the folder inside the workspace the change is in.
    #[must_use]
    pub fn in_folder(mut self, folder: Option<String>) -> Self {
        self.folder = folder;
        self
    }
}

/// Who an author is to the person reading the history. Rows draw the
/// reader's own changes, a member's and an agent's differently.
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

    tilo_test_env::scenario! {
        /// Names read with no session's app names on this thread.
        fn apps_are_named_in_words_never_by_an_id;

        phase an_app_id_is_written_as_words_a_person_reads;
        {
            assert_eq!(app_words("so.tilo.lang.today"), "Today");
            assert_eq!(app_words("so.tilo.file-explorer"), "File Explorer");
            assert_eq!(app_words("org.gnome.TextEditor"), "Text Editor");
        }
    }

    #[test]
    fn an_app_name_is_the_one_the_session_sent_in_the_reader_s_language() {
        // The session knows what each installed app is called, in every language its package
        // names it in: a reader names it so, and an app this device does not have by the words
        // its id makes — never by its id.
        assert!(set_installed_apps(vec![crate::history::NamedApp {
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
}
