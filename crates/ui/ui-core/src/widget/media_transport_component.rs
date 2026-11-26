//! [`MediaTransport`] as a described component: what plays and the controls that steer it.

use media::Source;

use crate::widget::{MediaTransport, TransportArt, TransportFeed, TransportRepeat, TransportState};
use tilo_ui_base::{component::Component, component::Described, component::Event};

use super::media_transport::TransportTriggers;
use tilo_ui_base::widget::DrawChild;

// The `TransportRepeat` choice is declared beside the enum, in `media_transport.rs`.

/// How far the place moves before the view is told again, as the video player reports its own.
const PROGRESS_STEP_MS: f64 = 5_000.0;

/// Which five-second step of the song `ms` falls in.
fn progress_step(ms: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a song's place in steps of five seconds is far inside i64"
    )]
    let step = (ms / PROGRESS_STEP_MS).floor() as i64;
    step
}

/// A declared place within this of the one the band reported is the same place, not a seek.
const SEEK_TOLERANCE_MS: f64 = 250.0;

/// What is playing and the controls that steer it, as one band: the art and the song, like
/// and lyrics, shuffle, previous, play/pause, next and repeat over the scrubber, the queue
/// and the volume.
#[derive(Component, Default)]
#[component(name = "MediaTransport", role = "Toolbar", label = "title")]
pub struct MediaTransportProps {
    /// The song's name, or what the band says with nothing playing.
    #[component(required)]
    pub title: String,
    /// Who it is by, or why it cannot play.
    pub subtitle: Option<String>,
    /// The subtitle says why it cannot play: it reads as an error.
    pub error: bool,
    /// Its cover: a picture by its address, loaded as it arrives.
    pub art: Option<crate::widget::PictureSource>,
    /// The name the placeholder cover takes its initials and colour from (the album).
    pub art_name: Option<String>,
    /// What a press on the art opens ("Open album", "Show lyrics"), as it is announced.
    pub opens: Option<String>,
    /// Whether it plays.
    pub playing: bool,
    /// Nothing can play: every control rests disabled.
    pub idle: bool,
    /// Where it is, in seconds.
    pub position: Option<f64>,
    /// How long it is, in seconds.
    pub duration: Option<f64>,
    /// The volume, `0..=1`; zero is muted.
    pub volume: Option<f64>,
    /// Whether shuffle is on. Without it there is no shuffle control.
    pub shuffle: Option<bool>,
    /// What repeat repeats. Without it there is no repeat control.
    pub repeat: Option<TransportRepeat>,
    /// Whether what plays is liked. Without it there is no like control.
    pub liked: Option<bool>,
    /// Whether the lyrics show. Without it there is no lyrics control.
    pub lyrics: Option<bool>,
    /// Whether the queue shows. Without it there is no queue control.
    pub queue: Option<bool>,
    /// The play/pause control was pressed.
    #[component(event)]
    pub act_play_pause: Event,
    /// The previous control was pressed.
    #[component(event)]
    pub act_previous: Event,
    /// The next control was pressed.
    #[component(event)]
    pub act_next: Event,
    /// Shuffle was toggled.
    #[component(event)]
    pub act_shuffle: Event,
    /// Repeat was toggled.
    #[component(event)]
    pub act_repeat: Event,
    /// What plays was liked or unliked.
    #[component(event)]
    pub act_like: Event,
    /// The band was muted or unmuted.
    #[component(event)]
    pub act_mute: Event,
    /// The band was moved to a place in the song: carries `0..=1`.
    #[component(event(value = f64))]
    pub act_seek: Event,
    /// The volume was set: carries `0..=1`.
    #[component(event(value = f64))]
    pub act_volume: Event,
    /// The lyrics were asked for.
    #[component(event)]
    pub act_lyrics: Event,
    /// The queue was asked for.
    #[component(event)]
    pub act_queue: Event,
    /// Where the sound plays from: a file's address or a stream's URL the engine fetches. With
    /// one the band plays it and its scrubber follows the sound; without one it only shows what
    /// the view declares.
    #[component(data)]
    pub source: Option<String>,
    /// What played reached its end: the app moves on to what comes next.
    #[component(event)]
    pub ended: Event,
    /// Where playback got to while it played, as a share of its length (`0..=1`): reported on the
    /// first five-second step, then once per step, and once more when it stops or ends, so a
    /// view remembers the place a song was left at even when nobody moved the playhead.
    #[component(event(value = f64))]
    pub progress: Event,
    /// How long the sound runs, in seconds, once the engine has read it: what a song opened from
    /// a file is long, which its name does not say.
    #[component(event(value = f64))]
    pub length: Event,
    /// The engine could not play the source: carries what it said, and where in the song it
    /// stopped, in seconds. A stream a source serves only partway through stops here, and the
    /// app asks the source for a fresh address and continues from that place.
    #[component(event(value = String, at = f64))]
    pub fail: Event,
    /// The engine the band plays on, where its host builds one (a test does, with the
    /// synthesized backend). Without one the band builds it when it is given a source.
    #[component(skip)]
    pub engine: Option<media::MediaEngine>,
}

impl MediaTransportProps {
    /// What the band shows, as the view declares it.
    #[must_use]
    pub fn state(&self) -> TransportState {
        let ms = |seconds: Option<f64>| seconds.unwrap_or(0.0) * 1000.0;
        TransportState {
            title: self.title.clone(),
            subtitle: self.subtitle.clone().unwrap_or_default(),
            error: self.error,
            art: match &self.art {
                Some(picture) if !picture.is_empty() => TransportArt::Picture(picture.clone()),
                _ => TransportArt::Placeholder,
            },
            art_name: self.art_name.clone().unwrap_or_default(),
            opens: self.opens.clone(),
            playing: self.playing,
            idle: self.idle,
            position_ms: ms(self.position),
            duration_ms: ms(self.duration),
            volume: self.volume.unwrap_or(1.0),
            shuffle: self.shuffle,
            repeat: self.repeat,
            liked: self.liked,
            lyrics: self.lyrics,
            queue: self.queue,
            more: false,
        }
    }

    /// What each control reports.
    #[must_use]
    pub fn triggers(&self) -> TransportTriggers {
        TransportTriggers {
            play_pause: given(&self.act_play_pause),
            previous: given(&self.act_previous),
            next: given(&self.act_next),
            shuffle: given(&self.act_shuffle),
            repeat: given(&self.act_repeat),
            like: given(&self.act_like),
            mute: given(&self.act_mute),
            seek: given(&self.act_seek),
            volume: given(&self.act_volume),
            lyrics: given(&self.act_lyrics),
            queue: given(&self.act_queue),
            open: None,
        }
    }
}

fn given(event: &Event) -> Option<Event> {
    if event.is_none() {
        None
    } else {
        Some(event.clone())
    }
}

/// A band driven from a view: what plays as the view declares it, and its controls
/// reporting the view's events.
///
/// With a `source` the band plays it on its own engine, the same media engine the video player
/// draws with: the scrubber follows the sound, so the view declares what to play and never keeps
/// a clock of its own.
pub struct DeclaredTransport {
    band: MediaTransport,
    feed: TransportFeed,
    /// What the view last declared, kept to show again with the engine's own playing, place and
    /// length where it plays.
    declared: TransportState,
    /// The band's own engine, when the view named a source.
    engine: Option<media::MediaEngine>,
    /// Where the sound plays from, as the view last declared it.
    source: Option<String>,
    /// What played ended by itself, and the failure the engine reported: told to the view.
    ended: Event,
    fail: Event,
    /// The place the view was last told, in milliseconds: a declared place that is this one is
    /// no seek, and the next five-second step is told again.
    told: Option<f64>,
    /// The length the view was last told, in milliseconds.
    measured: Option<f64>,
    /// Where playback got to, as a share of its length, and how long it runs: told to the view.
    progress: Event,
    length: Event,
    /// The engine told the view its song ended: it is told once, until the next song plays.
    ended_told: bool,
    /// The engine's place moved since the last frame: the band draws it.
    dirty: bool,
}

impl DeclaredTransport {
    /// The band showing `state`, its controls reporting `triggers`.
    #[must_use]
    pub fn new(state: TransportState, triggers: TransportTriggers) -> Self {
        let feed = TransportFeed::new();
        feed.show(state.clone());
        let band = MediaTransport::triggered(&feed, triggers);
        Self {
            band,
            feed,
            declared: state,
            engine: None,
            source: None,
            ended: Event::default(),
            fail: Event::default(),
            told: None,
            measured: None,
            progress: Event::default(),
            length: Event::default(),
            ended_told: false,
            dirty: false,
        }
    }

    /// The band, for a host to lay out.
    #[must_use]
    pub fn band(&mut self) -> &mut MediaTransport {
        &mut self.band
    }

    /// What the person asked to see since the last frame (a swipe, the queue's request).
    #[must_use]
    pub fn take_events(&self) -> Vec<super::TransportEvent> {
        self.feed.take_events()
    }

    /// What the band shows: the view's declaration with the engine's own playing, place and
    /// length where it plays.
    #[must_use]
    pub fn shown(&self) -> TransportState {
        self.feed.shown()
    }

    /// Play `source` when this window carries a decoder: where it starts, how loud and whether
    /// it plays. A window that carries none (the page's) reports the failure to the view.
    fn open(&mut self, source: &str, playing: bool, volume: Option<f64>, position: Option<f64>) {
        self.ended_told = false;
        self.told = position.map(|seconds| seconds * 1000.0);
        self.measured = None;
        // The engine is the window's own: native it decodes in the process, and a page it plays
        // through the page's media element. A build that carries no decoder at all answers with
        // an engine that reports why it cannot play, which the view hears as `fail`.
        let mut engine = self.engine.take().unwrap_or_default();
        if let Some(volume) = volume {
            engine.set_volume(tilo_ui_base::num::f32_of_f64(volume));
        }
        if let Some(position) = position {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a position in whole milliseconds, clamped at zero"
            )]
            engine.seek_ms((position.max(0.0) * 1000.0) as u64);
        }
        engine.open(Source::for_uri(source));
        if playing {
            engine.play();
        } else {
            engine.pause();
        }
        self.engine = Some(engine);
    }

    /// What the engine says of the song now: its place, its length and whether it plays, and
    /// the end of it the view has not heard yet. The view's own declaration is kept for all
    /// else — the song's name, its cover and what its controls do.
    fn pump(&mut self) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        // What the engine says, taken before the rest of the band is touched again: the borrow
        // of the engine ends here, and what follows is the band's own.
        let (failed, ended, position, duration, playing) = {
            let events = engine.poll();
            let failed = events.iter().find_map(|event| match event {
                media::MediaEvent::Error(reason) => Some(reason.clone()),
                _ => None,
            });
            let ended = events
                .iter()
                .any(|event| matches!(event, media::MediaEvent::Ended));
            (
                failed,
                ended,
                tilo_ui_base::num::f64_of_u64(engine.position_ms()),
                engine.duration_ms().map(tilo_ui_base::num::f64_of_u64),
                engine.is_playing(),
            )
        };
        // Compared by their bits, never by `==`: the lints keep a float comparison an explicit
        // act, and what is asked here is only whether the band must draw again.
        self.dirty |= self.declared.position_ms.to_bits() != position.to_bits()
            || duration
                .is_some_and(|duration| self.declared.duration_ms.to_bits() != duration.to_bits())
            || self.declared.playing != playing;
        self.declared.position_ms = position;
        if let Some(duration) = duration {
            self.declared.duration_ms = duration;
            if self
                .measured
                .is_none_or(|measured| (duration - measured).abs() >= 500.0)
            {
                self.measured = Some(duration);
                self.length.fire(Some(tilo_ui_base::component::Value::Float(
                    duration / 1000.0,
                )));
            }
        }
        self.declared.playing = playing;
        self.feed.show(self.declared.clone());
        if let Some(reason) = failed {
            self.fail
                .fire(Some(tilo_ui_base::component::Value::List(vec![
                    tilo_ui_base::component::Value::Text(reason),
                    tilo_ui_base::component::Value::Float(position / 1000.0),
                ])));
        }
        if ended && !self.ended_told {
            self.ended_told = true;
            self.ended.fire(None);
        }
        // Where the view must remember it got to, as the share of the length the video player
        // reports its own: once the song has a length and a place, at each five-second step
        // while it plays, and once more when it stops or ends. Never at zero, which reads as a
        // song heard whole.
        if let Some(duration) = duration.filter(|duration| *duration > 0.0) {
            let due = if playing {
                self.told
                    .is_none_or(|was| progress_step(position) != progress_step(was))
            } else {
                self.told != Some(position)
            };
            if due && position > 0.0 {
                self.told = Some(position);
                self.progress
                    .fire(Some(tilo_ui_base::component::Value::Float(
                        (position / duration).clamp(0.0, 1.0),
                    )));
            }
        }
    }

    /// Take what the view declares: a new source plays from it, a changed play state plays or
    /// pauses, and a moved place seeks. The engine's own answers are shown again after.
    fn adopt(&mut self, mut props: MediaTransportProps) {
        self.ended.clone_from(&props.ended);
        self.fail.clone_from(&props.fail);
        self.progress.clone_from(&props.progress);
        self.length.clone_from(&props.length);
        if let Some(engine) = props.engine.take() {
            self.engine = Some(engine);
        }
        if props.source != self.source {
            self.source.clone_from(&props.source);
            if let Some(source) = props.source.clone() {
                let (playing, volume, position) = (props.playing, props.volume, props.position);
                self.open(&source, playing, volume, position);
            } else if let Some(engine) = self.engine.as_mut() {
                engine.pause();
            }
        } else if let Some(engine) = self.engine.as_mut() {
            if engine.is_playing() != props.playing {
                if props.playing {
                    engine.play();
                } else {
                    engine.pause();
                }
            }
            if let Some(volume) = props.volume
                && (engine.volume() - tilo_ui_base::num::f32_of_f64(volume)).abs() > 0.001
            {
                engine.set_volume(tilo_ui_base::num::f32_of_f64(volume));
            }
            // The place the view declares is the one it was told, unless somebody moved it:
            // what the band reported is no seek back, a person's or an agent's jump is.
            if let Some(position) = props.position
                && self
                    .told
                    .is_none_or(|told| (position * 1000.0 - told).abs() > SEEK_TOLERANCE_MS)
            {
                self.told = Some(position * 1000.0);
                self.declared.position_ms = position * 1000.0;
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a position in whole milliseconds, clamped at zero"
                )]
                engine.seek_ms((position.max(0.0) * 1000.0) as u64);
            }
        }
        let has_source = self.engine.is_some();
        let mut state = props.state();
        if has_source {
            state.playing = self.declared.playing;
            state.position_ms = self.declared.position_ms;
            state.duration_ms = self.declared.duration_ms;
        }
        self.declared.clone_from(&state);
        self.feed.show(state);
    }
}

impl Described for DeclaredTransport {
    type Props = MediaTransportProps;

    fn from_props(_key: &str, props: MediaTransportProps) -> Self {
        let mut transport = Self::new(props.state(), props.triggers());
        transport.adopt(props);
        transport
    }

    /// A changed song or state shows in place: the band's controls follow what it declares.
    fn set_props(&mut self, props: MediaTransportProps) -> bool {
        self.adopt(props);
        true
    }
}

impl tilo_ui_base::widget::Widget for DeclaredTransport {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.pump();
        self.band.sync(state, path);
    }

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        self.band.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<
            taffy::NodeId,
            *mut dyn tilo_ui_base::widget::Widget,
        >,
    ) -> taffy::NodeId {
        self.band.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.band.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.band.draw_child(scene, transform, text, theme, &env);
    }

    fn children(&self) -> Vec<&dyn tilo_ui_base::widget::Widget> {
        self.band.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn tilo_ui_base::widget::Widget + '_)> {
        self.band.children_mut()
    }

    /// The band is this widget's one layout child: the window reaches it (and the manual
    /// region its controls are placed in) through this, never only through the controls
    /// themselves.
    fn for_each_layout_child(
        &self,
        visitor: &mut dyn FnMut(&dyn tilo_ui_base::widget::Widget),
    ) -> bool {
        visitor(&self.band);
        true
    }

    /// The band places its own controls: without this the window never hands the band its
    /// region's policy, and its controls fall back to a plain flow that spills past the band.
    fn manual_layout_policy(&self) -> Option<tilo_ui_base::widget::ManualPolicy> {
        self.band.manual_layout_policy()
    }

    fn layout_needs_measure(&self) -> bool {
        self.band.layout_needs_measure()
    }

    fn is_layout_dirty(&self) -> bool {
        self.band.is_layout_dirty()
    }

    fn size_before_solve(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        node: taffy::NodeId,
        text: &mut tilo_ui_base::text_render::TextRenderer,
    ) {
        self.band.size_before_solve(taffy, node, text);
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        node: taffy::NodeId,
        text: &mut tilo_ui_base::text_render::TextRenderer,
    ) -> bool {
        self.band.adjust_layout_style(taffy, node, text)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.band.take_structure_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(media::MediaEngine::is_playing)
            || self.band.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.band.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.band.mark_drawn();
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.band.get_rect()
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.band.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "MediaTransport"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.band.accessibility()
    }
}
