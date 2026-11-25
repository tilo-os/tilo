//! A person's presence colour: one of twelve hues they choose, or the one
//! their identity derives until they do.
//!
//! A colour is part of someone's identity, not of a theme, so it travels as
//! this choice (and as the sRGB colour [`identity_rgb`] gives it) and each
//! viewer's toolkit paints it at its own theme's lightness and chroma.

/// How many hues the presence palette has.
pub const PRESENCE_LEN: usize = tilo_face::PRESENCE_LEN;

/// The presence palette's hues, in OKLCH degrees: twelve evenly spaced, so
/// neighbours sit 30° apart.
pub const PRESENCE_HUES: [f32; PRESENCE_LEN] = [
    25.0, 55.0, 85.0, 115.0, 145.0, 175.0, 205.0, 235.0, 265.0, 295.0, 325.0, 355.0,
];

/// One of the twelve presence hues: a person's colour, as they choose it.
///
/// A colour is part of someone's identity, not of a theme: it is stored as
/// this choice and painted through whatever theme each viewer uses
/// (the toolkit's presence palette), so it reads at that theme's presence
/// lightness and chroma everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceHue {
    Red,
    Orange,
    Amber,
    Olive,
    Green,
    Teal,
    Cyan,
    Sky,
    Blue,
    Violet,
    Magenta,
    Pink,
}

impl PresenceHue {
    /// Every hue, in [`PRESENCE_HUES`] order.
    pub const ALL: [Self; PRESENCE_LEN] = [
        Self::Red,
        Self::Orange,
        Self::Amber,
        Self::Olive,
        Self::Green,
        Self::Teal,
        Self::Cyan,
        Self::Sky,
        Self::Blue,
        Self::Violet,
        Self::Magenta,
        Self::Pink,
    ];

    /// The palette slot, `0..PRESENCE_LEN`.
    #[must_use]
    pub fn slot(self) -> usize {
        Self::ALL.iter().position(|h| *h == self).unwrap_or(0)
    }

    /// The hue at `slot`, wrapping past [`PRESENCE_LEN`].
    #[must_use]
    pub fn from_slot(slot: usize) -> Self {
        Self::ALL
            .get(slot % PRESENCE_LEN)
            .copied()
            .unwrap_or(Self::Blue)
    }

    #[must_use]
    pub fn degrees(self) -> f32 {
        PRESENCE_HUES.get(self.slot()).copied().unwrap_or(265.0)
    }

    /// The stored name (`"violet"`), as settings and actions spell it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Amber => "amber",
            Self::Olive => "olive",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Cyan => "cyan",
            Self::Sky => "sky",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Magenta => "magenta",
            Self::Pink => "pink",
        }
    }

    /// The name a person reads in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Amber => "Amber",
            Self::Olive => "Olive",
            Self::Green => "Green",
            Self::Teal => "Teal",
            Self::Cyan => "Cyan",
            Self::Sky => "Sky",
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Magenta => "Magenta",
            Self::Pink => "Pink",
        }
    }

    /// Parse [`Self::id`].
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    /// The hue nearest to `degrees` (an OKLCH hue) on the ring. The toolkit
    /// reads a colour's hue and decides first whether it has one at all; a
    /// grey does not.
    #[must_use]
    pub fn nearest_to_degrees(degrees: f32) -> Self {
        let distance = |h: Self| {
            let d = (h.degrees() - degrees).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        Self::ALL
            .into_iter()
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
            .unwrap_or(Self::Blue)
    }

    /// The hue nearest an sRGB colour's OKLCH hue, or `None` for a grey
    /// (chroma under 0.02): how a roster's packed colour or a desktop accent
    /// reads as a presence hue. The `OKLab` conversion is `tilo_face`'s, which
    /// the toolkit's `ui::color::Oklch::from_color` reads a colour with too.
    #[must_use]
    pub fn nearest_to_srgb(red: u8, green: u8, blue: u8) -> Option<Self> {
        let (_, a, b) = tilo_face::linear_to_oklab(
            tilo_face::decode_srgb(red),
            tilo_face::decode_srgb(green),
            tilo_face::decode_srgb(blue),
        );
        let chroma = a.hypot(b);
        let hue = if chroma < 1e-6 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "OKLCH chroma is below 0.4 and hue in 0..360; f32 holds them well past 8-bit precision, as the toolkit's Oklch keeps them"
        )]
        let (chroma, hue) = (chroma as f32, hue as f32);
        (chroma >= 0.02).then(|| Self::nearest_to_degrees(hue))
    }

    /// The colour someone wears until they choose one: a pure function of
    /// their identity, so every host paints them the same without asking.
    /// The fixture names `me`, `alice`, `bob` and `charlie` are pinned so
    /// demos and tests stay recognisable.
    #[must_use]
    pub fn of_identity(id: &str) -> Self {
        Self::from_slot(tilo_face::presence_slot(id))
    }

    /// The person's stored choice in a settings document
    /// (`identity.colour`), if they made one.
    #[must_use]
    pub fn chosen_in(settings: &serde_json::Value) -> Option<Self> {
        settings
            .get("identity")?
            .get("colour")?
            .as_str()
            .and_then(Self::from_id)
    }

    /// The hue a person wears, as the settings document they keep says: the one
    /// they chose ([`Self::chosen_in`]), else the one their identity derives
    /// ([`Self::of_identity`]). The one lookup every surface asks, so one person
    /// is one colour, here, on the bar and in every app they edit in.
    #[must_use]
    pub fn worn_in(settings: &serde_json::Value, identity: &str) -> Self {
        Self::chosen_in(settings).unwrap_or_else(|| Self::of_identity(identity))
    }
}

/// Stable identity colour for a member who has not chosen one: the
/// presence hue [`PresenceHue::of_identity`] derives from
/// their id, as the sRGB colour in [`PEER_PALETTE`]. A pure function of the
/// id alone: two hosts painting the same workspace see different peer sets
/// (everyone but themselves), so a colour derived from who's present
/// couldn't agree across machines.
///
/// This is the identity as it travels; paint it through
/// `theme.tokens.colors.presence.of_rgb(..)`, which carries the hue at the
/// active theme's presence lightness and chroma.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    identity_rgb(PresenceHue::of_identity(user_id))
}

/// The colour a member wears until they choose one ([`PresenceHue::of_member`]), as it
/// travels: the one lookup every surface asks (the bar's faces, the People list, a profile
/// card, an agent's card in a window, its marks and carets, and the run a change is
/// attributed to), so one member is one colour everywhere.
#[must_use]
pub fn member_color(member: &tilo_action_policy::MemberId) -> (u8, u8, u8) {
    identity_rgb(PresenceHue::of_member(member))
}

/// The colour the identity `identity` (an actor's bytes as text) wears with no choice of its
/// own: the member it spells ([`member_color`]), or else the palette slot the string hashes
/// to ([`user_color`]). A person's key and an actor nobody named keep their slot; an agent's
/// change wears what the agent is, never the slot its `agent:<id>` spelling would hash to.
#[must_use]
pub fn identity_color(identity: &str) -> (u8, u8, u8) {
    match tilo_action_policy::MemberId::of_actor_bytes(identity.as_bytes()) {
        Some(member) => member_color(&member),
        None => user_color(identity),
    }
}

/// The MCP client an agent is, by the name it goes by: a run is named for its client and a number
/// of the person's runs ("Claude Code #3"), a grant or a live face for the client itself. A name
/// that only ends in a number ("Cursor 2") keeps it: only a run's `#<digits>` names a number.
#[must_use]
pub fn agent_client(name: &str) -> &str {
    name.rsplit_once(' ')
        .filter(|(_, number)| {
            number.strip_prefix('#').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .map_or(name, |(stem, _)| stem)
        .trim()
}

/// The agent `member`'s identity, with its person and the run instance its admission gave it
/// taken off: `agent:<person>/helper@host#2` is `helper@host`. One agent the person admits again
/// wears the colour of what it is, not of the run that carried it; its name still numbers the
/// runs apart.
#[must_use]
pub fn agent_identity(member: &str) -> &str {
    let bare = member.strip_prefix("agent:").unwrap_or(member);
    let bare = bare.split_once('/').map_or(bare, |(_, id)| id);
    bare.rsplit_once('#')
        .filter(|(stem, instance)| {
            !stem.is_empty() && !instance.is_empty() && instance.bytes().all(|b| b.is_ascii_digit())
        })
        .map_or(bare, |(stem, _)| stem)
}

impl PresenceHue {
    /// The hue every face of the agent `name` names wears ([`agent_client`]), so one client reads
    /// as itself at a glance wherever it shows: warm for Claude Code, cool for Codex. `None` for a
    /// client this build does not know, whose face keeps the identity's own hue.
    #[must_use]
    pub fn of_agent_client(name: &str) -> Option<Self> {
        let client = agent_client(name).to_ascii_lowercase();
        if client.contains("claude") {
            Some(Self::Orange)
        } else if client.contains("codex") {
            Some(Self::Sky)
        } else {
            None
        }
    }

    /// The hue the agent `name` names wears wherever it shows (presence, History): its client's
    /// ([`Self::of_agent_client`]), else the one its name gives it, the same for each of its runs
    /// ([`agent_identity`]).
    #[must_use]
    pub fn of_agent(name: &str) -> Self {
        let name = agent_identity(name);
        Self::of_agent_client(name).unwrap_or_else(|| Self::of_identity(agent_client(name)))
    }

    /// The hue a member wears until they choose one, from who they are and nothing else: a person
    /// the one their identity derives ([`Self::of_identity`]), software the one what it is wears
    /// ([`Self::of_agent`], the same for every run of it). The one rule and the one key every
    /// surface asks, so a member is one colour on the bar, in every app and in history.
    #[must_use]
    pub fn of_member(member: &tilo_action_policy::MemberId) -> Self {
        use tilo_action_policy::MemberId;
        match member {
            MemberId::Agent { id, .. } | MemberId::Bot(id) => Self::of_agent(id),
            MemberId::Person(person) => Self::of_identity(person.as_str()),
        }
    }

    /// The hue `actor` wears: a member's ([`Self::of_member`]), an automation's or an app's the
    /// one its id derives.
    #[must_use]
    pub fn of_actor(actor: &tilo_action_policy::Actor) -> Self {
        match actor.member_id() {
            Some(member) => Self::of_member(member),
            None => Self::of_identity(actor.id()),
        }
    }
}

/// A presence hue as the identity colour it travels as.
pub fn identity_rgb(hue: PresenceHue) -> (u8, u8, u8) {
    PEER_PALETTE.get(hue.slot()).copied().unwrap_or_default()
}

/// Every colour a peer can be drawn in:the design language's twelve
/// presence hues ([`PRESENCE_HUES`], 30° apart) at the dark
/// material's presence lightness and chroma (OKLCH L 0.74, C 0.14, gamut
/// mapped), as sRGB. Equal lightness and chroma, so no member looks
/// more important than another; a closed ring rather than a hash over all
/// 360 hues, so two people never land a few degrees apart.
pub const PEER_PALETTE: [(u8, u8, u8); 12] = [
    (0xf7, 0x85, 0x7d), // 25°  red
    (0xed, 0x91, 0x4c), // 55°  orange — pinned for "bob"
    (0xd3, 0xa3, 0x29), // 85°  amber
    (0xa8, 0xb5, 0x40), // 115° olive
    (0x6e, 0xc2, 0x72), // 145° green — pinned for "charlie"
    (0x00, 0xc7, 0xa6), // 175° teal
    (0x00, 0xc1, 0xd1), // 205° cyan
    (0x38, 0xb8, 0xf6), // 235° sky
    (0x81, 0xa8, 0xff), // 265° blue — pinned for "me"
    (0xb2, 0x98, 0xf9), // 295° violet
    (0xd7, 0x8a, 0xdb), // 325° magenta
    (0xee, 0x83, 0xaf), // 355° pink — pinned for "alice"
];

#[cfg(test)]
mod agent_client_tests {
    use super::{PresenceHue, agent_client, agent_identity};

    tilo_test_env::scenario! {
        /// The hue a person, an agent and an agent's clients wear.
        fn people_and_their_agents_wear_hues_that_stay_theirs;

        phase a_run_is_its_clients_and_a_client_has_its_hue;
        {
            assert_eq!(agent_client("Claude Code #3"), "Claude Code");
            assert_eq!(agent_client("Codex #12"), "Codex");
            assert_eq!(agent_client("Cursor 2"), "Cursor 2");
            assert_eq!(agent_client("Cursor #2 beta"), "Cursor #2 beta");
            assert_eq!(
                PresenceHue::of_agent_client("Claude Code #1"),
                Some(PresenceHue::Orange)
            );
            assert_eq!(
                PresenceHue::of_agent_client("codex"),
                Some(PresenceHue::Sky)
            );
            assert_eq!(PresenceHue::of_agent_client("Cursor"), None);
            assert_eq!(
                PresenceHue::of_agent("Jane Doe's agent #2"),
                PresenceHue::of_agent("Jane Doe's agent"),
                "every run of an agent wears one colour"
            );
        }

        phase an_admitted_agent_wears_one_colour_however_often_it_comes;
        // The id an agent's admission gives it ends in the instance number it was
        // admitted as, and that number is not part of what the agent is: the same
        // agent admitted again wears the same colour.
        {
            assert_eq!(agent_identity("agent:helper@host#1"), "helper@host");
            assert_eq!(agent_identity("agent:helper@host#2"), "helper@host");
            assert_eq!(agent_identity("helper@host"), "helper@host");
            assert_eq!(
                agent_identity("agent:claude-code@env#12"),
                "claude-code@env"
            );
            assert_eq!(agent_identity("agent:helper@host"), "helper@host");
            assert_eq!(
                agent_identity("#3"),
                "#3",
                "no stem before the number: nothing to take off"
            );
            assert_eq!(
                PresenceHue::of_agent(agent_identity("agent:helper@host#1")),
                PresenceHue::of_agent(agent_identity("agent:helper@host#4")),
                "one agent, one colour"
            );
            assert_eq!(
                PresenceHue::of_agent(agent_identity("agent:claude-code@host#9")),
                PresenceHue::Orange,
                "a known client keeps the colour it is known by"
            );
        }

        phase a_person_wears_what_they_chose_and_derives_it_until_they_do;
        // The colour a person wears is the one they chose, and the one their
        // identity derives only while they have chosen none.
        {
            let chosen = serde_json::json!({"identity": {"colour": "violet"}});
            assert_eq!(PresenceHue::worn_in(&chosen, "ada"), PresenceHue::Violet);
            for unset in [
                serde_json::json!({"identity": {"colour": null}}),
                serde_json::json!({"identity": {}}),
                serde_json::json!({}),
            ] {
                assert_eq!(
                    PresenceHue::worn_in(&unset, "ada"),
                    PresenceHue::of_identity("ada"),
                    "no choice stored: {unset}"
                );
            }
        }
    }
}
