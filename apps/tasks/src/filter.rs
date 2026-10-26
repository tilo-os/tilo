//! What the list and the board show: every issue, or the ones a filter
//! picks out.
//!
//! A filter is view state, never stored: the sidebar offers the ones that
//! read a tracker as a person works through it — everything, mine, what
//! nobody has taken, what an agent is working on, and one per label in use —
//! and the list and the board both narrow to it. An issue shows when it
//! matches, and so does an issue whose sub-issue matches, so the tree stays
//! whole down to what was asked for.

use std::collections::BTreeSet;

use ui::tilo_resource::Id;

use crate::model::{Issue, Tracker};

/// Which issues the list and the board show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    /// Every issue.
    #[default]
    All,
    /// The ones assigned to the person reading.
    Mine,
    /// The ones nobody has taken.
    Unassigned,
    /// The ones an agent is working on.
    Agent,
    /// The ones carrying this label.
    Label(String),
}

impl Filter {
    /// The filters the sidebar offers, before the labels in use.
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Mine, Self::Unassigned, Self::Agent];

    /// The sidebar row's name.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::All => crate::tr("tasks-filter-all"),
            Self::Mine => crate::tr("tasks-filter-mine"),
            Self::Unassigned => crate::tr("tasks-filter-unassigned"),
            Self::Agent => crate::tr("tasks-filter-agent"),
            Self::Label(label) => label.clone(),
        }
    }

    /// The row's icon.
    pub(crate) const fn icon(&self) -> ui::icons::Icon {
        match self {
            Self::All => ui::icons::Icon::ListTree,
            Self::Mine => ui::icons::Icon::User,
            Self::Unassigned => ui::icons::Icon::CircleDashed,
            Self::Agent => ui::icons::Icon::Bot,
            Self::Label(_) => ui::icons::Icon::Tag,
        }
    }

    /// The sidebar row's key.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::All => "filter:all".to_owned(),
            Self::Mine => "filter:mine".to_owned(),
            Self::Unassigned => "filter:unassigned".to_owned(),
            Self::Agent => "filter:agent".to_owned(),
            Self::Label(label) => format!("filter:label:{label}"),
        }
    }

    /// The filter a sidebar row named `key` stands for, if it is one.
    pub(crate) fn of(key: &str) -> Option<Self> {
        Some(match key {
            "filter:all" => Self::All,
            "filter:mine" => Self::Mine,
            "filter:unassigned" => Self::Unassigned,
            "filter:agent" => Self::Agent,
            other => Self::Label(other.strip_prefix("filter:label:")?.to_owned()),
        })
    }

    /// Whether this issue itself matches, read as `me`'s window.
    pub(crate) fn matches(&self, issue: &Issue, me: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Mine => me.is_some_and(|me| issue.assigned_person() == Some(me)),
            Self::Unassigned => issue.assignee.is_none(),
            Self::Agent => issue.assigned_agent().is_some(),
            Self::Label(label) => issue.labels.iter().any(|carried| carried == label),
        }
    }

    /// Whether the issue shows: it matches, or something under it does.
    pub(crate) fn shows(
        &self,
        tracker: &Tracker,
        id: &Id,
        issue: &Issue,
        me: Option<&str>,
    ) -> bool {
        self.matches(issue, me) || self.shows_below(tracker, id, me)
    }

    fn shows_below(&self, tracker: &Tracker, id: &Id, me: Option<&str>) -> bool {
        tracker
            .children(Some(id))
            .any(|(child, issue)| self.shows(tracker, child, issue, me))
    }
}

/// Every label the tracker's issues carry, in the order a person reads
/// them, for the sidebar's labels.
pub(crate) fn labels(tracker: &Tracker) -> Vec<String> {
    let all: BTreeSet<&str> = tracker
        .issues
        .values()
        .flat_map(|issue| issue.labels.iter())
        .map(String::as_str)
        .collect();
    all.into_iter().map(str::to_owned).collect()
}
