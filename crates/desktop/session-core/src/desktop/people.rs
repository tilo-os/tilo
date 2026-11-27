//! Who is here, as the roster reads them: one row per person and agent in the workspace, what
//! they are doing and who follows whom. The shell draws it.

use ui::peniko::Color;

mod roster;
pub use roster::{PeopleNames, in_app, roster_rows};

/// One row of the roster.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    pub id: String,
    pub label: String,
    pub initial: char,
    pub color: Color,
    /// Who they are: a person or an agent.
    pub member: tilo_action_policy::MemberId,
    pub role: Option<String>,
    /// Where they are: the app or document in front of them.
    pub activity: Option<String>,
    /// The window of this workspace a piece of software is working in: its tab, and what that tab is
    /// called here. Software has no screen to mirror, so its row shows this instead, and pressing it
    /// brings that window forward.
    pub working_in: Option<(u64, String)>,
    /// The app that window runs, by its id (what names the place software works in beside its
    /// line, without naming the app it already sits in).
    pub working_app: Option<String>,
    /// Short member intention, marked by its [`LineSource`] so model-suggested words cannot be mistaken for a direct quote.
    pub status: Option<RosterStatus>,
    pub followable: bool,
    /// How available they say they are (idle and away come on their own).
    pub availability: ui::Availability,
    /// When the workspace admitted them, Unix milliseconds: for someone nobody named, the one
    /// fact that tells their row from every other unnamed person's.
    pub joined: Option<u64>,
    /// The joined line their row shows, once the roster's unnamed rows are known: the clock they
    /// joined by, with milliseconds where two of them would otherwise read alike.
    pub joined_line: Option<String>,
    /// The voice channel they are in, if any.
    pub call: Option<String>,
}

impl RosterRow {
    /// Software, not a person: an agent.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        !self.member.is_person()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterStatus {
    pub text: String,
    pub source: LineSource,
}

/// Who wrote a line shown about someone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSource {
    /// The person typed it.
    Written,
    /// A model (or another generator) wrote it from what they did.
    Generated,
    /// Plainly read off what they did ("Editing Welcome in Notes").
    Observed,
}
