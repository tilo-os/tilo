//! What is live now, as the device publishes it.

use tilo_resource::Effect;

/// What is live now, the one live longest first.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Live {
    pub rows: Vec<LiveItem>,
}

/// One live thing.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct LiveItem {
    /// What names the row while it is live: the resource's address, or the app and its activity.
    pub key: String,
    /// The resource that is live, by the address the session publishes it under
    /// (`tl:<app>:<id>`); empty for what this machine plays or runs that no resource holds.
    pub resource: String,
    /// The app it is of, by id.
    pub app: String,
    /// The app as the person knows it ("Music", "Clocks").
    pub source: String,
    /// It is live on this device.
    pub here: bool,
    /// What the device it is live on calls itself, when that is another device: where a track
    /// plays or a timer runs.
    pub device: String,
    /// When it went live, ISO-8601 UTC; empty where that is not known.
    pub since: String,
    /// What it is, for a row the shell draws itself (no resource of it): the song, the timer.
    pub title: String,
    /// The line under it: the artist, who speaks.
    pub subtitle: String,
    /// The app's glyph, by lucide name.
    pub icon: String,
    /// What it reads now: "4:32", "muted".
    pub value: String,
    /// It runs now, as its play-pause control says (a song playing, a timer counting down): the
    /// button shows pause.
    pub playing: bool,
    /// What the person can do to it from the shell, for a row the shell draws itself.
    pub controls: Vec<LiveControl>,
    /// The person kept it off this device's bar: quick settings still shows it.
    pub collapsed: bool,
    /// What kind of thing it is, for a row the shell draws itself: `call`, `navigation`, `timer`,
    /// `stopwatch`, `music`, `picture` or `other`. Empty for a live resource, whose own view says.
    pub kind: String,
    /// How far along it is, from 0 to 1, where it has a length.
    pub progress: Option<f64>,
    /// Where it is and what is left of it ("1:12", "-2:48"), where its length is known: the person
    /// can then take it anywhere along it (`seek_live`).
    pub elapsed: String,
    pub remaining: String,
    /// Its artwork, by picture address.
    pub artwork: String,
    /// Its app asks for it to stand over the windows as a card while it runs, not only on the bar.
    pub floats: bool,
    /// The person pulled it out of the bar's pill: on a phone-width display its card drops from
    /// under the bar.
    pub expanded: bool,
    /// It stands as a card over the windows now: its app asks for that or the person pulled it
    /// out, and they did not fold it back. At most three do, the ones touched last.
    pub card: bool,
    /// Where its card stands in the corner column: 0 nearest the corner, the one touched last.
    pub place: i64,
    /// Where the person left its card, in pixels from its display's top-leading corner; none
    /// docks it in the corner column.
    pub x: Option<i64>,
    pub y: Option<i64>,
    /// Its app draws its card itself (a route, a call's video): the shell's card holds a block
    /// `drawn_width` by `drawn_height` the app's own window stands over.
    pub drawn: bool,
    pub drawn_width: f64,
    pub drawn_height: f64,
}

/// A control of a row the shell draws itself.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct LiveControl {
    /// What its app calls it, pressed back to it as it is.
    pub id: String,
    /// What the button says.
    pub label: String,
    /// The glyph it shows, by lucide name.
    pub icon: String,
    /// What it does in a transport, whatever its app calls it: `play-pause`, `previous` or `next`;
    /// empty for any other control.
    pub transport: String,
    /// It ends what is live: a card carries it in its header, beside the way back to the bar.
    pub ends: bool,
}

/// What the rows offer beside their reading: each action is the resource's own and names the row
/// it acts on by its key, since the reading publishes its rows as data, not as resources.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.live")]
pub enum LiveEffect {
    /// Press a control of something live here that no resource holds: a player's play or pause,
    /// a Rust app's timer's stop
    #[effect(kind = "outside")]
    #[effect(label = "session-live-press")]
    PressLive {
        /// The row's key, as the reading lists it.
        key: String,
        /// The control's id.
        control: String,
    },
    /// End something live here that no resource holds: an app that offers a close is asked to
    /// end it, and one that cannot be stopped from outside is put away until it shows something
    /// else. A live resource ends by its type's own action.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-close")]
    EndLive {
        /// The row's key, as the reading lists it.
        key: String,
    },
    /// Keep a row off this device's bar, or bring it back: quick settings shows it either way.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-collapse")]
    KeepOffBar {
        /// The row's key, as the reading lists it.
        key: String,
        /// Whether it stays off the bar; it does when not said.
        off: Option<bool>,
    },
    /// Pull a row out of the bar's pill as a card over the windows; on a phone-width display it
    /// drops from under the bar.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-expand")]
    ExpandLive {
        /// The row's key, as the reading lists it.
        key: String,
    },
    /// Fold a row's card back into the bar's pill.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-fold")]
    FoldLive {
        /// The row's key, as the reading lists it.
        key: String,
    },
    /// Take something live here that no resource holds to a point along it: the song the player
    /// here plays, an app's activity that has a length.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-seek")]
    SeekLive {
        /// The row's key, as the reading lists it.
        key: String,
        /// Where along it, from 0 to 1.
        fraction: f64,
    },
    /// Leave a row's card at a place of its own on its display, or dock it in the corner column
    /// again when no place is said.
    #[effect(kind = "outside")]
    #[effect(label = "session-live-move")]
    MoveLive {
        /// The row's key, as the reading lists it.
        key: String,
        /// Its top-leading corner `[x, y]`, in pixels from the display's: where a dragged card
        /// was let go.
        at: Option<Vec<i64>>,
    },
}

/// The rows as a view of another language reads them (`device.live`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Live, LiveEffect>("live", "Live")
}
