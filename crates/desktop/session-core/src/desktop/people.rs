//! Who is here, as the roster reads them: one row per person and agent in the workspace, what
//! they are doing and who follows whom. The shell draws it.

use crate::desktop::i18n::{t, t_args};
use ui::peniko::Color;

mod roster;
pub use roster::{PeopleNames, in_app, roster_rows, spoken_details};

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
    /// The voice channel of theirs this device may join from their row: one they are in and this person is not.
    pub join: Option<VoiceChannel>,
}

impl RosterRow {
    /// Software, not a person: an agent.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        !self.member.is_person()
    }
}

/// One voice channel's call, by the app, record and channel that hold it: what joining it asks of the app that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChannel {
    pub app: String,
    pub record: String,
    pub channel: String,
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

/// Say on each follower's row whom they follow, from `pairs` of `(follower, followed)` roster ids: "Following Ada", or "Following you" when they follow this person (`own_id`).
pub fn note_following(rows: &mut [RosterRow], pairs: &[(String, String)], own_id: Option<&str>) {
    for (follower, followed) in pairs {
        // `None` for this person: "you".
        let person = if own_id == Some(followed.as_str()) {
            None
        } else if let Some(row) = rows.iter().find(|row| &row.id == followed) {
            Some(row.label.clone())
        } else {
            continue;
        };
        let Some(row) = rows.iter_mut().find(|row| &row.id == follower) else {
            continue;
        };
        let place = row
            .activity
            .take()
            .filter(|activity| !activity.trim().is_empty());
        row.activity = Some(match (place, person) {
            (None, None) => t("people-follows-you"),
            (None, Some(person)) => t_args("people-follows", &[("person", person.into())]),
            (Some(place), None) => t_args("people-follows-you-where", &[("place", place.into())]),
            (Some(place), Some(person)) => t_args(
                "people-follows-where",
                &[("place", place.into()), ("person", person.into())],
            ),
        });
    }
}

/// The line under a person's name saying what they are doing: a call they are in, else what they wrote or are doing, else how available they are.
#[must_use]
pub fn doing_line(row: &RosterRow) -> Option<String> {
    if let Some(call) = &row.call {
        return Some(if call.trim().is_empty() {
            t("people-panel-in-a-call")
        } else {
            t_args("people-panel-in-call", &[("channel", call.clone().into())])
        });
    }
    if let Some(status) = &row.status {
        return Some(status.text.clone());
    }
    match row.availability {
        ui::Availability::Available => None,
        ui::Availability::Focusing => Some(t("people-panel-focusing")),
        ui::Availability::Away => Some(t("people-panel-away")),
        ui::Availability::Idle => Some(t("people-panel-idle")),
    }
}

/// When someone nobody named joined: the one fact a person can tell them apart and recognise
/// them by, since their name and face are every unnamed person's. Never a device's name or a key.
/// The roster fills it once it knows its other unnamed rows ([`roster_rows_named`]).
#[must_use]
pub fn joined_line(row: &RosterRow) -> Option<String> {
    row.joined_line.clone()
}

/// The roster's header line: who is here besides this person, counted as the list is — the others,
/// never this person, who is the "you" of the words ("2 here" over one row reads as someone
/// missing).
#[must_use]
pub fn here_line(others: usize) -> String {
    t_args("people-panel-here-count", &[("count", others.into())])
}

/// Whether the line under someone's name is software's words about them (a model's, or the host's reading of what they did), not theirs: it carries the automatic mark.
#[must_use]
pub fn says_automatically(row: &RosterRow) -> bool {
    row.call.is_none()
        && row.status.as_ref().is_some_and(|status| {
            status.source != LineSource::Written && !status.text.trim().is_empty()
        })
}

#[cfg(test)]
mod tests {
    use super::here_line;

    /// The header counts who the list shows, besides this person.
    #[test]
    fn the_header_counts_the_people_listed() {
        ui::i18n::I18nManager::init("en-US");
        assert_eq!(here_line(0), "Just you");
        assert_eq!(here_line(1), "You and 1 other");
        assert_eq!(here_line(3), "You and 3 others");
    }
}
