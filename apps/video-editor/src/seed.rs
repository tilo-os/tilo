//! The demo project the editor opens populated with, and the document it
//! is stored in.

use ui::tilo_resource::{Doc, Fields};
use uuid::Uuid;

use crate::media;
use crate::model::{
    Clip, EffectKind, Filters, MediaItem, MediaKind, Project, Track, TrackKind, Transition,
};

/// A new record's document holding `project`, as its first content.
#[must_use]
pub fn document_of(project: &Project) -> Doc {
    let mut doc = Doc::new();
    let _ = project.write_fields(&mut doc, &automerge::ROOT);
    doc.commit();
    doc
}

/// The demo project's document.
#[must_use]
pub fn sample_document() -> Doc {
    document_of(&sample_project())
}

/// Named parameters for a deterministic demo clip placement. Source and
/// timeline times sit beside normalized values, so positional arguments would
/// make accidental transposition far too easy.
struct DemoPlacement {
    track: Uuid,
    media: Uuid,
    start: f64,
    source: std::ops::Range<f64>,
    palette: u8,
    opacity: f64,
    volume: f64,
    transition: Transition,
    filters: Filters,
    fade_in: f64,
    fade_out: f64,
}

impl DemoPlacement {
    fn new(
        track: Uuid,
        media: Uuid,
        start: f64,
        source: std::ops::Range<f64>,
        palette: u8,
    ) -> Self {
        Self {
            track,
            media,
            start,
            source,
            palette,
            opacity: 1.0,
            volume: 1.0,
            transition: Transition::None,
            filters: Filters::default(),
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }

    fn transition(mut self, transition: Transition) -> Self {
        self.transition = transition;
        self
    }

    fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    fn volume(mut self, volume: f64) -> Self {
        self.volume = volume;
        self
    }

    fn filters(mut self, filters: Filters) -> Self {
        self.filters = filters;
        self
    }

    fn fades(mut self, fade_in: f64, fade_out: f64) -> Self {
        self.fade_in = fade_in;
        self.fade_out = fade_out;
        self
    }

    fn into_clip(self) -> Clip {
        Clip {
            id: Uuid::new_v4(),
            track: self.track,
            media: self.media,
            start: self.start,
            in_point: self.source.start,
            out_point: self.source.end,
            opacity: self.opacity,
            volume: self.volume,
            transition: self.transition,
            palette: self.palette,
            filters: self.filters,
            fade_in: self.fade_in,
            fade_out: self.fade_out,
            transition_duration: 0.0,
            effects: ui::tilo_resource::Ordered::new(),
            ..Default::default()
        }
    }
}

struct DemoMedia {
    drone: Uuid,
    interview: Uuid,
    market: Uuid,
    sunset: Uuid,
    logo: Uuid,
    voiceover: Uuid,
    pad: Uuid,
}

struct DemoTracks {
    video_main: Uuid,
    video_overlay: Uuid,
    audio_voiceover: Uuid,
    audio_bed: Uuid,
}

fn demo_clips(media: &DemoMedia, tracks: &DemoTracks) -> Vec<Clip> {
    let graded = Filters {
        brightness: 0.62,
        contrast: 0.64,
        saturation: 0.58,
    };
    let placements = [
        DemoPlacement::new(tracks.video_main, media.drone, 0.0, 2.0..10.0, 0)
            .transition(Transition::FadeIn)
            .filters(graded)
            .fades(1.5, 0.0),
        DemoPlacement::new(tracks.video_main, media.interview, 8.0, 5.0..17.0, 1)
            .transition(Transition::CrossDissolve),
        DemoPlacement::new(tracks.video_main, media.market, 20.0, 0.0..6.0, 2).fades(0.0, 1.0),
        DemoPlacement::new(tracks.video_overlay, media.logo, 1.0, 0.0..4.0, 4)
            .transition(Transition::FadeIn)
            .opacity(0.9),
        DemoPlacement::new(tracks.video_overlay, media.sunset, 21.0, 0.0..6.0, 3)
            .transition(Transition::CrossDissolve)
            .opacity(0.85)
            .fades(0.0, 1.2),
        DemoPlacement::new(tracks.audio_voiceover, media.voiceover, 8.0, 1.5..13.5, 5)
            .fades(0.3, 0.5),
        DemoPlacement::new(tracks.audio_bed, media.pad, 0.0, 0.0..28.0, 6)
            .transition(Transition::FadeIn)
            .volume(0.4)
            .fades(2.0, 3.0),
    ];
    let mut clips: Vec<_> = placements
        .into_iter()
        .map(DemoPlacement::into_clip)
        .collect();

    // Stacked effects make the demo exercise the effects panel as well as
    // timeline placement and transitions.
    if let Some(drone) = clips.first_mut() {
        drone.add_effect(EffectKind::Warm);
        drone.add_effect(EffectKind::Vignette);
    }
    clips
}

/// A short documentary-style cut: drone B-roll under an interview, an
/// overlaid logo and a sunset insert with a dissolve, narration on A1 and
/// an ambient bed on A2. Built deterministically so screenshots are
/// stable across runs.
pub fn sample_project() -> Project {
    let mut p = Project::new("Coastline_Doc_v3");
    // A cut this far along has a format: it is 1080p at 30, and the next clip
    // to land on it must not re-shape the timeline it is already cut against.
    p.set_frame_size(1920, 1080);
    p.set_fps(30.0);

    let mk = |name: &str, kind: MediaKind, dur: f64, w: u32, h: u32, fps: f64, pal: u8| {
        MediaItem {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            // Sample media stands in for footage that was never probed.
            has_audio: false,
            duration: dur,
            width: w,
            height: h,
            fps,
            palette: pal,
            // Sample media lives under the workspace media prefix; the
            // bundled sample-media/ dir hydrates these for --shots.
            path: media::workspace_path(name),
            text: None,
        }
    };

    let drone = mk(
        "Drone_Coastline.mov",
        MediaKind::Video,
        24.0,
        3840,
        2160,
        24.0,
        0,
    );
    let interview = mk(
        "Interview_A.mp4",
        MediaKind::Video,
        38.5,
        1920,
        1080,
        30.0,
        1,
    );
    let market = mk(
        "Cutaway_Market.mp4",
        MediaKind::Video,
        12.0,
        1920,
        1080,
        30.0,
        2,
    );
    let sunset = mk(
        "Sunset_Timelapse.mp4",
        MediaKind::Video,
        9.0,
        3840,
        2160,
        30.0,
        3,
    );
    let logo = mk("Logo_Bug.png", MediaKind::Image, 6.0, 800, 800, 30.0, 4);
    let vo = mk("VO_Take3.wav", MediaKind::Audio, 32.0, 0, 0, 0.0, 5);
    let pad = mk("Ambient_Pad.wav", MediaKind::Audio, 60.0, 0, 0, 0.0, 6);

    let media = DemoMedia {
        drone: drone.id,
        interview: interview.id,
        market: market.id,
        sunset: sunset.id,
        logo: logo.id,
        voiceover: vo.id,
        pad: pad.id,
    };
    p.media = vec![drone, interview, market, sunset, logo, vo, pad].into();

    let mk_track = |name: &str, kind: TrackKind, index: u32| Track {
        id: Uuid::new_v4(),
        name: name.into(),
        kind,
        index,
        muted: false,
        locked: false,
        solo: false,
        gain_db: 0.0,
        pan: 0.0,
    };
    let v1 = mk_track("V1", TrackKind::Video, 1);
    let v2 = mk_track("V2", TrackKind::Video, 2);
    let a1 = mk_track("A1", TrackKind::Audio, 1);
    let a2 = mk_track("A2", TrackKind::Audio, 2);
    let tracks = DemoTracks {
        video_main: v1.id,
        video_overlay: v2.id,
        audio_voiceover: a1.id,
        audio_bed: a2.id,
    };
    p.tracks = vec![v1, v2, a1, a2].into();
    p.clips = demo_clips(&media, &tracks).into();

    p
}
