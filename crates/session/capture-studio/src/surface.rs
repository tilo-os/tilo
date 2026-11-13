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
use ui::widget::DrawChild;

/// How long what became of a send stays up once it is known, the same as the
/// receipt it belongs to.
const SENT_SHOWN: ui::time::Duration = ui::time::Duration::from_millis(5_500);

/// The capture bar over the desktop, over the studio's seat.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each one is a different thing the studio may have to say"
)]
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
    /// The file of the recording that ended, while its receipt offers to
    /// play or show it: a recording that could not be finished leaves
    /// nothing to play.
    finished: Option<std::path::PathBuf>,
    /// Whether the person sent the last screenshot somewhere the session is
    /// still delivering it: the bar went down with the picker, and the studio
    /// stays up to say what became of it.
    sending: bool,
    /// The studio itself put the bar away. What the session then says of the
    /// capture comes back up, rather than being lost on a sheet the shell has
    /// already taken down; a sheet the shell took down stays down.
    put_away: bool,
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
            finished: None,
            sending: false,
            put_away: false,
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
        if self.sending {
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
        self.come_back();
        if opened {
            self.choose_how_it_opened();
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
                self.sending = false;
                self.saying = None;
                self.put_away = false;
                self.widget.open();
                true
            }
            Phase::Leaving | Phase::Closed if self.widget.is_open() => {
                // The shell took the studio down: it stays down, whatever the
                // bar was saying.
                self.put_away = false;
                self.widget.close();
                false
            }
            _ => false,
        }
    }

    /// The session said something of the capture after the studio put its own
    /// bar away: the bar comes back up, and the shell shows the studio again.
    fn come_back(&mut self) {
        if self.own || !self.put_away || !self.widget.is_open() {
            return;
        }
        self.put_away = false;
        let presentation = self.studio.presentation();
        if presentation.is_open() {
            return;
        }
        // The same studio shown again, not a new opening over what it has to
        // say.
        presentation.open(false);
        self.opening = Some(presentation.openings());
    }

    /// The bar comes up on what somebody outside the studio opened it for: a
    /// screenshot, a recording, or a picture in a picture.
    fn choose_how_it_opened(&mut self) {
        if let Some(mode) = self.studio.link().take_open_on() {
            self.widget.set_mode(mode);
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
            && (self.own || self.studio.presentation().is_drawn())
        {
            // A shot taken while the bar is down is said on the floating
            // layer's card, which the session put up with it: nothing opens
            // over the desk for it.
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
            self.sending = false;
            self.saying = Some((i18n::sent(sent), ui::time::now()));
        }
        if scene.failed != self.scene.failed
            && let Some(failed) = &scene.failed
        {
            // The capture the person took did not come out: the bar says why
            // on the thing they touched, and stays up until it is read.
            self.saying = Some((i18n::failed(&failed.failure), ui::time::now()));
        }
        self.scene = scene;
        self.dirty = true;
    }

    /// Show the recording that runs, and what became of the one that ended:
    /// the frames it had to skip, or why its file could not be finished, and
    /// the receipt that plays or shows it.
    fn show_recording(&mut self, scene: &Scene) {
        let was = self.scene.recording.is_some();
        match &scene.recording {
            Some(recording) => {
                self.widget.set_recording_active(true);
                self.widget.set_recording_paused(recording.paused);
                self.widget.set_recording_skipped(recording.skipped);
            }
            None if was => self.widget.set_recording_active(false),
            None => {}
        }
        if scene.finished == self.scene.finished {
            return;
        }
        match &scene.finished {
            Some(finished) => {
                self.finished.clone_from(&finished.path);
                match &finished.failure {
                    Some(reason) => self
                        .widget
                        .set_recording_failed(reason.clone(), finished.path.is_some()),
                    None => self.widget.set_recording_finished(finished.skipped),
                }
            }
            None => self.finished = None,
        }
    }

    /// Show the seconds the replay buffer keeps ready.
    fn show_replay(&mut self, scene: &Scene) {
        let was = self.scene.replay;
        match scene.replay {
            Some(seconds) => {
                if was.is_none() {
                    self.widget.set_replay_active(true, scene.replay_audio);
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
            // What the receipt acts on: the screenshot it shows.
            let kept = self.scene.kept.as_ref().map(|kept| kept.path.clone());
            match command {
                ScreenshotCommand::OpenLast
                | ScreenshotCommand::MarkupLast
                | ScreenshotCommand::PinLast => {
                    if let Some(path) = kept {
                        link.ask(Ask::Open(path));
                    }
                }
                ScreenshotCommand::RevealLast => {
                    if let Some(path) = kept {
                        link.ask(Ask::Reveal(path));
                    }
                }
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
                    self.sending = true;
                    self.saying = Some((i18n::sending(&label), ui::time::now()));
                    self.dirty = true;
                    link.ask(Ask::SendLastTo(id));
                }
                // Sending into the person's workspace files: the session
                // copies it there, saying so until it says what came of it.
                ScreenshotCommand::SendLast(ScreenshotSendTarget::Workspace) => {
                    self.sending = true;
                    self.saying = Some((i18n::saving(), ui::time::now()));
                    self.dirty = true;
                    link.ask(Ask::SendLastToWorkspace);
                }
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
                // The receipt acts on the recording it is about, never on a
                // screenshot taken since.
                RecordingCommand::OpenLast => {
                    if let Some(path) = self.finished.clone() {
                        link.ask(Ask::Open(path));
                    }
                }
                RecordingCommand::RevealLast => {
                    if let Some(path) = self.finished.clone() {
                        link.ask(Ask::Reveal(path));
                    }
                }
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
    fn dismiss(&mut self) {
        if self.own {
            return;
        }
        // What the person put down stays down, whatever the bar was still
        // saying. A bar that went down with the pick of where a screenshot
        // goes stays up to say what became of it, until that has been up long
        // enough to read.
        if !self.widget.take_put_away() && self.picked() {
            return;
        }
        let presentation = self.studio.presentation();
        if !self.widget.is_open() && presentation.phase() == Phase::Open {
            // The studio put the bar away itself, receipt and all: what the
            // person put down has been read, and the session says the next
            // capture where the bar is down.
            self.put_away = true;
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

    /// The studio comes up on a screenshot choosing a window: the bar the
    /// person picks one in, over the windows it lists.
    fn choose_a_window(&mut self) {
        self.studio.link().open_on(CaptureMode::Screenshot);
        self.studio.presentation().open(false);
        self.catch_up();
        self.widget.set_source_mode(SourceMode::Window);
        self.dirty = true;
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
            audio: AudioCaptureMode::None,
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

/// Whether `take` asks for the window in front and there is none to take:
/// the studio then comes up to choose one rather than answering with a
/// failure the person never saw.
fn nothing_in_front(take: &Take) -> bool {
    take.source == SourceMode::Window && take.targets.is_empty()
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
        audio: intent.audio,
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
            .draw_child(scene, transform, text_renderer, theme, presence);
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
        self.dirty
            || self.behind()
            || (self.shown_open()
                && (self.widget.needs_redraw() || self.widget.needs_animation_frame()))
    }

    fn needs_animation_frame(&self) -> bool {
        // With the bar down nothing of it is drawn: it runs for its actions
        // and the scene, not to animate what nobody sees.
        self.shown_open() && self.widget.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let said = match (self.sending, &self.saying) {
            (false, Some((_, since))) => since.checked_add(SENT_SHOWN),
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
            CaptureEffect::Open { mode } => {
                if let Some(mode) = mode {
                    self.studio.link().open_on(mode.into());
                }
                if self.own {
                    self.widget.open();
                } else {
                    self.studio.presentation().open(false);
                }
                self.catch_up();
            }
            CaptureEffect::Screenshot { source, window } => {
                let take = self.take_for(source.as_deref(), window.as_deref());
                if nothing_in_front(&take) {
                    // No window is in front to take: the person picks one in
                    // the studio, as a shortcut that names no window does.
                    self.choose_a_window();
                    return Ok(Effected::done());
                }
                self.studio.link().ask(Ask::Screenshot(take));
            }
            CaptureEffect::Record { source, window } => {
                let take = self.take_for(source.as_deref(), window.as_deref());
                if nothing_in_front(&take) {
                    self.choose_a_window();
                    return Ok(Effected::done());
                }
                if take.source == SourceMode::Region && take.regions.is_empty() {
                    return Err("a region recording needs a selection made in the studio".into());
                }
                self.studio.link().ask(Ask::StartRecording(take));
            }
            CaptureEffect::StopRecording => self.studio.link().ask(Ask::StopRecording),
            CaptureEffect::PauseRecording => self.studio.link().ask(Ask::PauseRecording),
            CaptureEffect::ResumeRecording => self.studio.link().ask(Ask::ResumeRecording),
            // The last half minute when a buffer keeps one. With nothing
            // kept there is nothing to save: the person's chord starts a
            // recording instead, unless one already runs.
            CaptureEffect::SaveReplay => {
                if self.scene.replay.is_some() {
                    self.studio.link().ask(Ask::SaveReplay);
                } else if self.scene.recording.is_none() {
                    self.studio.link().open_on(CaptureMode::Recording);
                    self.studio.presentation().open(false);
                }
            }
        }
        self.dirty = true;
        Ok(Effected::done())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar the session speaks through says what went wrong with the
    /// capture the person took, where it says what became of a send: on the
    /// bar itself, not only in a log.
    #[test]
    fn a_capture_that_did_not_come_out_is_said_on_the_bar() {
        use std::path::PathBuf;

        use crate::link::{Failed, Failure, WriteCause};
        // The person's own folder, named the way they read it: the system's
        // error is never what the bar says.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let (folder, named) = home.map_or_else(
            || (PathBuf::from("/pictures"), String::from("/pictures")),
            |home| {
                (
                    home.join("Pictures/Screenshots"),
                    String::from("~/Pictures/Screenshots"),
                )
            },
        );
        let said = format!("Couldn't save the screenshot: Tilo can't write to {named}");
        let failure = Failure::NotWritten {
            folder,
            cause: WriteCause::Permission,
        };
        let studio = Studio::new();
        let mut surface = StudioSurface::new(Some(studio.clone()));
        studio.link().publish(Scene {
            failed: Some(Failed {
                number: 1,
                failure: failure.clone(),
            }),
            ..Scene::default()
        });
        surface.catch_up();
        assert_eq!(
            surface.saying.as_ref().map(|(said, _)| said.as_str()),
            Some(said.as_str())
        );
        // The same capture taken again is news again, not a repeat the bar
        // keeps quiet about.
        studio.link().publish(Scene {
            failed: Some(Failed { number: 2, failure }),
            ..surface.scene.clone()
        });
        surface.catch_up();
        assert_eq!(
            surface.saying.as_ref().map(|(said, _)| said.as_str()),
            Some(said.as_str()),
            "a second attempt that failed the same way is said again"
        );
    }

    /// Print with the window modifier names the window in front; with none in
    /// front the bar comes up choosing one, rather than the capture failing
    /// where the person never saw it.
    #[test]
    fn a_window_capture_with_none_in_front_comes_up_to_choose_one() {
        let studio = Studio::new();
        let mut surface = StudioSurface::new(Some(studio.clone()));
        surface.catch_up();
        let take = surface.take_for(Some("window"), None);
        assert_eq!(take.source, SourceMode::Window);
        assert!(nothing_in_front(&take), "no window is in front");
        surface.choose_a_window();
        assert!(
            studio.presentation().is_open(),
            "the bar comes up rather than the capture failing"
        );
        assert_eq!(
            surface.widget.source_mode(),
            SourceMode::Window,
            "choosing the window the capture is of"
        );
        assert!(
            !studio.link().has_asks(),
            "nothing is captured behind a choice the person has not made"
        );

        // A window in front is taken at once, without the bar coming up.
        studio.link().publish(Scene {
            windows: vec![ui::widget::CaptureTarget {
                id: "window-1".to_owned(),
                title: "Notes".to_owned(),
                app_id: "tilo-notes".to_owned(),
                visible_rect: Rect::new(0.0, 0.0, 800.0, 600.0),
                virtual_rect: Rect::new(0.0, 0.0, 800.0, 600.0),
                complete_rect: None,
                focused: true,
                on_active_strip: true,
            }],
            ..Scene::default()
        });
        surface.catch_up();
        let take = surface.take_for(Some("window"), None);
        assert_eq!(take.targets, ["window-1"]);
        assert!(!nothing_in_front(&take));
    }

    /// A shot taken at the desk once the person put the studio away says so
    /// on the floating layer; the bar does not come back over the desk for it.
    #[test]
    fn a_shot_from_the_desk_does_not_bring_the_studio_back() {
        use crate::link::Kept;
        use ui::widget::{CapturePreview, ScreenshotKept};

        let studio = Studio::new();
        let mut surface = StudioSurface::new(Some(studio.clone()));
        studio.presentation().open(false);
        surface.catch_up();
        assert!(surface.widget.is_open(), "the bar is up with the studio");

        // The person puts it away with Escape: the bar closes itself and the
        // studio goes with it.
        surface.widget.close();
        surface.catch_up();
        assert!(
            !studio.presentation().is_open(),
            "the studio goes down with its bar"
        );

        // A shot taken from the desk answers where the bar is down: on the
        // floating layer, with nothing opening over the desk.
        let kept = Kept {
            kept: ScreenshotKept::SAVED,
            path: "/home/person/Pictures/Screenshots/shot.png".into(),
            width: 800,
            height: 600,
            preview: CapturePreview {
                columns: 1,
                rows: 1,
                pixels: vec![[20, 40, 60, 255]],
            },
        };
        studio.link().publish(Scene {
            kept: Some(kept),
            ..Scene::default()
        });
        surface.catch_up();
        assert!(
            !studio.presentation().is_open(),
            "the shot's card is the floating layer's: the bar stays down"
        );
        assert!(!surface.widget.is_open(), "and so does the bar window");
    }

    /// A capture that could not be written says so on the bar, and a person
    /// who puts the bar down while that answer is still up takes the studio
    /// with it: the next capture answers on the desk, not behind a studio the
    /// session still thinks it has up.
    #[test]
    fn a_failure_put_away_takes_the_studio_down_with_it() {
        use crate::link::{Failed, Failure, WriteCause};

        let studio = Studio::new();
        let mut surface = StudioSurface::new(Some(studio.clone()));
        studio.presentation().open(false);
        surface.catch_up();
        studio.link().publish(Scene {
            failed: Some(Failed {
                number: 1,
                failure: Failure::NotWritten {
                    folder: "/home/person/Pictures/Screenshots".into(),
                    cause: WriteCause::Permission,
                },
            }),
            ..Scene::default()
        });
        surface.catch_up();
        assert!(
            studio.presentation().is_open() && surface.widget.is_open(),
            "the bar says what became of the capture until it is read"
        );

        // Escape, with the answer still up: the bar goes down, and the studio
        // goes with it rather than standing over a desk nobody sees it on.
        surface.widget.close();
        surface.catch_up();
        assert!(
            !studio.presentation().is_drawn(),
            "a bar the person put down leaves nothing drawn"
        );

        // The next capture opens the bar again, and what it says is read on
        // it as before.
        studio.presentation().open(false);
        surface.catch_up();
        assert!(
            studio.presentation().is_open() && surface.widget.is_open(),
            "the studio comes back up on the next capture"
        );
    }

    #[test]
    fn a_recording_takes_the_sound_the_person_chose() {
        let intent = CaptureIntent {
            mode: CaptureMode::Recording,
            source: SourceMode::Display,
            destination: CaptureDestination::File,
            regions: Vec::new(),
            anchors: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::SystemAndMicrophone,
            pip_layout: ui::widget::PipLayout::Auto,
            targets: Vec::new(),
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        assert_eq!(
            take(&intent).audio,
            AudioCaptureMode::SystemAndMicrophone,
            "what the bar offered to record is what the session is asked for"
        );
        assert_eq!(
            Take::default().audio,
            AudioCaptureMode::None,
            "a take nobody asked sound of is silent"
        );
    }
}
