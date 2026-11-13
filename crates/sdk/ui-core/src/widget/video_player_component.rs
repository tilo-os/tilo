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
    fail: Event,
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
            source,
            volume,
            muted,
            set_playing,
            seek,
            set_volume,
            fail,
            engine,
        } = props;
        let _ = (duration, set_volume);
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
        if let Some(source) = source {
            player.open(Source::for_uri(&source));
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
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a position in whole milliseconds, clamped at zero"
            )]
            let at = (position.max(0.0) * 1000.0) as u64;
            player.seek_to_ms(at);
        }
        let reporting = seek.clone();
        if !seek.is_none() {
            player = player.on_seek(move |at: crate::time::Duration| {
                reporting.fire(Some(Value::Float(at.as_secs_f64())));
            });
        }
        Self {
            player,
            playing,
            set_playing,
            seek,
            fail,
        }
    }

    /// The player, for a host to lay out.
    #[must_use]
    pub fn player(&mut self) -> &mut VideoPlayer {
        &mut self.player
    }

    /// Report what the player asks of the view since the last frame: the play state it moved
    /// to when the person pressed play or pause. The app's player stays the one truth.
    fn report(&mut self) {
        let playing = self.player.is_playing();
        if playing != self.playing {
            self.playing = playing;
            self.set_playing.fire(Some(Value::Bool(playing)));
        }
    }
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
        self.player.build_layout(taffy, theme, node_map)
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
            volume,
            muted: _,
            seek,
            fail,
            ..
        } = props;
        self.fail = fail;
        self.seek = seek;
        self.playing = playing;
        if playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        if let Some(volume) = volume {
            self.player.set_volume(crate::num::f32_of_f64(volume));
        }
        if let Some(position) = position {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a position in whole milliseconds, clamped at zero"
            )]
            let at = (position.max(0.0) * 1000.0) as u64;
            self.player.seek_to_ms(at);
        }
        true
    }
}

/// The state the view used to declare, kept so a host that plays on its own reports each change.
#[allow(dead_code, reason = "a host reports every change of this state")]
fn _playing(_: PlaybackState) {}
