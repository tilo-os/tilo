//! [`MediaTransport`] as a described component: what plays and the controls that steer it.

use crate::component::{Component, Described, Event};
use crate::widget::{MediaTransport, TransportArt, TransportFeed, TransportRepeat, TransportState};

use super::media_transport::TransportTriggers;
use crate::widget::DrawChild;

// The `TransportRepeat` choice is declared beside the enum, in `media_transport.rs`.

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
pub struct DeclaredTransport {
    band: MediaTransport,
    feed: TransportFeed,
}

impl DeclaredTransport {
    /// The band showing `state`, its controls reporting `triggers`.
    #[must_use]
    pub fn new(state: TransportState, triggers: TransportTriggers) -> Self {
        let feed = TransportFeed::new();
        feed.show(state);
        let band = MediaTransport::triggered(&feed, triggers);
        Self { band, feed }
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
}

impl Described for DeclaredTransport {
    type Props = MediaTransportProps;

    fn from_props(_key: &str, props: MediaTransportProps) -> Self {
        Self::new(props.state(), props.triggers())
    }

    /// A changed song or state shows in place: the band's controls follow what it declares.
    fn set_props(&mut self, props: MediaTransportProps) -> bool {
        self.feed.show(props.state());
        true
    }
}

impl crate::widget::Widget for DeclaredTransport {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.band.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.band.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
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

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: crate::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.band
            .draw_child(scene, transform, text, theme, presence);
    }

    fn children(&self) -> Vec<&dyn crate::widget::Widget> {
        self.band.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        self.band.children_mut()
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.band.get_rect()
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.band.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "MediaTransport"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.band.accessibility()
    }
}
