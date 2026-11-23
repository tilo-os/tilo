//! [`VideoPlayer`] as a described component: a video surface with playback controls.

use media::Source;

use crate::component::{Component, Described, Event, Value};

use super::{NowPlaying, PlaybackState, PlayerSetting, PlayerSettingEvent, VideoPlayer};
use crate::widget::{DrawChild, PictureInPictureAsk, PictureInPictureProvider};

/// A video with its playback controls over it: play/pause, the scrubber, the volume, mute and
/// fullscreen. The app's player is the one truth; what the person does reaches the view as its
/// events.
///
/// The video's bytes are a camera, a capture, or the medium it names (a file or a stream),
/// which the toolkit opens on the engine its app hands in.
#[derive(Component, Default)]
#[component(name = "VideoPlayer", role = "Group", label = "title")]
pub struct VideoPlayerProps {
    /// The video's title, for assistive technology.
    pub title: Option<String>,
    /// A still shown before the first frame: a picture by its address.
    #[component(data)]
    pub poster: Option<String>,
    /// Whether it plays.
    pub playing: bool,
    /// Where it is, in seconds.
    pub position: Option<f64>,
    /// How long it is, in seconds.
    pub duration: Option<f64>,
    /// Jump here, in seconds: a one-shot control, applied when `seek_to_seq` changes, so a
    /// bookmark's jump, a restart or a jump to the end reaches the player once and the resting
    /// `position` never fights the `progress` reading.
    pub seek_to: Option<f64>,
    /// Counts the view's jumps: a number it raises each time it asks to jump.
    pub seek_to_seq: Option<i64>,
    /// Where it is stored, as a `file://` address or a URL the engine fetches.
    #[component(data)]
    pub source: Option<String>,
    /// The volume, `0..=1`.
    pub volume: Option<f64>,
    /// Whether it is muted.
    pub muted: bool,
    /// The speed it plays at, `0.25..=4`; absent at normal speed.
    pub rate: Option<f64>,
    /// Whether it begins again when it ends.
    pub repeat: bool,
    /// The audio track by its index in `audio_tracks`; absent leaves the engine's choice.
    pub audio_track: Option<i64>,
    /// The subtitle track by its index in `subtitle_tracks`; `-1` turns subtitles off.
    pub subtitle_track: Option<i64>,
    /// Play or pause was pressed: carries `true` when it plays.
    #[component(event(value = bool))]
    pub set_playing: Event,
    /// It was moved to a place in the video: carries `0..=1`.
    #[component(event(value = f64))]
    pub seek: Event,
    /// The volume was set: carries `0..=1`.
    #[component(event(value = f64))]
    pub set_volume: Event,
    /// The player could not show its video: carries what the engine said.
    #[component(event(value = String))]
    pub fail: Event,
    /// Where playback got to while it played, as a share of its length (`0..=1`): reported on the
    /// first frame it plays, then every five seconds, and once more when it stops, so a view
    /// remembers the place a video was left at even when nobody moved the playhead.
    #[component(event(value = f64))]
    pub progress: Event,
    /// How long it runs, in seconds, once the engine has read it.
    #[component(event(value = f64))]
    pub length: Event,
    /// The audio tracks' labels, in the order `audio_track` indexes them.
    #[component(event(value = Vec<String>))]
    pub audio_tracks: Event,
    /// The subtitle tracks' labels, in the order `subtitle_track` indexes them.
    #[component(event(value = Vec<String>))]
    pub subtitle_tracks: Event,
    /// The chapters' labels, in the order `seek` indexes them.
    #[component(event(value = Vec<String>))]
    pub chapters: Event,
    /// The engine the player runs the video on, which its app builds.
    #[component(skip)]
    pub engine: Option<media::MediaEngine>,
}

/// A video player driven from a view: its picture follows what the view declares, and its
/// controls report the view's events.
/// The player's gear-menu control that asks for picture-in-picture.
pub const PICTURE_IN_PICTURE: &str = "picture_in_picture";

pub struct DeclaredVideo {
    player: VideoPlayer,
    playing: bool,
    set_playing: Event,
    seek: Event,
    fail: Event,
    progress: Event,
    length: Event,
    audio_tracks: Event,
    subtitle_tracks: Event,
    chapters: Event,
    /// What the player last told the view, so only a reading that moved is reported.
    reported: Reported,
    /// The address the player was opened at: a view that answers with another one for the same
    /// video (what asking the service again does) opens that one.
    source: Option<String>,
    /// The controls the view asks for now, and the ones the engine has.
    wanted: Applied,
    applied: Applied,
    /// Whether it begins again when it ends.
    repeat: bool,
    /// The jump the view asks for, and the sequence number it asked with.
    jump: Option<(f64, i64)>,
    /// Whether the person pressed Picture-in-picture since the host last asked.
    pip: std::cell::Cell<bool>,
    /// The sound the player decoded since the host last took it, for the host to play in every
    /// place the session's outputs name.
    audio: std::cell::RefCell<Vec<i16>>,
}

/// How far playback moves between the progress reports a view keeps: five seconds of video, so a
/// video watched without touching the playhead is remembered where it was left.
const PROGRESS_STEP_MS: u64 = 5_000;

/// How far the player's own playhead may stand from the place a view declares before it is moved
/// there: a view that declares the place it was just told is not moved back to it.
const SEEK_TOLERANCE_MS: u64 = 1_000;

/// The readings last sent to the view.
#[derive(Default)]
struct Reported {
    /// Where the view was last told playback stands, in milliseconds: a share is reported again
    /// only when a five-second step passed, or when playback stopped.
    progress_ms: Option<u64>,
    duration_ms: Option<u64>,
    audio: Option<Vec<String>>,
    subtitles: Option<Vec<String>>,
    chapters: Option<Vec<String>>,
}

/// The engine controls last applied from the view's props.
#[derive(Default, Clone, Copy)]
struct Applied {
    rate: Option<f64>,
    audio: Option<i64>,
    subtitles: Option<i64>,
    seek_to_seq: Option<i64>,
}

impl DeclaredVideo {
    /// The player the props describe.
    #[must_use]
    pub fn new(props: VideoPlayerProps) -> Self {
        let VideoPlayerProps {
            title,
            poster,
            playing,
            position,
            duration,
            seek_to,
            seek_to_seq,
            source,
            volume,
            muted,
            rate,
            repeat,
            audio_track,
            subtitle_track,
            set_playing,
            seek,
            set_volume,
            fail,
            progress,
            length,
            audio_tracks,
            subtitle_tracks,
            chapters,
            engine,
        } = props;
        let _ = set_volume;
        let mut player = VideoPlayer::new();
        if let Some(engine) = engine {
            player.set_engine(engine);
        }
        // The player's own controls are the shared ones; picture-in-picture is one of them,
        // offered to every view that draws a player, so an app never builds one itself.
        player.set_setting(PlayerSetting::button(
            PICTURE_IN_PICTURE,
            crate::icons::Icon::PictureInPicture2,
            crate::i18n::widget_format("media-picture-in-picture", &[]),
            0,
        ));
        if let Some(address) = &source {
            // A window that draws a declared player is the one that plays its video: the
            // toolkit builds the engine there, over whatever decoder the process carries. The
            // address is the one the host handed over, already a medium this process plays.
            // The sound is captured rather than played on a device: the session's `outputs`
            // name every place it comes out, and the host routes the one stream there.
            #[cfg(feature = "native")]
            player.ensure_engine(|| {
                media::MediaEngine::with_config(media::EngineConfig::default().captured())
            });
            player.open(Source::for_uri(address));
        }
        if let Some(duration) = duration {
            player.set_duration_ms(seconds_to_ms(duration));
        }
        if let Some(title) = title {
            player.set_now_playing(NowPlaying {
                title,
                ..NowPlaying::default()
            });
        }
        if let Some(poster) = poster {
            let store = crate::pictures::Pictures::window().store().clone();
            player.set_poster(Some(crate::widget::MediaSource::Url { store, url: poster }));
        }
        if let Some(volume) = volume {
            player.set_volume(crate::num::f32_of_f64(volume));
        }
        if muted {
            player.toggle_mute();
        }
        if playing {
            player.play();
        }
        if let Some(position) = position {
            player.seek_to_ms(seconds_to_ms(position));
        }
        let reporting = seek.clone();
        if !seek.is_none() {
            player = player.on_seek(move |share: f64| {
                reporting.fire(Some(Value::Float(share)));
            });
        }
        Self {
            player,
            playing,
            set_playing,
            seek,
            fail,
            progress,
            length,
            audio_tracks,
            subtitle_tracks,
            chapters,
            reported: Reported::default(),
            applied: Applied::default(),
            wanted: Applied {
                rate,
                audio: audio_track,
                subtitles: subtitle_track,
                seek_to_seq: None,
            },
            jump: seek_to.zip(seek_to_seq),
            repeat,
            source,
            pip: std::cell::Cell::new(false),
            audio: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The player, for a host to lay out.
    #[must_use]
    pub fn player(&mut self) -> &mut VideoPlayer {
        &mut self.player
    }

    /// Report what the player offers the view since the last frame: the play state it moved to,
    /// where it stands, how long it runs, its tracks and chapters. Also apply the controls that
    /// changed, and begin again when the view asked it to repeat. The app's player stays the one
    /// truth.
    fn report(&mut self) {
        // A control the person pressed in the player's own gear menu: picture-in-picture is one
        // the host answers, so the player keeps it for the walk that takes it.
        for event in self.player.take_setting_events() {
            if matches!(&event, PlayerSettingEvent::Activated { id } if id == PICTURE_IN_PICTURE) {
                self.pip.set(true);
            }
        }
        // What the engine said, or the decoder or the limit it hit: a view shows it as the
        // player's failure instead of a picture that never came.
        for event in self.player.take_media_events() {
            if let Some(reason) = failure_of(&event) {
                self.fail.fire(Some(Value::Text(reason)));
            }
        }
        let playing = self.player.is_playing();
        if playing != self.playing {
            self.playing = playing;
            self.set_playing.fire(Some(Value::Bool(playing)));
        }
        let duration_ms = self.player.duration_ms();
        if self.reported.duration_ms != Some(duration_ms) {
            self.reported.duration_ms = Some(duration_ms);
            self.length.fire(Some(Value::Float(
                crate::num::f64_of_u64(duration_ms) / 1000.0,
            )));
        }
        // After the length went to the view, so the share it reads names a place in a video it
        // knows the length of.
        self.report_progress(playing);
        let audio: Vec<String> = self
            .player
            .engine()
            .map(|engine| {
                engine
                    .audio_tracks()
                    .into_iter()
                    .map(|track| track.label)
                    .collect()
            })
            .unwrap_or_default();
        if self.reported.audio.as_ref() != Some(&audio) {
            self.reported.audio = Some(audio.clone());
            self.audio_tracks.fire(Some(labels(audio)));
        }
        let subtitles: Vec<String> = self
            .player
            .engine()
            .map(|engine| {
                engine
                    .subtitle_tracks()
                    .into_iter()
                    .map(|track| track.label)
                    .collect()
            })
            .unwrap_or_default();
        if self.reported.subtitles.as_ref() != Some(&subtitles) {
            self.reported.subtitles = Some(subtitles.clone());
            self.subtitle_tracks.fire(Some(labels(subtitles)));
        }
        let chapters: Vec<String> = self
            .player
            .engine()
            .map(|engine| {
                engine
                    .chapters()
                    .into_iter()
                    .map(|chapter| chapter.label)
                    .collect()
            })
            .unwrap_or_default();
        if self.reported.chapters.as_ref() != Some(&chapters) {
            self.reported.chapters = Some(chapters.clone());
            self.chapters.fire(Some(labels(chapters)));
        }
        if let Some((seconds, seq)) = self.jump
            && self.applied.seek_to_seq != Some(seq)
        {
            self.player.seek_to_ms(seconds_to_ms(seconds));
            self.applied.seek_to_seq = Some(seq);
        }
        if let Some(engine) = self.player.engine_mut() {
            if self.wanted.rate != self.applied.rate {
                if let Some(rate) = self.wanted.rate {
                    engine.set_rate(crate::num::f32_of_f64(rate));
                }
                self.applied.rate = self.wanted.rate;
            }
            if self.wanted.audio != self.applied.audio {
                if let Some(index) = self
                    .wanted
                    .audio
                    .and_then(|index| u32::try_from(index).ok())
                {
                    engine.select_audio_track(index);
                }
                self.applied.audio = self.wanted.audio;
            }
            if self.wanted.subtitles != self.applied.subtitles {
                engine.select_subtitle_track(
                    self.wanted
                        .subtitles
                        .and_then(|index| u32::try_from(index).ok()),
                );
                self.applied.subtitles = self.wanted.subtitles;
            }
            // The sound belongs to the host: it plays it wherever the session's outputs name.
            let samples = engine.take_audio();
            if !samples.is_empty() {
                self.audio.borrow_mut().extend(samples);
            }
        }
        if self.repeat
            && self.player.is_playing()
            && duration_ms > 0
            && self.player.position_ms().saturating_add(250) >= duration_ms
        {
            self.player.seek_to_ms(0);
        }
    }
}

impl DeclaredVideo {
    /// Report where playback got to, as the share of the video's length the view keeps: once on
    /// the first frame it plays, then every five-second step, and once when it stops or ends. A
    /// player that has not started is not reported at zero, which reads as a video watched whole.
    fn report_progress(&mut self, playing: bool) {
        let position = self.player.position_ms();
        if position == 0 {
            return;
        }
        let due = if playing {
            self.reported
                .progress_ms
                .is_none_or(|was| position / PROGRESS_STEP_MS != was / PROGRESS_STEP_MS)
        } else {
            self.reported.progress_ms != Some(position)
        };
        if !due {
            return;
        }
        let duration = self.player.duration_ms();
        if duration == 0 {
            return;
        }
        self.reported.progress_ms = Some(position);
        let share = crate::num::f64_of_u64(position) / crate::num::f64_of_u64(duration);
        self.progress
            .fire(Some(Value::Float(share.clamp(0.0, 1.0))));
    }
}

/// Why the player could not go on, in the words the view shows it by: `None` for an event that is
/// not a failure.
fn failure_of(event: &media::MediaEvent) -> Option<String> {
    match event {
        media::MediaEvent::Error(reason) => Some(reason.clone()),
        media::MediaEvent::MissingDecoder { codec } => Some(crate::i18n::widget_format(
            "media-missing-decoder",
            &[("codec", codec.as_str().into())],
        )),
        media::MediaEvent::LimitReached => {
            Some(crate::i18n::widget_format("media-playback-limit", &[]))
        }
        _ => None,
    }
}

/// `seconds` as whole milliseconds, clamped at zero.
fn seconds_to_ms(seconds: f64) -> u64 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a position in whole milliseconds, clamped at zero"
    )]
    let ms = (seconds.max(0.0) * 1000.0) as u64;
    ms
}

/// The labels as the view reads them, as a list of text.
fn labels(labels: Vec<String>) -> Value {
    Value::List(labels.into_iter().map(Value::Text).collect())
}

impl PictureInPictureProvider for DeclaredVideo {
    fn wants_picture_in_picture(&self) -> bool {
        self.pip.get()
    }

    /// The floating picture the person asked for: the player's own region, the picture's shape,
    /// and the video's title. Taken once, so one press makes one surface.
    fn take_picture_in_picture(&self) -> Option<PictureInPictureAsk> {
        if !self.pip.replace(false) {
            return None;
        }
        let region = crate::widget::Widget::get_rect(&self.player);
        if region.width() < 1.0 || region.height() < 1.0 {
            return None;
        }
        let aspect = (region.width() / region.height()).clamp(0.4, 2.8);
        Some(PictureInPictureAsk {
            region,
            aspect,
            title: self.player.now_playing().title.clone(),
        })
    }
}

impl crate::widget::AudioTapProvider for DeclaredVideo {
    /// The sound the player decoded since the host last asked, for the host to play in every
    /// place the session's outputs name. One pipeline decodes it; the host fans it out.
    fn take_audio(&self) -> Vec<i16> {
        std::mem::take(&mut *self.audio.borrow_mut())
    }
}

impl crate::widget::Widget for DeclaredVideo {
    // A view's player offers picture-in-picture, and its sound to the host; the host walks the
    // tree it draws to find it.
    crate::provides!(
        dyn PictureInPictureProvider,
        dyn crate::widget::AudioTapProvider
    );

    /// The player itself.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        crate::widget::Widget::html(&self.player, cx);
    }

    fn needs_redraw(&self) -> bool {
        self.player.needs_redraw() || self.player.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.player.mark_drawn();
    }

    fn needs_animation_frame(&self) -> bool {
        self.player.needs_animation_frame()
    }

    fn is_layout_dirty(&self) -> bool {
        crate::widget::Widget::is_layout_dirty(&self.player)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.player.sync(state, path);
        self.report();
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let changed = self.player.update(state, path);
        self.report();
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        // The player's own leaf, measured as the player is: a view's player is the leaf the
        // layout asks for its size, so a column that grows with what it holds grows to hold it.
        let node = self.player.build_layout(taffy, theme, node_map);
        node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut crate::text_render::TextRenderer,
    ) -> taffy::Size<f32> {
        crate::widget::Widget::measure(&mut self.player, known, available, text_renderer)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.player.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: crate::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.player
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.player.handle_event(event, ctx);
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.player.get_rect()
    }

    fn name(&self) -> &'static str {
        "VideoPlayer"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.player.accessibility()
    }
}

impl Described for DeclaredVideo {
    type Props = VideoPlayerProps;

    fn from_props(_key: &str, props: VideoPlayerProps) -> Self {
        Self::new(props)
    }

    /// A changed play state, position or control takes in place.
    fn set_props(&mut self, props: VideoPlayerProps) -> bool {
        let VideoPlayerProps {
            playing,
            position,
            duration,
            seek_to,
            seek_to_seq,
            source,
            volume,
            rate,
            repeat,
            audio_track,
            subtitle_track,
            muted: _,
            seek,
            fail,
            progress,
            length,
            audio_tracks,
            subtitle_tracks,
            chapters,
            ..
        } = props;
        self.fail = fail;
        self.seek = seek;
        self.progress = progress;
        self.length = length;
        self.audio_tracks = audio_tracks;
        self.subtitle_tracks = subtitle_tracks;
        self.chapters = chapters;
        self.repeat = repeat;
        self.wanted = Applied {
            rate,
            audio: audio_track,
            subtitles: subtitle_track,
            seek_to_seq: self.wanted.seek_to_seq,
        };
        self.jump = seek_to.zip(seek_to_seq);
        self.playing = playing;
        if playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        if let Some(volume) = volume {
            self.player.set_volume(crate::num::f32_of_f64(volume));
        }
        if let Some(duration) = duration {
            self.player.set_duration_ms(seconds_to_ms(duration));
        }
        // A view that answers with another address for the same video (asking the service again
        // after a failure) plays what it now names.
        if source != self.source
            && let Some(address) = &source
        {
            self.player.open(Source::for_uri(address));
        }
        self.source = source;
        if let Some(position) = position {
            // A view that declares the place it was just told is not moved back to it.
            let at = seconds_to_ms(position);
            let standing = self.player.position_ms();
            if at.abs_diff(standing) > SEEK_TOLERANCE_MS {
                self.player.seek_to_ms(at);
            }
        }
        true
    }
}

/// The state the view used to declare, kept so a host that plays on its own reports each change.
#[allow(dead_code, reason = "a host reports every change of this state")]
fn _playing(_: PlaybackState) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::video_player::PlayerSettingControl;

    /// Every view's player offers picture-in-picture as one of its own controls, so an app never
    /// builds the control itself.
    #[test]
    fn a_declared_player_offers_picture_in_picture() {
        let video = DeclaredVideo::new(VideoPlayerProps::default());
        let setting = video
            .player
            .settings()
            .iter()
            .find(|setting| setting.id == PICTURE_IN_PICTURE)
            .expect("the player offers picture-in-picture");
        assert!(
            matches!(setting.control, PlayerSettingControl::Button),
            "it is a control that runs when pressed: {:?}",
            setting.control
        );
    }
}
