//! Who is here, as the device publishes it: every member of the workspace in front who is here
//! now, the person at this device, whom they follow and whom the bar has to tell them about.
//!
//! The roster is the session's reading of presence and the workspace's member rows, published as
//! `session:people` for a shell in another language to draw. What each person's row says: their
//! name, the colour the workspace gives them, where they are (the window they are in, or the app
//! when this shell shows no window of it), what they say they are doing (marked where their device
//! wrote it for them), how available they are,
//! the call they are in, whom they follow and when they joined, whether this person follows them
//! and whether they follow this person. Software is a member too: an agent's or a bot's row says
//! which it is, and an agent's says the window it works in. What the row's words are is the shell's,
//! so each language says them its own way; the reading carries the facts.

use serde::{Deserialize, Serialize};
use tilo_resource::Effect;

/// Who is here, as `session:people` reads it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct People {
    /// Everyone here in the workspace in front, people first and software after.
    pub rows: Vec<Person>,
    /// The person at this device.
    pub me: Person,
    /// Whose card the roster shows: a person's presence key, `self` for the person here, or empty
    /// for the roster itself.
    pub showing: String,
    /// Whom this person follows, by their presence key; empty for nobody.
    pub following: String,
    /// The person whose view this was: the person's own input just ended following them, and the
    /// bar says "Stopped following" while this holds. Empty otherwise.
    pub stopped: String,
    /// Who waved at this person just now: their name here, while the bar still says it.
    pub waved: String,
    /// Whom this person waved at just now, by that person's presence key, while their row still
    /// says the wave went.
    pub waved_to: String,
    /// Who arrived where this person is, by their name here, while the bar still says it.
    pub arrived: Vec<String>,
    /// The first time in a public workspace, what its people see of the person; empty otherwise.
    pub arrived_seen: String,
    /// This person may invite someone into the workspace in front, as the loop last answered it:
    /// its owner's grant decides.
    pub may_invite: bool,
    /// The presence colours a person may wear, in the order their card offers them: the twelve
    /// hues a face is drawn in, each by the id a choice names it with and the colour it paints.
    #[serde(default)]
    pub colours: Vec<PresenceColour>,
    /// What the person's own card offers, in the order it shows them: where the person's settings
    /// are, what the host can do about them, and their other devices.
    #[serde(default)]
    pub actions: Vec<PersonAction>,
    /// The host's action that opens the picture picker, where it offers one: the id the card's
    /// face and its "Change picture" row name; empty when it has none.
    #[serde(default)]
    pub photo_change: String,
    /// The host's action that takes the person's picture away again; empty when it has none.
    #[serde(default)]
    pub photo_remove: String,
    /// Whether the desktop's control hints show, where this host offers the choice; `None` where
    /// it has none.
    #[serde(default)]
    pub control_hints: Option<bool>,
}

/// One presence colour a person may wear: the id a choice names it by (`red` … `pink`), and the
/// colour the card's swatch paints, `rrggbb`.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct PresenceColour {
    pub id: String,
    pub colour: String,
}

/// One of the host's actions about the person, as their own card offers it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct PersonAction {
    /// The action's id, which choosing it names to the host.
    pub id: String,
    /// What it is called, in the person's language.
    pub title: String,
    /// Its glyph, by the platform icon's kebab-case name.
    pub icon: String,
    /// Where the card shows it: `settings` at the card's top, `identity` among what the person is
    /// seen as, or `devices` with the person's other devices.
    pub group: String,
}

/// One of the devices a person is here on: the key a press names it by (never shown as its name),
/// what the device calls itself, whether it is the one at this desk, and how available its person
/// is there.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct PersonDevice {
    pub id: String,
    /// What the device calls itself; empty on a device that named none.
    pub label: String,
    /// This is the device the roster is drawn on.
    pub this: bool,
    /// How available their person is there: `available`, `focusing`, `away` or `idle`.
    pub availability: String,
}

/// One of someone's accounts elsewhere, as the workspace's services name it: the service they are
/// linked to it on, and the handle their profile there goes by.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct PersonLinked {
    pub service: String,
    /// The handle their profile there goes by, as the service spells it.
    pub handle: String,
    /// The service's mark, by the platform icon's kebab-case name.
    pub icon: String,
}

/// Someone here in the workspace in front, a person or a piece of software.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one fact presence says of someone, read apart by a view"
)]
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Person {
    /// Their presence key, hex: what a follow, a wave or a card is asked for.
    pub key: String,
    /// Their name here; empty for someone nobody named yet.
    pub name: String,
    /// The colour the workspace gives them, `rrggbb`.
    pub colour: String,
    /// They are software: an agent or one of the workspace's bots.
    pub agent: bool,
    /// They are one of the workspace's bots: software with an installation of its own, where an
    /// agent is a run under someone here.
    pub bot: bool,
    /// This is the person at this device.
    pub me: bool,
    /// How available they say they are: `available`, `focusing`, `away` or `idle`.
    pub availability: String,
    /// What they say they are doing, in their own words: empty when they say nothing.
    pub doing: String,
    /// Their device read `doing` off the steps they took, rather than anyone saying it: theirs to
    /// see, never theirs to edit in their card's own field. A view marks such words as software's,
    /// never showing them as if they had said them.
    #[serde(default)]
    pub automatic: bool,
    /// The window of this workspace they are in, as this shell names it: empty when they are in
    /// none it shows.
    pub activity: String,
    /// The app of that window, or the app they are in when this shell shows no window of it.
    pub in_app: String,
    /// The name voice gives them where they are: their voice channel, empty when none.
    pub call: String,
    /// Whom they follow, by that person's presence key, `self` for the person at this device; empty
    /// when they follow nobody here.
    pub follows: String,
    /// The clock they joined the workspace by, in the person's own time format, to the second (with
    /// milliseconds where another unnamed person joined in the same second): the one fact that
    /// tells someone nobody named apart. Empty for someone with a name.
    pub joined: String,
    /// This person follows them.
    pub following: bool,
    /// They follow this person.
    pub follower: bool,
    /// A conversation with them can be opened: Chat is on this device and the key a conversation
    /// is kept on is known here.
    #[serde(default)]
    pub can_message: bool,
    /// The name they gave themselves; empty while they gave none, so a card asks for one rather
    /// than holding the words said in place of it. Meaningful for the person at this device.
    #[serde(default)]
    pub given: String,
    /// The presence colour they chose, by its id (`violet`); empty while they wear the one their
    /// identity derives.
    #[serde(default)]
    pub colour_id: String,
    /// Which of [`People::colours`] their face is drawn in now, counted from 0: the colour they
    /// chose, else the one their identity derives.
    #[serde(default)]
    pub colour_slot: Option<u32>,
    /// The devices they are here on, each with how available they are there.
    #[serde(default)]
    pub devices: Vec<PersonDevice>,
    /// Their accounts at the services this workspace is connected to.
    #[serde(default)]
    pub linked: Vec<PersonLinked>,
    /// An agent's client, by the name its card in Console is addressed by; empty for a person.
    #[serde(default)]
    pub agent_client: String,
    /// The root key they are here by, in hex, once the host heard it: what a card shows short and
    /// copies whole, and what names the record a conversation with them is kept on.
    #[serde(default)]
    pub user_key: String,
}

/// What the roster and a person's card ask of the session.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.people")]
pub enum PeopleEffect {
    /// Open who is here, or close it when it is up
    #[effect(screen, idempotent, label = "people-show")]
    Show,
    /// Close who is here
    #[effect(screen, idempotent, label = "people-close")]
    Close,
    /// Show a person's card in the roster: their devices, what they are doing and the actions they
    /// offer. An empty key comes back from a card to the roster
    #[effect(screen, label = "people-show-person")]
    ShowPerson {
        /// The person, by their presence key
        #[param("The person, as the roster names them")]
        person: String,
    },
    /// Follow a person: the bar keeps their view in front, and says whose view it is
    #[effect(kind = "outside", label = "people-follow")]
    Follow {
        /// The person, by their presence key
        #[param("The person, as the roster names them")]
        person: String,
    },
    /// Stop following whoever this person follows
    #[effect(kind = "outside", label = "people-unfollow")]
    Unfollow,
    /// Wave at a person: their bar says so for a moment
    #[effect(kind = "outside", label = "people-wave")]
    Wave {
        /// The person, by their presence key
        #[param("The person, as the roster names them")]
        person: String,
    },
    /// Open this person's conversation with someone in Chat
    #[effect(kind = "outside", label = "people-message")]
    Message {
        /// The person, by their presence key
        #[param("The person, as the roster names them")]
        person: String,
    },
    /// Go to where a person works: their window of this workspace, or their app
    #[effect(kind = "outside", label = "people-go-to")]
    GoTo {
        /// The person, by their presence key
        #[param("The person, as the roster names them")]
        person: String,
    },
    /// Give this person's own card the name everyone here sees
    #[effect(kind = "undoable", label = "people-name")]
    SetName {
        /// The name
        #[param("The name everyone here sees")]
        name: String,
    },
    /// Say what this person is doing, in their own words; empty stops saying it
    #[effect(kind = "undoable", label = "people-doing")]
    SetDoing {
        /// What they are doing
        #[param("What they are doing, in their own words")]
        text: String,
    },
    /// Say how available this person is: `available`, `focusing`, `away` or `idle`
    #[effect(kind = "undoable", label = "people-availability")]
    SetAvailability {
        /// How available they are
        #[param("How available they are: `available`, `focusing`, `away` or `idle`")]
        availability: String,
    },
    /// Add another of this person's devices: the link its camera reads, as the session's own sheet
    /// shows it
    #[effect(screen, label = "people-pair-device")]
    PairDevice,
    /// Ask this of an agent through the card it keeps in Console: stop it, undo its run, pick up
    /// where it was, take back what let it in, or open its card
    #[effect(kind = "outside", label = "people-agent")]
    Agent {
        /// The agent, by their presence key
        #[param("The agent, as the roster names them")]
        person: String,
        /// The client it is, by the name its card in Console is addressed by
        #[param("The agent's client, as its card in Console is addressed")]
        client: String,
        /// What is asked of it: `stop`, `undo-run`, `take-over`, `revoke`, or `card` for the card
        /// itself
        #[param("What is asked of the agent: `stop`, `undo-run`, `take-over`, `revoke` or `card`")]
        ask: String,
    },
    /// Wear this presence colour: the one the reading's `colours` lists at this place
    #[effect(kind = "undoable", label = "people-colour")]
    Colour {
        /// The colour's place among the reading's `colours`, counted from 0
        #[param("The colour's place among the colours the card offers, counted from 0")]
        slot: u32,
    },
    /// Do one of the host's own actions about this person, by id, as their own card offers them
    #[effect(kind = "outside", label = "people-host-action")]
    HostAction {
        /// The action's id, as the card's actions carry it
        #[param("The action, as the card's actions name it")]
        id: String,
    },
    /// Show the desktop's control hints, or not: the person's own record, which every device of
    /// theirs reads
    #[effect(kind = "undoable", label = "people-control-hints")]
    ControlHints {
        /// Whether the hints show
        #[param("Whether the hints show")]
        on: bool,
    },
    /// Keep what the person typed in their own card once they pause, at the same place Enter and
    /// closing the card keep it: each change the field sends is kept once typing stops, so nothing
    /// typed is lost for want of a Save
    #[effect(screen, label = "people-draft")]
    Draft {
        /// What was typed into: `name` or `doing`
        #[param("The field that was typed into: `name` or `doing`")]
        field: String,
        /// What the field holds now
        #[param("What the field holds now")]
        text: String,
    },
    /// Open a page of this person's own settings: their accounts, their devices and what they share
    #[effect(screen, label = "people-settings")]
    OpenSettings {
        /// The settings page, as a settings link (`settings://devices_access`); the person's own
        /// page when empty
        #[param("The settings page, as `settings://devices_access`; the person's own when empty")]
        link: Option<String>,
    },
}

/// The people resource as a view of another language reads it (`device.people`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<People, PeopleEffect>("people", "People")
}
