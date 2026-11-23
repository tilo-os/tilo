//! [`VideoPlayer`] as a described component: a video surface with playback controls.

use media::Source;

use crate::component::{Component, Described, Event, Value};

use super::{NowPlaying, PlaybackState, VideoPlayer};
use crate::widget::DrawChild;

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
    /// Where it is stored, as a `file://` address or a URL the engine fetches.
    #[component(data)]
    pub source: Option<String>,
    /// The volume, `0..=1`.
    pub volume: Option<f64>,
    /// Whether it is muted.
    pub muted: bool,
    /// Play or pause was pressed: carries `true` when it plays.
    #[component(event(value = bool))]
    pub set_playing: Event,
    /// It was moved to a place in the video: carries `0..=1`.
    #[component(event(value = f64))]
    pub seek: Event,
    /// Where playback got to while it played, as a share of its length (`0..=1`): reported on the
    /// first frame it plays, then every five seconds, and once more when it stops, so a view
    /// remembers the place a video was left at even when nobody moved the playhead.
    #[component(event(value = f64))]
    pub progress: Event,
    /// The volume was set: carries `0..=1`.
    #[component(event(value = f64))]
    pub set_volume: Event,
    /// The player could not show its video: carries what the engine said.
    #[component(event(value = String))]
    pub fail: Event,
    /// The engine the player runs the video on, which its app builds.
    #[component(skip)]
    pub engine: Option<media::MediaEngine>,
}

/// A video player driven from a view: its picture follows what the view declares, and its
/// controls report the view's events.
pub struct DeclaredVideo {
    player: VideoPlayer,
    playing: bool,
    set_playing: Event,
    seek: Event,
    progress: Event,
    fail: Event,
    /// Where playback was last reported at, in milliseconds: the app's share is reported again
    /// only when a five-second step of it passed, or when playback stopped.
    reported_ms: Option<u64>,
    /// The address the player was opened at: a view that answers with another one for the same
    /// video (what asking the service again does) opens that one.
    source: Option<String>,
}

/// How far playback moves between the progress reports a view keeps: five seconds of video, so a
/// video watched without touching the playhead is remembered where it was left.
const PROGRESS_STEP_MS: u64 = 5_000;

/// How far the player's own playhead may stand from the place a view declares before it is moved
/// there: a view that declares the place it was just told is not moved back to it.
const SEEK_TOLERANCE_MS: u64 = 1_000;

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
            source,
            volume,
            muted,
            set_playing,
            seek,
            progress,
            set_volume,
            fail,
            engine,
        } = props;
        let _ = set_volume;
        let mut player = VideoPlayer::new();
        if let Some(engine) = engine {
            player.set_engine(engine);
        }
        if let Some(title) = title {
            player.set_now_playing(NowPlaying {
                title,
                ..NowPlaying::default()
            });
        }
        if let Some(source) = &source {
            player.open(Source::for_uri(source));
        }
        if let Some(duration) = duration {
            player.set_duration_ms(seconds_to_ms(duration));
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
        let reporting = seek.clone();
        if !seek.is_none() {
            player = player.on_seek(move |at: f64| {
                reporting.fire(Some(Value::Float(at)));
            });
        }
        if playing {
            player.play();
        }
        if let Some(position) = position {
            // The place the view opens the video at is reported as a seek of its own, so a view
            // that keeps the place hears the one it starts at too.
            player.seek_to_ms(seconds_to_ms(position));
        }
        Self {
            player,
            playing,
            set_playing,
            seek,
            progress,
            fail,
            reported_ms: None,
            source,
        }
    }

    /// The player, for a host to lay out.
    #[must_use]
    pub fn player(&mut self) -> &mut VideoPlayer {
        &mut self.player
    }

    /// Report what the player asks of the view since the last frame: the play state it moved
    /// to when the person pressed play or pause, the place playback got to as it played, and why
    /// it could not go on. The app's player stays the one truth.
    fn report(&mut self) {
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
        self.report_progress(playing);
    }

    /// Report where playback got to, as the share of the video's length the view keeps: once on
    /// the first frame it plays, then every five-second step, and once when it stops or ends. A
    /// player that has not started is not reported at zero, which reads as a video watched whole.
    fn report_progress(&mut self, playing: bool) {
        let position = self.player.position_ms();
        if position == 0 {
            return;
        }
        let due = if playing {
            self.reported_ms
                .is_none_or(|was| position / PROGRESS_STEP_MS != was / PROGRESS_STEP_MS)
        } else {
            self.reported_ms != Some(position)
        };
        if !due {
            return;
        }
        let duration = self.player.duration_ms();
        if duration == 0 {
            return;
        }
        self.reported_ms = Some(position);
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

impl crate::widget::Widget for DeclaredVideo {
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

    /// A changed play state or a changed position takes in place.
    fn set_props(&mut self, props: VideoPlayerProps) -> bool {
        let VideoPlayerProps {
            playing,
            position,
            source,
            volume,
            muted: _,
            seek,
            progress,
            fail,
            ..
        } = props;
        self.fail = fail;
        self.seek = seek;
        self.progress = progress;
        self.playing = playing;
        // Another address for the same video: the service answered the player's failure, and the
        // new one is opened where the view says the person was.
        if let Some(source) = &source
            && self.source.as_deref() != Some(source.as_str())
        {
            self.player.open(Source::for_uri(source));
            self.source = Some(source.clone());
            self.reported_ms = None;
        }
        if playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        if let Some(volume) = volume {
            self.player.set_volume(crate::num::f32_of_f64(volume));
        }
        if let Some(position) = position {
            // A view declares the place it was last told about: the player is moved there only
            // when it stands somewhere else, so reporting where playback got to does not pull it
            // back to that place on the next frame.
            let at = seconds_to_ms(position);
            if at.abs_diff(self.player.position_ms()) > SEEK_TOLERANCE_MS {
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
    use crate::component::testing::{Node, fired};

    /// The player a view declares through `props`, with its events queued.
    fn player(
        queue: &crate::Intents<crate::component::Fired>,
        events: &[&'static str],
        props: &[(&str, Value)],
    ) -> DeclaredVideo {
        let mut node = Node::new("VideoPlayer", "v", queue);
        for event in events {
            node = node.on(event);
        }
        for (name, value) in props {
            node = node.prop(name, value.clone());
        }
        let props = VideoPlayerProps::from_source(&mut node).expect("the props read");
        <DeclaredVideo as Described>::from_props("v", props)
    }

    fn report(share: f64) -> crate::component::Fired {
        fired("v", "progress", Some(Value::Float(share)))
    }

    tilo_test_env::scenario! {
        fn the_player_reports_where_playback_got_to;

        phase a_five_second_step_is_reported_and_stopping_reports_the_last_place_once;
        {
            let queue = crate::Intents::default();
            let mut video = player(
                &queue,
                &["progress", "set_playing"],
                &[("playing", Value::Bool(true)), ("duration", Value::Float(100.0))],
            );
            let doc: &crate::widget::AppModel = &();
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [], "a video that has not moved is not reported at zero");
            video.player().seek_to_ms(4_200);
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [report(0.042)], "the first playing frame reports");
            video.player().seek_to_ms(4_900);
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [], "inside the same five seconds nothing is said again");
            video.player().seek_to_ms(5_000);
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [report(0.05)], "the next five seconds are reported");
            video.player().seek_to_ms(5_000);
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [], "a place already reported is not reported again");
            video.player().pause();
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(
                queue.take(),
                [fired("v", "set_playing", Some(Value::Bool(false)))],
                "stopping where it stood reports the stop only"
            );
            video.player().seek_to_ms(7_000);
            crate::headless::frame_widget(&mut video, doc);
            assert_eq!(queue.take(), [report(0.07)], "a pause reports the place it was left at");
        }

        phase a_place_the_view_declares_moves_the_player_only_when_it_stands_elsewhere;
        {
            let queue = crate::Intents::default();
            let mut video = player(
                &queue,
                &["seek"],
                &[("position", Value::Float(30.0)), ("duration", Value::Float(100.0))],
            );
            assert_eq!(
                queue.take(),
                [fired("v", "seek", Some(Value::Float(0.3)))],
                "starting at a place reports it, as a share of the video"
            );
            video.set_props(VideoPlayerProps {
                position: Some(30.4),
                duration: Some(100.0),
                ..VideoPlayerProps::default()
            });
            assert_eq!(queue.take(), [], "the place it already stands at is not sought again");
            video.set_props(VideoPlayerProps {
                position: Some(45.0),
                ..VideoPlayerProps::default()
            });
            assert_eq!(
                queue.take(),
                [fired("v", "seek", Some(Value::Float(0.45)))],
                "a place it does not stand at moves it"
            );
        }
    }
}
