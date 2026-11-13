//! What an app keeps going outside its own window: a running timer, a call, a route, a song, a picture
//! in a picture. An app describes its activity as a [`Summary`]: a title, the value it reads (a
//! countdown, an elapsed time, who is on the line), how far along it is, up to three controls and a
//! tone. The session draws that one shape as the bar's pill and as a floating card, so the two never
//! disagree and neither has to know what the activity is.

use serde::{Deserialize, Serialize};

/// The most controls a summary names: what fits a card's row, and what the bar's pill and keyboard
/// reach.
pub const MAX_CONTROLS: usize = 4;

/// What an activity is. It decides where the activity sits in the bar's pill and whether it wants a
/// card of its own.
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
    /// Where an activity of this kind stands in the pill, sooner first: a call before directions, a
    /// timer that is nearly due before one that is not, and a song last.
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

    /// Whether an activity of this kind is a card of its own while it runs (a call, directions) rather
    /// than only once it is pulled out of the pill (music).
    #[must_use]
    pub const fn asks_for_a_card(self) -> bool {
        matches!(self, Self::Call | Self::Navigation | Self::PictureInPicture)
    }

    /// The lucide glyph that stands for this kind of activity, for a card whose app names no icon of
    /// its own.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Call => "phone",
            Self::Navigation => "navigation",
            Self::Timer => "timer",
            Self::Stopwatch => "clock",
            Self::Music => "music",
            Self::PictureInPicture => "monitor",
            Self::Other => "layers",
        }
    }
}

/// How loudly an activity reads: its glyph on the card, its reading in the pill.
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

impl Tone {
    /// The ink an activity of this tone reads in, wherever it is drawn: the bar's pill and the card's
    /// glyph say the same thing about it.
    #[must_use]
    pub fn ink(self, theme: &crate::theme::Theme) -> vello::peniko::Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Neutral => colors.text_primary,
            Self::Accent => colors.intent.accent,
            Self::Alert => colors.semantic.danger,
        }
    }
}

/// How an activity's artwork heads its card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Art {
    /// A square tile beside the lines, as a song's cover stands beside its title and artist.
    #[default]
    Square,
    /// The card's whole width at nine sixteenths above the lines: a picture the activity made, such as
    /// a screenshot, shown as its own hero.
    Hero,
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
    /// Ends what runs, in the colour that warns: a recording's stop.
    Destructive,
}

/// One control the activity offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    /// What the app calls it: its own command id, pressed back to the app as it is.
    pub id: String,
    /// What the button says.
    pub label: String,
    #[serde(default)]
    pub kind: ControlKind,
    /// The glyph the button shows instead of its label, by lucide name ("play", "skip-back"): a media
    /// control reads as its icon, and its label is still what assistive technology says.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

impl Control {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: ControlKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            icon: String::new(),
        }
    }

    /// The same control drawn as `icon`, by lucide name.
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// What this control does in a media transport, when it is one of the transport's three: read from
    /// its glyph first, so play and pause say what the player does now, then from its id.
    #[must_use]
    pub fn transport(&self) -> Option<crate::widget::TransportControl> {
        use crate::widget::TransportControl;
        match self.icon.as_str() {
            "skip-back" | "skip-backward" | "rewind" => Some(TransportControl::Previous),
            "skip-forward" | "forward" => Some(TransportControl::Next),
            "play" => Some(TransportControl::PlayPause { playing: false }),
            "pause" => Some(TransportControl::PlayPause { playing: true }),
            _ => match self.id.as_str() {
                "previous" | "back" => Some(TransportControl::Previous),
                "next" => Some(TransportControl::Next),
                _ => None,
            },
        }
    }
}

/// One activity, as the bar's pill and the floating layer draw it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// The activity's own id, stable while it runs: the same timer or call keeps its id across frames,
    /// so a card keeps its place and its dismissal.
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    /// What it is, in the app's words: "Tea", "Ada", "9th Street".
    pub title: String,
    /// The line under it: the artist of a song, who is speaking, the street the directions turn onto.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    /// The app's own glyph, by lucide name ("timer", "music"): a card's header shows it, so a glance
    /// says which app the activity is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// The app the activity belongs to, as the person knows it ("Music", "Clocks"): a card's header
    /// says it beside the app's glyph. The session that shows the activity fills it, so it is named in
    /// the person's language and never as an id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// What it reads now: "4:32", "2 min", "muted".
    #[serde(default)]
    pub value: String,
    /// What is left, in seconds, for a countdown: a timer due sooner stands ahead of one that is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown: Option<i64>,
    /// How far along it is, from 0 to 1, when it has a length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// How long what it plays runs, in seconds: with `progress`, a card says where the song is and what
    /// is left of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// Its artwork, by picture address (a content address, or a `data:` picture): a card shows it
    /// beside the title, the pill's a thumbnail.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artwork: String,
    /// How the artwork heads the card: beside the lines, or over them.
    #[serde(default)]
    pub art: Art,
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
            subtitle: String::new(),
            icon: String::new(),
            source: String::new(),
            value: value.into(),
            countdown: None,
            progress: None,
            length: None,
            artwork: String::new(),
            art: Art::Square,
            tone: Tone::Neutral,
            controls: Vec::new(),
        }
    }

    /// The line under the title: the artist, the speaker, the street.
    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// The app's own glyph, by lucide name.
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// The app it belongs to, as the person knows it: a card's header says it beside the app's glyph.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// The glyph a card's header shows: the app's own, or the one its kind stands for.
    #[must_use]
    pub fn glyph(&self) -> &str {
        if self.icon.is_empty() {
            self.kind.glyph()
        } else {
            &self.icon
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

    /// How long it runs, in seconds, so a card can say where it is and what is left of it.
    #[must_use]
    pub fn with_length(mut self, seconds: u64) -> Self {
        self.length = Some(seconds);
        self
    }

    /// The artwork it shows, by picture address.
    #[must_use]
    pub fn with_artwork(mut self, artwork: impl Into<String>) -> Self {
        self.artwork = artwork.into();
        self
    }

    /// The artwork heads the card over its lines, at nine sixteenths of the card's width: a picture the
    /// activity made is shown whole, not as a tile beside a title.
    #[must_use]
    pub fn with_hero_artwork(mut self, artwork: impl Into<String>) -> Self {
        self.artwork = artwork.into();
        self.art = Art::Hero;
        self
    }

    /// Where the activity stands in what it runs, in seconds, from how far along it is: `None` when it
    /// has no length.
    #[must_use]
    pub fn elapsed_seconds(&self) -> Option<u64> {
        let length = self.length?;
        let progress = f64::from(self.progress.unwrap_or(0.0).clamp(0.0, 1.0));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a recording's length in seconds is far inside u64"
        )]
        Some((progress * length as f64).round() as u64)
    }

    /// What is left of it, in seconds, when it has a length.
    #[must_use]
    pub fn remaining_seconds(&self) -> Option<u64> {
        let length = self.length?;
        Some(length.saturating_sub(self.elapsed_seconds().unwrap_or(0)))
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

    /// The words the pill and a card's header say: the title, and its reading when it has one.
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

    /// What a card shows largest: the reading when it has one, else the title.
    #[must_use]
    pub fn hero(&self) -> &str {
        if self.value.is_empty() {
            &self.title
        } else {
            &self.value
        }
    }

    /// The title a card's header says above its hero, when the hero is not the title itself.
    #[must_use]
    pub fn header_title(&self) -> Option<&str> {
        (!self.value.is_empty() && !self.title.is_empty()).then_some(self.title.as_str())
    }

    /// What a card's header says beside the app's glyph: the title, when the hero is the card's
    /// reading, else the app the activity belongs to.
    #[must_use]
    pub fn header_line(&self) -> &str {
        self.header_title().unwrap_or(&self.source)
    }

    /// Where this activity stands in the pill: its kind first, then a timer that is due within a
    /// minute, then what it has left, then its id, so the order never depends on the order the board
    /// was read in.
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

    /// The control a plain press (Space, a click on the card) runs: the first one the app marked
    /// primary, else its first.
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

    /// Whether the card draws a row of offered controls: every control but the close, which its header
    /// carries instead.
    #[must_use]
    pub fn has_actions(&self) -> bool {
        self.controls
            .iter()
            .any(|control| control.kind != ControlKind::Close)
    }

    /// The controls the card's row draws, in the order the app named them: everything but the close,
    /// which the card's header carries instead.
    #[must_use]
    pub fn action_controls(&self) -> Vec<&Control> {
        self.controls
            .iter()
            .filter(|control| control.kind != ControlKind::Close)
            .take(MAX_CONTROLS.saturating_sub(1))
            .collect()
    }

    /// The activity's offered controls as a media transport, when every one of them is the transport's
    /// — previous, play/pause, next: the card then paints them the one way every media surface paints a
    /// transport, by id.
    #[must_use]
    pub fn transport(&self) -> Option<Vec<(&str, crate::widget::TransportControl)>> {
        let controls = self.action_controls();
        if controls.is_empty() {
            return None;
        }
        controls
            .iter()
            .map(|control| control.transport().map(|step| (control.id.as_str(), step)))
            .collect()
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

/// What an app publishes on the live activity board: the activity it keeps going, and an announcement
/// it wants made.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Published {
    /// The activity running now, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    /// The person keeps this one over the windows: it is a card while it runs, not only a pill in the
    /// bar.
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
