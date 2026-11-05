//! What an app keeps going outside its own window: a running timer, a call, a
//! route, a song, a picture in a picture.
//!
//! An app describes its activity as a [`Summary`]: a title, the value it reads
//! (a countdown, an elapsed time, who is on the line), how far along it is, up
//! to three controls and a tone. The session draws that one shape as the bar's
//! pill and as a floating card, so the two never disagree and neither has to
//! know what the activity is. Rich bodies (a call's video, a live route) are
//! drawn by their app inside the card frame; the summary is still what names,
//! prioritises and closes them.
//!
//! The summary crosses from an app to the session as JSON, so every field is
//! optional but the title and kind: an older app's activity still shows.

use serde::{Deserialize, Serialize};

/// The most controls a summary names: what fits a card's row, and what the
/// bar's pill and keyboard reach.
pub const MAX_CONTROLS: usize = 3;

/// What an activity is. It decides where the activity sits in the bar's pill
/// and whether it wants a card of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A conversation with someone, running now.
    Call,
    /// Turn-by-turn directions.
    Navigation,
    /// A timer counting down.
    Timer,
    /// A stopwatch counting up.
    Stopwatch,
    /// Something playing.
    Music,
    /// Part of a window shown on its own (picture in picture).
    PictureInPicture,
    /// Anything else.
    #[default]
    Other,
}

impl Kind {
    /// Where an activity of this kind stands in the pill, sooner first: a call
    /// before directions, a timer that is nearly due before one that is not,
    /// and a song last.
    #[must_use]
    pub const fn pill_rank(self) -> u8 {
        match self {
            Self::Call => 0,
            Self::Navigation => 1,
            Self::Timer => 2,
            Self::Stopwatch => 3,
            Self::Music => 4,
            Self::PictureInPicture => 5,
            Self::Other => 6,
        }
    }

    /// Whether an activity of this kind is a card of its own while it runs
    /// (a call, directions) rather than only once it is pulled out of the
    /// pill (music).
    #[must_use]
    pub const fn asks_for_a_card(self) -> bool {
        matches!(self, Self::Call | Self::Navigation | Self::PictureInPicture)
    }
}

/// How loudly an activity reads: its bar in the card, and the pill's dot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Running as usual.
    #[default]
    Neutral,
    /// The person's own, in their colour.
    Accent,
    /// Nearly due, or asking for attention.
    Alert,
}

/// What pressing a control does, as the card draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// The activity's main action, drawn as the card's primary button.
    #[default]
    Primary,
    /// A second action, drawn plainly.
    Secondary,
    /// Ends the activity: the card's close, or the pill's dismiss.
    Close,
}

/// One control the activity offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    /// What the app calls it: its own command id, pressed back to the app as
    /// it is.
    pub id: String,
    /// What the button says.
    pub label: String,
    #[serde(default)]
    pub kind: ControlKind,
}

impl Control {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: ControlKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
        }
    }
}

/// One activity, as the bar's pill and the floating layer draw it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// The activity's own id, stable while it runs: the same timer or call
    /// keeps its id across frames, so a card keeps its place and its
    /// dismissal.
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    /// What it is, in the app's words: "Tea", "Ada", "9th Street".
    pub title: String,
    /// What it reads now: "4:32", "2 min", "muted".
    #[serde(default)]
    pub value: String,
    /// What is left, in seconds, for a countdown: a timer due sooner stands
    /// ahead of one that is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown: Option<i64>,
    /// How far along it is, from 0 to 1, when it has a length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(default)]
    pub tone: Tone,
    #[serde(default)]
    pub controls: Vec<Control>,
}

impl Summary {
    /// An activity of `kind` called `title`, reading `value`.
    #[must_use]
    pub fn new(kind: Kind, title: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            kind,
            title: title.into(),
            value: value.into(),
            countdown: None,
            progress: None,
            tone: Tone::Neutral,
            controls: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    #[must_use]
    pub fn with_countdown(mut self, seconds: i64) -> Self {
        self.countdown = Some(seconds);
        self
    }

    #[must_use]
    pub fn with_progress(mut self, progress: f32) -> Self {
        self.progress = Some(progress.clamp(0.0, 1.0));
        self
    }

    #[must_use]
    pub fn with_tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub fn with_control(mut self, control: Control) -> Self {
        self.controls.push(control);
        self
    }

    /// The words the pill and a card's header say: the title, and its reading
    /// when it has one.
    #[must_use]
    pub fn reading(&self) -> String {
        if self.value.is_empty() {
            return self.title.clone();
        }
        if self.title.is_empty() {
            return self.value.clone();
        }
        format!("{}  {}", self.title, self.value)
    }

    /// Where this activity stands in the pill: its kind first, then a timer
    /// that is due within a minute, then what it has left, then its id, so
    /// the order never depends on the order the board was read in.
    #[must_use]
    pub fn pill_rank(&self) -> (u8, i64, &str) {
        let due_soon = self
            .countdown
            .is_some_and(|remaining| (0..=60).contains(&remaining));
        let rank = match (self.kind, due_soon) {
            (Kind::Timer, true) => 1,
            (kind, _) => kind.pill_rank(),
        };
        // A timer that is not due soon sorts by what it has left; everything
        // else sorts by its id alone.
        let left = match self.kind {
            Kind::Timer => self.countdown.unwrap_or(i64::MAX),
            _ => 0,
        };
        (rank, left, self.id.as_str())
    }

    /// The control a plain press (Space, a click on the card) runs: the first
    /// one the app marked primary, else its first.
    #[must_use]
    pub fn first_control(&self) -> Option<&Control> {
        self.controls
            .iter()
            .find(|control| control.kind == ControlKind::Primary)
            .or_else(|| self.controls.first())
    }

    /// The control that ends the activity, when the app offers one.
    #[must_use]
    pub fn close_control(&self) -> Option<&Control> {
        self.controls
            .iter()
            .find(|control| control.kind == ControlKind::Close)
    }

    /// Whether the card draws a row of offered controls: every control but
    /// the close, which its header carries instead.
    #[must_use]
    pub fn has_actions(&self) -> bool {
        self.controls
            .iter()
            .any(|control| control.kind != ControlKind::Close)
    }

    /// The controls the card's row draws, in the order the app named them:
    /// everything but the close, which the card's header carries instead.
    #[must_use]
    pub fn action_controls(&self) -> Vec<&Control> {
        self.controls
            .iter()
            .filter(|control| control.kind != ControlKind::Close)
            .take(MAX_CONTROLS.saturating_sub(1))
            .collect()
    }

    /// At most [`MAX_CONTROLS`], in the order the card draws them: the close
    /// last, so it never sits where a hurried press aimed at the main action.
    #[must_use]
    pub fn shown_controls(&self) -> Vec<&Control> {
        let mut controls = self.action_controls();
        if let Some(close) = self.close_control() {
            controls.push(close);
        }
        controls
    }
}

/// A timer or an alarm that came due, said as a notification.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Stable identity of the due event: it is announced once.
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// What an app publishes on the live activity board: the activity it keeps
/// going, and an announcement it wants made.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Published {
    /// The activity running now, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    /// The person keeps this one over the windows: it is a card while it runs,
    /// not only a pill in the bar.
    #[serde(default)]
    pub overlay: bool,
    /// Something that came due and wants announcing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<Announcement>,
}

impl Published {
    /// Only what reads now: the activity, with nothing else.
    #[must_use]
    pub fn of(summary: Summary) -> Self {
        Self {
            summary: Some(summary),
            overlay: false,
            announce: None,
        }
    }

    /// A due event with nothing running: the announcement alone.
    #[must_use]
    pub fn announcing(announce: Announcement) -> Self {
        Self {
            summary: None,
            overlay: false,
            announce: Some(announce),
        }
    }
}

#[cfg(test)]
#[path = "live_activity_tests.rs"]
mod tests;
