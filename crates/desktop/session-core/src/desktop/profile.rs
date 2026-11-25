//! What a person's card says: the person themselves, someone else in the workspace, or an agent,
//! with the groups of actions the card offers and the choices a person makes on it. The shell's
//! card draws this; nothing here measures or paints.

use ui::icons::Icon;

use crate::desktop::launchpad::AgentAsk;
use ui::vello::peniko::Color;

pub use tilo_app::linked_accounts::{LinkState, LinkedAccount};

/// What a profile action is about, so the card can group them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileGroup {
    /// The card's way into Settings: its top, under the person's name and picture, where someone
    /// scanning the card for it looks first.
    Settings,
    /// Being with the people here: saying something beside the pointer.
    Together,
    /// The person as others see them: name, picture, what they are doing.
    Identity,
    /// The person's other devices.
    Devices,
}

/// One of the host's commands about the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileAction {
    pub id: String,
    pub title: String,
    pub group: ProfileGroup,
    /// What it is about at a glance: a name, a picture, a device.
    pub icon: Icon,
}

/// Who the card is about, as the host knows it when it opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileModel {
    /// The name everyone here sees.
    pub name: String,
    /// The name the person gave themselves, as their own field starts with: blank while they gave
    /// none, even where `name` stands in the words said for a person without one. `None` where
    /// `name` is already the one they gave.
    pub given_name: Option<String>,
    /// The stable identity the avatar's colour and initials derive from when the person chose no
    /// colour.
    pub identity: String,
    pub colour: Option<Color>,
    pub can_rename: bool,
    /// The person has not named themselves yet (`name` is their login account's): the card asks,
    /// once, for the name people here should see, until they give one.
    pub ask_name: bool,
    pub actions: Vec<ProfileAction>,
    /// The services the workspace is connected to, with the person's account at each; none hides
    /// the section.
    pub linked: Vec<LinkedAccount>,
    /// How available the person says they are.
    pub availability: ui::Availability,
    /// The short status they wrote, if any.
    pub status: String,
    /// Whether what they are doing is shared on its own, only in their words, or not at all; `None`
    /// where the host keeps no such choice.
    pub sharing: Option<ui::StatusSharing>,
    /// The colour they chose, by its id; `None` while their identity's own colour is worn.
    pub colour_id: Option<String>,
    /// What the people in the workspace on screen see of them; `None` outside any shared workspace.
    pub sight: Option<WorkspaceSight>,
    /// The host keeps the person's status, colour and what people see: the card offers them.
    pub offers_presence: bool,
    /// Whether the desktop's control hints show; `None` where the host has none to offer.
    pub control_hints: Option<bool>,
    /// The other person's root key (hex).
    pub key: Option<String>,
    /// The devices the other person is here on.
    pub devices: Vec<crate::desktop::here::PersonDevice>,
    /// The card is about someone else: what they say they are doing and what this person can do
    /// with them. `None` for the person's own card.
    pub other: Option<OtherPerson>,
}

/// Someone else, as their card shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherPerson {
    /// Their roster id (the device their presence comes from).
    pub id: String,
    /// What they are doing: their own words, or a line software wrote,
    /// which the card marks as automatic.
    pub status: Option<crate::desktop::people::RosterStatus>,
    /// Where they are ("In Chat"), when it adds to the status.
    pub place: Option<String>,
    /// How they said they are here; `None` for an agent, which says nothing of it.
    pub presence: Option<ui::Availability>,
    /// This person follows their view now.
    pub following: bool,
    /// The host can follow them (bring forward what they are in).
    pub can_follow: bool,
    /// The host can open the conversation this person has with them: their root key is known and
    /// the app that holds it is here.
    pub can_message: bool,
    /// Their accounts elsewhere, as their profile names them.
    pub linked: Vec<tilo_app::linked_accounts::ProfileLink>,
    /// They are an agent: the card offers what can be done with it, each through its card in
    /// Console. `None` for a person.
    pub agent: Option<AgentFacts>,
}

/// An agent as its face's card shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFacts {
    /// The member it is: its id and its person. Its mark wears this member's colour.
    pub member: tilo_action_policy::MemberId,
    /// The client it is, by the name it goes by ("Claude Code"): what its card in Console is
    /// addressed by.
    pub client: String,
}

/// The host actions about the person's picture, which the card offers beside their avatar rather
/// than in its list of identity actions.
pub const SET_PHOTO: &str = "set-photo";

pub const RESET_PHOTO: &str = "reset-photo";

/// A root key shown short: its first eight and last four characters.
#[must_use]
pub fn short_key(key: &str) -> String {
    let key = key.trim();
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 14 {
        return key.to_owned();
    }
    let head: String = chars.iter().take(8).collect();
    let tail: String = chars.iter().skip(chars.len().saturating_sub(4)).collect();
    format!("{head}…{tail}")
}

/// What the people in one workspace see of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSight {
    /// The workspace's name.
    pub workspace: String,
    pub visibility: ui::PresenceVisibility,
}

/// What the person did in the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileChoice {
    /// Go by this name from now on (trimmed, never empty).
    Rename(String),
    /// The host's command, by id.
    Action(String),
    /// Link the person's account with this connector's service (its app id). The card stays open
    /// for the code.
    Link(String),
    /// Unlink the person's account with this connector's service.
    Unlink(String),
    /// Link the account this connector's service offers.
    Accept(String),
    /// Turn that offer down.
    Decline(String),
    Availability(ui::Availability),
    /// Say this short status (empty clears it).
    Status(String),
    /// Share what you are doing this way from now on.
    Sharing(ui::StatusSharing),
    /// Wear this colour, by its id.
    Colour(String),
    /// Let the people in the workspace on screen see this much.
    Visibility(ui::PresenceVisibility),
    /// Show the desktop's control hints, or not.
    ControlHints(bool),
    /// Follow this person (roster id), or stop following them.
    Follow(String),
    /// Open the conversation this person has with them in Chat (root key,
    /// hex), on Chat's row for it, its composer ready.
    Message(String),
    /// Go to where this person (roster id) works: the window of theirs shown here, else their app.
    GoTo(String),
    /// Ask this of the agent of this client (`AgentFacts::client`).
    Agent {
        client: String,
        ask: AgentAsk,
    },
    /// Go from this person to everyone here: the roster in the card's place.
    EveryoneHere,
}

impl ProfileChoice {
    /// Whether the card closes on it: a linked account's choice keeps it open to show what came of
    /// it.
    #[must_use]
    pub fn closes(&self) -> bool {
        matches!(
            self,
            Self::Action(_)
                | Self::Follow(_)
                | Self::GoTo(_)
                | Self::Message(_)
                | Self::Agent { .. }
                | Self::EveryoneHere
        )
        // The rest are settings the card goes on showing. A name is one of them: it is kept
        // while the person works, and a card that shut itself the moment typing paused would
        // take the status, the colour and the way back out with it.
    }
}
