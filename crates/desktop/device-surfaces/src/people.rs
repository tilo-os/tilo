//! Who is here, as the device publishes it: every member of the workspace in front who is here
//! now, the person at this device, whom they follow and whom the bar has to tell them about.
//!
//! The roster is the session's reading of presence and the workspace's member rows, published as
//! `session:people` for a shell in another language to draw. What each person's row says: their
//! name, the colour the workspace gives them, where they are (the window they are in, or the app
//! when this shell shows no window of it), what they say they are doing, how available they are,
//! the call they are in, whether this person follows them and whether they follow this person.
//! Software is a member too: an agent's row says the window it works in. What the row's words
//! are is the shell's, so each language says them its own way; the reading carries the facts.

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
    /// Who waved at this person just now: their name here, while the bar still says it.
    pub waved: String,
    /// Who arrived where this person is, by their name here, while the bar still says it.
    pub arrived: Vec<String>,
    /// The first time in a public workspace, what its people see of the person; empty otherwise.
    pub arrived_seen: String,
}

/// Someone here in the workspace in front, a person or a piece of software.
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
    /// This is the person at this device.
    pub me: bool,
    /// How available they say they are: `available`, `focusing`, `away` or `idle`.
    pub availability: String,
    /// What they say they are doing, in their own words: empty when they say nothing.
    pub doing: String,
    /// The window of this workspace they are in, as this shell names it: empty when they are in
    /// none it shows.
    pub activity: String,
    /// The app of that window, or the app they are in when this shell shows no window of it.
    pub in_app: String,
    /// The name voice gives them where they are: their voice channel, empty when none.
    pub call: String,
    /// This person follows them.
    pub following: bool,
    /// They follow this person.
    pub follower: bool,
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
