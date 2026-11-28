//! [`TrackTimeline`] as a described component: the tracks, spans and markers a view hands it, and
//! the changes a person's hand on it reports back.

use tilo_ui::widget::Ink;
use tilo_ui_base::{component::Component, component::Described, component::Event};

use super::widget::{Marker, Span, Track, TrackTimeline};

/// A time ruler over rows of tracks, with what stands on each track as spans, the markers a person
/// set on the ruler and the playhead: a film's cut, a sound's layers, a show's timing. Dragging a
/// span moves it along its track or onto another, dragging its edge trims it, and both line up
/// with the grid, the other spans' edges, the markers and the playhead; dragging along the ruler
/// moves the playhead. The wheel and a pinch zoom the lane about the pointer and scroll it along
/// the time, and a bar under the tracks shows how much of the cut the lane holds and moves it.
/// Each is a typed event, and the timeline never changes what it shows: the view commits the
/// change, and the span settles into the place the view gives it.
#[derive(Component)]
#[component(name = "TrackTimeline", role = "Group", label = "label")]
pub struct TrackTimelineProps {
    /// What the timeline is, for assistive technology: "The cut".
    pub label: Option<String>,
    /// How many seconds the lane shows across its width: the zoom.
    pub span: f64,
    /// The moment the lane's leading edge shows.
    pub scroll: f64,
    /// Where the playhead stands, in seconds.
    pub playhead: f64,
    /// How long what is on the tracks runs, in seconds: where zooming to fit and the end key go.
    pub length: f64,
    /// The moments a measurement found a problem at — where the mix went over full scale — marked
    /// on the ruler and down the tracks, so the flag stands where it happened.
    pub clips: Vec<f64>,
    /// The grid a drag lands on, in seconds: one frame (1/30) for a film, nothing for free time.
    pub step: f64,
    /// The tracks, top to bottom, each with what stands on it.
    #[component(part(default))]
    pub tracks: Vec<TimelineTrackProps>,
    /// The moments marked on the ruler.
    #[component(part)]
    pub markers: Vec<TimelineMarkerProps>,
    /// Where other members are at work in the cut, as the view read them from `spots(p)`:
    /// where each plays from, and the stretch each selected.
    pub peers: Vec<tilo_ui::widget::PeerSpot>,
    /// The playhead is being dragged: carries where it is now, as the hand moves.
    #[component(event(value = f64))]
    pub scrub: Event,
    /// The playhead was set down: the ruler pressed, the playhead let go, or a key moved it.
    /// Carries where it stands.
    #[component(event(value = f64))]
    pub seek: Event,
    /// The person zoomed: carries how many seconds the lane shows now.
    #[component(event(value = f64))]
    pub zoomed: Event,
    /// The person scrolled along the time: carries the moment the leading edge shows now.
    #[component(event(value = f64))]
    pub scrolled: Event,
    /// A track's empty stretch was pressed: carries the moment and the track's key.
    #[component(event(value = f64, at = String))]
    pub press: Event,
    /// Whether a press on a track runs now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the label does not say (an action whose requirement
    /// does not hold): spoken as its description beside its name.
    pub description: Option<String>,
}

impl Default for TrackTimelineProps {
    fn default() -> Self {
        Self {
            label: None,
            span: 20.0,
            scroll: 0.0,
            playhead: 0.0,
            length: 0.0,
            clips: Vec::new(),
            step: 0.0,
            tracks: Vec::new(),
            markers: Vec::new(),
            peers: Vec::new(),
            scrub: Event::None,
            seek: Event::None,
            zoomed: Event::None,
            scrolled: Event::None,
            press: Event::None,
            enabled: true,
            description: None,
        }
    }
}

/// One row of the timeline: its name in the header and the spans on it. A toggle for muting,
/// soloing or locking it shows in the header when the view handles that toggle's event; a locked
/// track's spans hold still.
#[derive(Component, Default)]
#[component(name = "TimelineTrack", role = "Group", label = "label")]
pub struct TimelineTrackProps {
    /// The track's stable key.
    #[component(key)]
    pub key: String,
    /// What the track stands for, as the view names it: what a span moved onto it reports.
    /// Without it, the track's key.
    #[component(data)]
    pub item: Option<String>,
    /// What the track is called: "V1", "Music".
    pub label: Option<String>,
    /// Whether it is muted: its spans are drawn faded.
    pub muted: bool,
    /// Whether it plays alone.
    pub solo: bool,
    /// Whether it is locked: its spans cannot be dragged, and nothing can be dragged onto it.
    pub locked: bool,
    /// What stands on the track, in any order.
    #[component(part(default))]
    pub spans: Vec<TimelineSpanProps>,
    /// The mute toggle was pressed: carries whether the track is to be muted.
    #[component(event(value = bool))]
    pub toggled_mute: Event,
    /// The solo toggle was pressed: carries whether the track is to play alone.
    #[component(event(value = bool))]
    pub toggled_solo: Event,
    /// The lock toggle was pressed: carries whether the track is to be locked.
    #[component(event(value = bool))]
    pub toggled_lock: Event,
}

tilo_ui_base::part_type!(TimelineTrackProps);

/// One thing standing on a track for a stretch of time: a clip, a sound, a title. It shows its
/// name, its colour, a strip of its frames and the shape of its sound when the view gives them.
#[derive(Component, Default)]
#[component(name = "TimelineSpan", role = "ListItem", label = "label")]
pub struct TimelineSpanProps {
    /// The span's stable key.
    #[component(key)]
    pub key: String,
    /// What the span stands for, as the view names it: what its press reports. Without it, the
    /// span's key.
    #[component(data)]
    pub item: Option<String>,
    /// What it is called.
    pub label: Option<String>,
    /// When it starts, in seconds.
    #[component(required)]
    pub start: f64,
    /// How long it lasts, in seconds.
    #[component(required)]
    pub length: f64,
    /// The colour it takes from the theme.
    pub tone: Ink,
    /// Whether it is among what the person chose: drawn raised and outlined.
    pub selected: bool,
    /// Pictures of its frames, by address, laid edge to edge across it.
    pub frames: Vec<String>,
    /// Its loudness, one peak from 0 to 1 each equal stretch, drawn as its waveform.
    pub peaks: Vec<f64>,
    /// Moments inside it that are marked, in seconds from its start: its keyframes.
    pub keys: Vec<f64>,
    /// How long the transition into it lasts, in seconds from its start.
    pub lead_in: f64,
    /// It was pressed, by the pointer or by Enter or Space with it focused: carries its place
    /// among its track's spans, which a view reads as a `host.Press` with the modifier keys held.
    #[component(event(value = i64))]
    pub pressed: Event,
    /// It was dragged along its track or onto another: carries the moment it starts at now and
    /// what the track it stands on stands for.
    #[component(event(value = f64, at = String))]
    pub moved: Event,
    /// Its leading or trailing edge was dragged: carries the moment it starts at now and how
    /// long it lasts.
    #[component(event(value = f64, at = f64))]
    pub trimmed: Event,
}

tilo_ui_base::part_type!(TimelineSpanProps);

/// A moment marked on the ruler: a flag with its name, which a person drags along the time.
#[derive(Component, Default)]
#[component(name = "TimelineMarker", role = "Button", label = "label")]
pub struct TimelineMarkerProps {
    /// The marker's stable key.
    #[component(key)]
    pub key: String,
    /// What it is called.
    pub label: Option<String>,
    /// The moment it marks, in seconds.
    #[component(required)]
    pub at: f64,
    /// The colour it takes from the theme.
    pub tone: Ink,
    /// It was pressed.
    #[component(event)]
    pub pressed: Event,
    /// It was dragged along the ruler: carries the moment it marks now.
    #[component(event(value = f64))]
    pub moved: Event,
}

tilo_ui_base::part_type!(TimelineMarkerProps);

fn tracks(props: Vec<TimelineTrackProps>) -> Vec<Track> {
    props
        .into_iter()
        .map(|track| Track {
            key: track.item.unwrap_or(track.key),
            label: track.label.unwrap_or_default(),
            muted: track.muted,
            solo: track.solo,
            locked: track.locked,
            toggled_mute: track.toggled_mute,
            toggled_solo: track.toggled_solo,
            toggled_lock: track.toggled_lock,
            spans: track
                .spans
                .into_iter()
                .map(|span| Span {
                    key: span.item.unwrap_or(span.key),
                    label: span.label.unwrap_or_default(),
                    start: span.start.max(0.0),
                    length: span.length.max(0.0),
                    tone: span.tone,
                    selected: span.selected,
                    frames: span.frames.into_iter().filter(|f| !f.is_empty()).collect(),
                    peaks: span.peaks,
                    keys: span.keys,
                    lead_in: span.lead_in.max(0.0),
                    pressed: span.pressed,
                    moved: span.moved,
                    trimmed: span.trimmed,
                })
                .collect(),
        })
        .collect()
}

fn markers(props: Vec<TimelineMarkerProps>) -> Vec<Marker> {
    props
        .into_iter()
        .map(|marker| Marker {
            key: marker.key,
            label: marker.label.unwrap_or_default(),
            at: marker.at.max(0.0),
            tone: marker.tone,
            pressed: marker.pressed,
            moved: marker.moved,
        })
        .collect()
}

impl Described for TrackTimeline {
    type Props = TrackTimelineProps;

    fn from_props(_key: &str, props: TrackTimelineProps) -> Self {
        let mut timeline = Self::new(tracks(props.tracks), markers(props.markers));
        timeline.label = props.label;
        timeline.span = props.span.max(0.01);
        timeline.scroll = props.scroll.max(0.0);
        timeline.playhead = props.playhead.max(0.0);
        timeline.length = props.length.max(0.0);
        timeline.clips = props.clips;
        timeline.step = props.step.max(0.0);
        timeline.peers = props.peers;
        timeline.scrub = props.scrub;
        timeline.seek = props.seek;
        timeline.zoomed = props.zoomed;
        timeline.scrolled = props.scrolled;
        timeline.enabled = props.enabled;
        timeline.description = props.description;
        timeline.press = if props.enabled {
            props.press
        } else {
            Event::None
        };
        timeline
    }

    fn set_props(&mut self, props: TrackTimelineProps) -> bool {
        self.retarget(tracks(props.tracks), markers(props.markers));
        self.label = props.label;
        self.span = props.span.max(0.01);
        self.scroll = props.scroll.max(0.0);
        self.playhead = props.playhead.max(0.0);
        self.length = props.length.max(0.0);
        self.clips = props.clips;
        self.step = props.step.max(0.0);
        self.peers = props.peers;
        self.scrub = props.scrub;
        self.seek = props.seek;
        self.zoomed = props.zoomed;
        self.scrolled = props.scrolled;
        self.enabled = props.enabled;
        self.description = props.description;
        self.press = if props.enabled {
            props.press
        } else {
            Event::None
        };
        true
    }
}
