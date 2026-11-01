//! The studio's surface: the capture bar over the desktop, showing the
//! windows the session lends it and what came of the captures it asked for,
//! and putting what the person chose in front of the session as
//! [`Ask`]s.

use std::collections::HashMap;

use tilo_sheets::{Phase, Seat as _};
use ui::framework::{EffectAnswer, EffectCx, Effected, ResourceSurface, SurfaceCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::vello::Scene as VelloScene;
use ui::vello::kurbo::{Affine, Rect};
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CaptureIntent, CaptureMode, CaptureStudio, EventCtx,
    RecordingCommand, ReplayCommand, ScreenshotCommand, ScreenshotSendTarget, SourceMode,
    SurfaceLevel, Widget,
};
use ui::winit::event::WindowEvent;

use crate::i18n;
use crate::link::{Ask, Scene, Studio, Take};
use crate::resource::{Capture, CaptureEffect};

/// How long what became of a send stays up once it is known, the same as the
/// receipt it belongs to.
const SENT_SHOWN: ui::time::Duration = ui::time::Duration::from_millis(5_500);

/// The capture bar over the desktop, over the studio's seat.
pub struct StudioSurface {
    widget: CaptureStudio,
    studio: Studio,
    /// No session lent the studio (its own window, a capture, a test): the
    /// bar is up from the first frame and what it asks goes nowhere.
    own: bool,
    /// The opening shown, so a new one starts afresh.
    opening: Option<u64>,
    /// The scene shown, and the generation it was published with.
    scene: Scene,
    shown: u64,
    /// The device the person sent the last screenshot to, while the session
    /// stages and offers it: the bar went down with the picker, and the studio
    /// stays up to say what became of it.
    sending: Option<String>,
    /// What the studio says of that send, and since when.
    saying: Option<(String, ui::time::Instant)>,
    /// Where that was drawn, for tests to find it.
    chip: Rect,
    rect: Rect,
    dirty: bool,
}

impl StudioSurface {
    /// The studio over the session's seat, or on its own when no session
    /// lent one.
    #[must_use]
    pub fn new(seat: Option<Studio>) -> Self {
        let own = seat.is_none();
        let studio = seat.unwrap_or_default();
        let mut surface = Self {
            widget: CaptureStudio::default(),
            studio,
            own,
            opening: None,
            scene: Scene::default(),
            shown: 0,
            sending: None,
            saying: None,
            chip: Rect::ZERO,
            rect: Rect::ZERO,
            dirty: true,
        };
        surface.catch_up();
        surface
    }

    /// Whether the bar is up: on its own, always; over a session, while the
    /// session has it up.
    fn shown_open(&self) -> bool {
        self.own || self.studio.presentation().is_open()
    }

    /// Whether the studio is still answering a send: the bar went down with
    /// the pick, and the receipt stays up to say what became of it, until
    /// what it says has been up long enough to read.
    fn picked(&self) -> bool {
        if self.sending.is_some() {
            return true;
        }
        self.saying.as_ref().is_some_and(|(_, since)| {
            ui::time::now().saturating_duration_since(*since) < SENT_SHOWN
        })
    }

    /// Whether the session or its presentation moved past what is shown here.
    fn behind(&self) -> bool {
        if self.studio.link().generation() != self.shown {
            return true;
        }
        if self.own {
            return !self.widget.is_open();
        }
        let presentation = self.studio.presentation();
        match presentation.phase() {
            Phase::Open => {
                self.opening != Some(presentation.openings())
                    || (!self.widget.is_open() && !self.picked())
            }
            Phase::Leaving | Phase::Closed => self.widget.is_open(),
        }
    }

    /// The seat the studio talks to the session over.
    #[must_use]
    pub const fn studio(&self) -> &Studio {
        &self.studio
    }

    /// Follow the session and the scene: open or close the bar as the
    /// session says, and show what it last handed the studio.
    fn catch_up(&mut self) {
        let opened = self.follow();
        self.take_scene();
        if opened {
            self.choose_what_is_asked();
        }
        self.take_intents();
        self.stop_saying();
        self.dismiss();
    }

    /// Open on a new opening, close when the session took the studio down.
    /// On its own the bar is up from the first frame. `true` when the bar
    /// came up now.
    fn follow(&mut self) -> bool {
        if self.own {
            if !self.widget.is_open() {
                self.widget.open();
                return true;
            }
            return false;
        }
        let presentation = self.studio.presentation();
        match presentation.phase() {
            Phase::Open if self.opening != Some(presentation.openings()) => {
                self.opening = Some(presentation.openings());
                // A new capture is a new conversation: what was said of the
                // last send belongs to the receipt that is gone.
                self.sending = None;
                self.saying = None;
                self.widget.open();
                true
            }
            Phase::Leaving | Phase::Closed if self.widget.is_open() => {
                self.widget.close();
                false
            }
            _ => false,
        }
    }

    /// The bar comes up choosing what an app asked the person for: a window
    /// to pick or an area to draw, as a screenshot.
    fn choose_what_is_asked(&mut self) {
        let Some(asking) = self.scene.asking else {
            return;
        };
        self.widget.set_mode(CaptureMode::Screenshot);
        self.widget.set_source_mode(asking.source());
    }

    /// Show what the session published since the last look.
    fn take_scene(&mut self) {
        let scene = self.studio.link().scene();
        if scene.generation == self.shown {
            return;
        }
        self.shown = scene.generation;
        if scene.windows != self.scene.windows {
            self.widget.set_candidates(scene.windows.clone());
        }
        if (scene.system_audio, scene.microphone)
            != (self.scene.system_audio, self.scene.microphone)
        {
            self.widget
                .set_audio_available(scene.system_audio, scene.microphone);
        }
        self.show_recording(&scene);
        self.show_replay(&scene);
        if scene.replay_saves != self.scene.replay_saves {
            self.widget.set_replay_saved();
        }
        if scene.kept != self.scene.kept
            && let Some(kept) = &scene.kept
        {
            self.widget.set_screenshot_finished(
                kept.kept,
                kept.width,
                kept.height,
                kept.preview.clone(),
            );
        }
        if scene.devices != self.scene.devices {
            self.widget
                .set_screenshot_send_devices(scene.devices.clone());
        }
        if scene.sent != self.scene.sent
            && let Some(sent) = &scene.sent
        {
            // The session took the send: say what became of it in place of
            // "sending", and stay up until it has been read.
            self.sending = None;
            self.saying = Some((i18n::sent(sent), ui::time::now()));
        }
        self.scene = scene;
        self.dirty = true;
    }

    /// Show the recording that runs, or that the last one ended.
    fn show_recording(&mut self, scene: &Scene) {
        let was = self.scene.recording.is_some();
        match &scene.recording {
            Some(recording) => {
                self.widget.set_recording_active(true);
                self.widget.set_recording_paused(recording.paused);
            }
            None if was => self.widget.set_recording_active(false),
            None => {}
        }
    }

    /// Show the seconds the replay buffer keeps ready.
    fn show_replay(&mut self, scene: &Scene) {
        let was = self.scene.replay;
        match scene.replay {
            Some(seconds) => {
                if was.is_none() {
                    self.widget.set_replay_active(true, AudioCaptureMode::None);
                }
                self.widget.set_replay_buffered_seconds(seconds);
            }
            None => {
                if was.is_some() {
                    self.widget.set_replay_active(false, AudioCaptureMode::None);
                }
            }
        }
    }

    /// Put what the person did on the bar in front of the session.
    fn take_intents(&mut self) {
        for intent in self.widget.take_intents() {
            self.ask(intent);
        }
    }

    fn ask(&mut self, intent: CaptureIntent) {
        let link = self.studio.link().clone();
        // Post-capture commands act on what was made; they never capture
        // again.
        if let Some(command) = intent.screenshot_command {
            match command {
                ScreenshotCommand::OpenLast
                | ScreenshotCommand::MarkupLast
                | ScreenshotCommand::PinLast => link.ask(Ask::OpenLast),
                ScreenshotCommand::RevealLast => link.ask(Ask::RevealLast),
                ScreenshotCommand::CopyLast => link.ask(Ask::CopyLast),
                ScreenshotCommand::SendLast(ScreenshotSendTarget::Device(id)) => {
                    let label = self
                        .scene
                        .devices
                        .iter()
                        .find(|device| device.id == id)
                        .map_or_else(|| id.clone(), |device| device.label.clone());
                    // Say where it is going until the session says what came
                    // of it: the bar is down from here on.
                    self.sending = Some(id.clone());
                    self.saying = Some((i18n::sending(&label), ui::time::now()));
                    self.dirty = true;
                    link.ask(Ask::SendLastTo(id));
                }
                // Sending into the person's workspace files is the workspace
                // files stack's.
                ScreenshotCommand::SendLast(ScreenshotSendTarget::Workspace) => {}
            }
            return;
        }
        if let Some(command) = intent.replay_command {
            match command {
                ReplayCommand::Start => link.ask(Ask::StartReplay(take(&intent))),
                ReplayCommand::SaveRecent => link.ask(Ask::SaveReplay),
                ReplayCommand::Stop => link.ask(Ask::StopReplay),
            }
            return;
        }
        if let Some(command) = intent.recording_command {
            match command {
                RecordingCommand::Start => link.ask(Ask::StartRecording(take(&intent))),
                RecordingCommand::Pause => link.ask(Ask::PauseRecording),
                RecordingCommand::Resume => link.ask(Ask::ResumeRecording),
                RecordingCommand::Stop => link.ask(Ask::StopRecording),
                RecordingCommand::OpenLast => link.ask(Ask::OpenLast),
                RecordingCommand::RevealLast => link.ask(Ask::RevealLast),
            }
            return;
        }
        match intent.mode {
            CaptureMode::Screenshot => link.ask(Ask::Screenshot(take(&intent))),
            CaptureMode::Recording => link.ask(Ask::StartRecording(take(&intent))),
            // A live picture in picture is a share stream of the same
            // source, which the share stack owns.
            CaptureMode::Pip => {}
        }
    }

    /// Take the studio down once the bar closed itself (a capture, a
    /// button, Escape), and say when nothing of it is drawn.
    fn dismiss(&self) {
        if self.own {
            return;
        }
        // The receipt stays up to say where the screenshot went, and what
        // became of it.
        if self.picked() {
            return;
        }
        let presentation = self.studio.presentation();
        if !self.widget.is_open() && presentation.phase() == Phase::Open {
            presentation.dismissed();
        }
        if presentation.phase() == Phase::Leaving {
            presentation.gone();
        }
    }

    /// Stop saying what became of a send once it has been up long enough.
    fn stop_saying(&mut self) {
        if self.saying.is_some() && !self.picked() {
            self.saying = None;
            self.dirty = true;
        }
    }

    /// The take `source` names, as an action asked for it.
    fn take_for(&self, source: Option<&str>, window: Option<&str>) -> Take {
        let source = match source.map(str::trim) {
            Some("window") => SourceMode::Window,
            Some("region") => SourceMode::Region,
            Some("strip") => SourceMode::Strip,
            _ => SourceMode::Display,
        };
        let targets: Vec<String> = match window.map(str::trim) {
            Some(id) => vec![id.to_owned()],
            // The focused window when the caller named none.
            None => self
                .scene
                .windows
                .iter()
                .find(|target| target.focused)
                .map(|target| vec![target.id.clone()])
                .unwrap_or_default(),
        };
        Take {
            source,
            regions: if source == SourceMode::Region {
                self.widget.regions().to_vec()
            } else {
                Vec::new()
            },
            anchors: Vec::new(),
            targets,
            include_pointer: false,
            destination: CaptureDestination::File,
        }
    }
}

/// Say what became of a send in a chip over the studio's seat, and answer
/// where it was drawn.
fn said_chip(
    scene: &mut VelloScene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    seat: Rect,
    said: &str,
) -> Rect {
    if seat.width() <= 0.0 || seat.height() <= 0.0 {
        return Rect::ZERO;
    }
    let style = theme.text(TextRole::Label);
    let room = ui::num::f32_of_f64((seat.width() * 0.8).min(520.0));
    let shown = text.elide_to_width_style(said, room, &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (pad_x, pad_y) = (12.0, 8.0);
    let (width, height) = (f64::from(w) + 2.0 * pad_x, f64::from(h) + 2.0 * pad_y);
    let center = seat.center();
    let chip = Rect::new(
        center.x - width / 2.0,
        center.y - height / 2.0,
        center.x + width / 2.0,
        center.y + height / 2.0,
    );
    let on = ui::widget::paint_surface(scene, transform, chip, SurfaceLevel::Floating, theme);
    text.draw_style(
        scene,
        &shown,
        on.tokens.colors.text_primary,
        transform.pre_translate((chip.x0 + pad_x, center.y - f64::from(h) / 2.0).into()),
        None,
        &style,
    );
    chip
}

/// The take an intent names.
fn take(intent: &CaptureIntent) -> Take {
    Take {
        source: intent.source,
        regions: intent.regions.clone(),
        anchors: intent.anchors.clone(),
        targets: intent
            .targets
            .iter()
            .map(|target| target.id.clone())
            .collect(),
        include_pointer: intent.include_pointer,
        destination: intent.destination,
    }
}

impl Widget for StudioSurface {
    fn name(&self) -> &'static str {
        "CaptureStudio"
    }

    fn test_id(&self) -> Option<&str> {
        Some("capture-studio")
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        // The bar covers the whole surface: it is an overlay over what it
        // captures, not a row of it.
        let style = ui::taffy::Style {
            position: ui::taffy::Position::Absolute,
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::percent(1.0),
                height: ui::taffy::Dimension::percent(1.0),
            },
            ..ui::taffy::Style::default()
        };
        let node = taffy
            .new_leaf(style)
            .unwrap_or_else(|_| ui::taffy::NodeId::from(0_u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::vello::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        // The bar draws in the surface's whole area, as it does in the
        // shell it came from.
        self.widget.set_overlay_rect(self.rect);
        self.widget.apply_layout(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        // What the session published, and whether it has the studio up, live
        // outside the document: a scene the studio has not shown yet is what
        // this frame is for, so it is never a frame that syncs nothing.
        let behind = self.behind();
        if behind {
            self.catch_up();
        }
        behind
    }

    fn sync(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) {
        self.catch_up();
    }

    fn draw(
        &mut self,
        scene: &mut VelloScene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.catch_up();
        self.widget
            .draw(scene, transform, text_renderer, theme, presence);
        self.take_intents();
        self.chip = match &self.saying {
            Some((said, _)) => said_chip(scene, transform, text_renderer, theme, self.rect, said),
            None => Rect::ZERO,
        };
        // What this frame drew is what the link and the session said of it.
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.widget.handle_event(event, ctx);
        self.take_intents();
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.behind() || (self.shown_open() && self.widget.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        // With the bar down nothing of it is drawn: it runs for its actions
        // and the scene, not to animate what nobody sees.
        self.shown_open() && self.widget.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let said = match (&self.sending, &self.saying) {
            (None, Some((_, since))) => since.checked_add(SENT_SHOWN),
            _ => None,
        };
        match (self.widget.next_wake_deadline(), said) {
            (Some(bar), Some(said)) => Some(bar.min(said)),
            (bar, said) => bar.or(said),
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.widget.wants_pointer_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let said = self.saying.as_ref().map(|(said, _)| {
            ui::a11y::AccessNode::new(ui::a11y::Role::Label)
                .with_name(said.clone())
                .with_live(ui::a11y::Politeness::Polite)
        });
        match (self.widget.accessibility(), said) {
            (Some(mut bar), Some(said)) => {
                bar.children.push(said);
                Some(bar)
            }
            (bar, None) => bar,
            (None, said) => said,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let pressed = self
            .widget
            .perform_semantic_action(target, action, state, path);
        self.take_intents();
        pressed
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = self.widget.test_rects();
        if self.chip.area() > 0.0 {
            rects.push(("capture.send-status".to_owned(), self.chip));
        }
        rects
    }
}

impl ResourceSurface<Capture, CaptureEffect> for StudioSurface {
    fn present(&mut self, _state: &std::rc::Rc<Capture>) {}

    fn attach(&mut self, _cx: SurfaceCx<Capture>) {}

    fn effect(&mut self, effect: CaptureEffect, _cx: &mut EffectCx<Capture, Self>) -> EffectAnswer {
        match effect {
            CaptureEffect::Open => {
                if self.own {
                    self.widget.open();
                } else {
                    self.studio.presentation().open(false);
                }
                self.catch_up();
            }
            CaptureEffect::Screenshot { source, window } => {
                let take = self.take_for(source.as_deref(), window.as_deref());
                self.studio.link().ask(Ask::Screenshot(take));
            }
            CaptureEffect::Record { source, window } => {
                let take = self.take_for(source.as_deref(), window.as_deref());
                if take.source == SourceMode::Region && take.regions.is_empty() {
                    return Err("a region recording needs a selection made in the studio".into());
                }
                self.studio.link().ask(Ask::StartRecording(take));
            }
            CaptureEffect::StopRecording => self.studio.link().ask(Ask::StopRecording),
            CaptureEffect::SaveReplay => self.studio.link().ask(Ask::SaveReplay),
        }
        self.dirty = true;
        Ok(Effected::done())
    }
}
