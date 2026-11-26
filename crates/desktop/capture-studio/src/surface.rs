//! The studio's surface: the capture bar over the desktop, showing the
//! windows the session lends it and what came of the captures it asked for,
//! and putting what the person chose in front of the session as
//! [`Ask`]s.

use std::collections::HashMap;
use std::path::PathBuf;

use tilo_sheets::{Phase, SheetState as _};
use ui::framework::{EffectAnswer, EffectCx, Effected, ResourceSurface, SurfaceCx};

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use ui::vello::Scene as VelloScene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::{widget::AudioCaptureMode, widget::CaptureDestination, widget::CaptureIntent, widget::CaptureMode, widget::CaptureStudio, widget::MarkupStroke, widget::MarkupTool, widget::RecordingCommand, widget::ReplayCommand, widget::ScreenshotCommand, widget::ScreenshotMarkup, widget::ScreenshotMarkupCommand, widget::ScreenshotSendTarget, widget::SourceMode, widget::SurfaceLevel};
use tilo_ui_base::{widget::EventCtx, widget::TaffyTreeExt, widget::Widget};
use ui::winit::event::WindowEvent;

use crate::i18n;
use crate::link::{Ask, Scene, Studio, Take};
use crate::resource::{Capture, CaptureEffect};
use tilo_ui_base::widget::DrawChild;

/// How long what became of a send stays up once it is known, the same as the
/// receipt it belongs to.
const SENT_SHOWN: ui::time::Duration = ui::time::Duration::from_millis(5_500);

/// The markup editor, over the picture it marks up.
struct Markup {
    widget: ScreenshotMarkup,
    /// The screenshot it marks up: keeping its marks writes another file, so
    /// this tells the receipt the editor opened from the picture it keeps.
    path: PathBuf,
}

impl Markup {
    fn of(image: crate::link::MarkupImage, spawn_from: Option<Rect>) -> Self {
        let widget = ScreenshotMarkup::new(image.rgba, image.width, image.height)
            .with_spawn_from(spawn_from);
        Self {
            widget,
            path: image.path,
        }
    }
}

/// The capture bar over the desktop, over the studio's state.
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
    /// The markup editor, while the last screenshot is being marked up.
    markup: Option<Markup>,
    /// The session is still reading the picture the editor draws on.
    markup_wanted: bool,
    /// Where the editor grows from: the receipt's thumbnail the person
    /// pressed, or nothing for an action's.
    markup_from: Option<Rect>,
    /// The marks an action asked for before the picture was there to draw
    /// them on.
    pending_marks: Vec<MarkupStroke>,
    /// The marked picture is to be kept once its marks are on.
    keeping: bool,
    /// The marked picture is on its way to its file: a second Save while
    /// that runs asks for no second file.
    saving: bool,
    /// The editor came or went: the tree under the surface is to be built
    /// again, so the editor's node is there (or gone).
    markup_changed: bool,
    /// Where that was drawn, for tests to find it.
    chip: Rect,
    rect: Rect,
    dirty: bool,
}

impl StudioSurface {
    /// The studio over the session's state, or on its own when no session
    /// lent one.
    #[must_use]
    pub fn new(state: Option<Studio>) -> Self {
        let own = state.is_none();
        let studio = state.unwrap_or_default();
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
            markup: None,
            markup_wanted: false,
            markup_from: None,
            pending_marks: Vec::new(),
            keeping: false,
            saving: false,
            markup_changed: false,
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
                    || (!self.widget.is_open()
                        && !self.picked()
                        && self.markup.is_none()
                        && !self.markup_wanted)
            }
            Phase::Leaving | Phase::Closed => self.widget.is_open(),
        }
    }

    /// The state the studio talks to the session over.
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
        self.take_markup_picture();
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
            // over the shell for it.
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
            // The picture the editor was to draw on could not be read: the
            // studio stays up to say why, rather than waiting for an editor
            // nobody will see.
            if self.markup.is_none() {
                self.markup_wanted = false;
            } else {
                // The marked picture could not be kept: the marks are still
                // the person's to keep again.
                self.keeping = false;
                self.saving = false;
            }
        }
        // The picture the editor marks up was kept as a new screenshot: its
        // receipt says what became of it, and the editor has done its work.
        let kept_anew = self.markup.as_ref().is_some_and(|markup| {
            scene
                .kept
                .as_ref()
                .is_some_and(|kept| kept.path != markup.path)
        });
        if kept_anew {
            self.close_markup();
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

    /// Show the picture the session read back for the markup editor, once:
    /// the editor draws on it, with the marks an action queued for it.
    fn take_markup_picture(&mut self) {
        let Some(image) = self.studio.link().take_markup() else {
            return;
        };
        self.markup = Some(Markup::of(image, self.markup_from.take()));
        self.markup_wanted = false;
        self.markup_changed = true;
        // The editor is the studio now: the bar it came from stays down
        // under it.
        if !self.own {
            self.widget.close();
        }
        let pending = std::mem::take(&mut self.pending_marks);
        if let Some(markup) = self.markup.as_mut() {
            for stroke in pending {
                markup.widget.add_stroke(stroke);
            }
        }
        self.keep_markup_when_asked();
        self.dirty = true;
    }

    /// Take up the markup editor's own Save, Copy, Open and Cancel.
    fn take_markup_commands(&mut self) {
        let Some(markup) = self.markup.as_mut() else {
            return;
        };
        let commands = markup.widget.take_commands();
        for command in commands {
            match command {
                ScreenshotMarkupCommand::Save(export) => {
                    self.keeping = true;
                    self.save_markup(export);
                }
                ScreenshotMarkupCommand::Copy(export) => {
                    self.studio.link().ask(Ask::CopyMarkup(export));
                }
                ScreenshotMarkupCommand::OpenExternal => {
                    if let Some(kept) = &self.scene.kept {
                        self.studio.link().ask(Ask::Open(kept.path.clone()));
                    }
                }
                ScreenshotMarkupCommand::Cancel => self.close_markup(),
            }
        }
    }

    /// Keep the marked picture once it has its marks: the person pressed Save,
    /// or an action asked for the picture to be kept now.
    fn keep_markup_when_asked(&mut self) {
        if !self.keeping {
            return;
        }
        let Some(markup) = self.markup.as_mut() else {
            return;
        };
        let export = markup.widget.flattened();
        self.save_markup(export);
    }

    fn save_markup(&mut self, export: ui::widget::MarkupExport) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.studio.link().ask(Ask::SaveMarkup(export));
        self.dirty = true;
    }

    /// Put the editor away: nothing of the marked picture is kept.
    fn close_markup(&mut self) {
        if self.markup.take().is_none() && !self.markup_wanted {
            return;
        }
        self.markup_wanted = false;
        self.markup_from = None;
        self.pending_marks.clear();
        self.keeping = false;
        self.saving = false;
        self.markup_changed = true;
        self.dirty = true;
    }

    /// Mark the last screenshot up, as the action `effect` names: the editor
    /// opens on it, and the stroke it gives is drawn on it as a drag with
    /// that tool is.
    fn annotate(&mut self, effect: CaptureEffect) -> EffectAnswer {
        let CaptureEffect::Annotate {
            shape,
            color,
            width,
            points,
            keep,
        } = effect
        else {
            return Err("not an annotation".into());
        };
        if self.scene.kept.is_none() {
            return Err("there is no screenshot to mark up: take one first".into());
        }
        if let Some(stroke) =
            stroke_of(shape.as_deref(), color.as_deref(), width, points.as_deref())
        {
            match self.markup.as_mut() {
                Some(markup) => markup.widget.add_stroke(stroke),
                None => self.pending_marks.push(stroke),
            }
        }
        if keep == Some(true) {
            self.keeping = true;
        }
        if self.markup.is_none() && !self.markup_wanted {
            self.markup_wanted = true;
            self.studio.link().ask(Ask::AnnotateLast);
        }
        self.keep_markup_when_asked();
        self.dirty = true;
        Ok(Effected::done())
    }

    fn ask(&mut self, intent: CaptureIntent) {
        let link = self.studio.link().clone();
        // Post-capture commands act on what was made; they never capture
        // again.
        if let Some(command) = intent.screenshot_command {
            // What the receipt acts on: the screenshot it shows.
            let kept = self.scene.kept.as_ref().map(|kept| kept.path.clone());
            match command {
                ScreenshotCommand::MarkupLast => {
                    // The editor grows out of the thumbnail the person
                    // pressed, where the receipt showed it.
                    self.markup_from = intent.regions.first().copied();
                    self.markup_wanted = true;
                    link.ask(Ask::AnnotateLast);
                }
                ScreenshotCommand::OpenLast | ScreenshotCommand::PinLast => {
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
        // The editor is the studio while it is up, and the session is still
        // reading its picture: the bar being down says nothing about the
        // studio.
        if self.markup.is_some() || self.markup_wanted {
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

/// Say what became of a send in a chip over the studio's state, and answer
/// where it was drawn.
fn said_chip(
    scene: &mut VelloScene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    said: &str,
) -> Rect {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Rect::ZERO;
    }
    let style = theme.text(TextRole::Label);
    let room = tilo_ui_base::num::f32_of_f64((rect.width() * 0.8).min(520.0));
    let shown = text.elide_to_width_style(said, room, &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (pad_x, pad_y) = (12.0, 8.0);
    let (width, height) = (f64::from(w) + 2.0 * pad_x, f64::from(h) + 2.0 * pad_y);
    let center = rect.center();
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

/// The stroke an action names, in the picture's own pixels: its shape, ink,
/// width and points. The tool's own colour and width stand where the action
/// names none, as the toolbar would have them.
fn stroke_of(
    shape: Option<&str>,
    color: Option<&str>,
    width: Option<f32>,
    points: Option<&[String]>,
) -> Option<MarkupStroke> {
    let tool = match shape?.trim().to_ascii_lowercase().as_str() {
        "" | "pen" => MarkupTool::Pen,
        "highlighter" => MarkupTool::Highlighter,
        "arrow" => MarkupTool::Arrow,
        "rectangle" => MarkupTool::Rectangle,
        "redact" => MarkupTool::Redact,
        _ => return None,
    };
    let points: Vec<Point> = points
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point_of(point))
        .collect();
    if points.is_empty() {
        return None;
    }
    let (width_px, alpha) = match tool {
        MarkupTool::Highlighter => (18.0, 92),
        MarkupTool::Redact => (0.0, 255),
        MarkupTool::Pen | MarkupTool::Arrow | MarkupTool::Rectangle => (4.0, 255),
    };
    let mut ink = match tool {
        // What a redaction covers with: the tool picks its own ink.
        MarkupTool::Redact => [18, 18, 22, 255],
        _ => color.and_then(ink_of).unwrap_or([244, 84, 92, 255]),
    };
    ink[3] = alpha;
    Some(MarkupStroke {
        tool,
        color: ink,
        width_px: width
            .filter(|width| width.is_finite() && *width >= 0.0)
            .unwrap_or(width_px),
        points,
    })
}

/// The ink an action names, as `#rrggbb`.
fn ink_of(ink: &str) -> Option<[u8; 4]> {
    let hex = ink.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(hex.get(range)?, 16).ok();
    Some([byte(0..2)?, byte(2..4)?, byte(4..6)?, 255])
}

/// One of an action's points, as `x,y` in the picture's own pixels.
fn point_of(point: &str) -> Option<Point> {
    let (x, y) = point.split_once(',')?;
    Some(Point::new(x.trim().parse().ok()?, y.trim().parse().ok()?))
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
        theme: &Theme,
        node_map: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        // The bar covers the whole surface: it is an overlay over what it
        // captures, not a row of it. The markup editor stands over it the
        // same way, while it is up.
        let mut kids = Vec::with_capacity(2);
        kids.push(self.widget.build_layout(taffy, theme, node_map));
        let markup = self
            .markup
            .as_mut()
            .map(|markup| markup.widget.build_layout(taffy, theme, node_map));
        kids.extend(markup);
        let only = match kids.as_slice() {
            // Nothing but the bar: it is the surface's own node, as it always
            // was.
            [only] => Some(*only),
            _ => None,
        };
        if let Some(only) = only {
            node_map.insert(only, std::ptr::from_mut::<dyn Widget>(self));
            return only;
        }
        let style = ui::taffy::Style {
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::percent(1.0),
                height: ui::taffy::Dimension::percent(1.0),
            },
            ..ui::taffy::Style::default()
        };
        taffy.widget_node(style, &kids, node_map, self)
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::vello::kurbo::Point,
    ) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let inner = ui::vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        let kids = taffy.children(node).unwrap_or_default();
        if self.markup.is_some() {
            if let Some(&bar) = kids.first() {
                self.widget.apply_layout(taffy, bar, inner);
            }
            if let (Some(markup), Some(&node)) = (self.markup.as_mut(), kids.get(1)) {
                self.widget.set_overlay_rect(self.rect);
                markup.widget.apply_layout(taffy, node, inner);
            }
        } else {
            self.widget.set_overlay_rect(self.rect);
            self.widget.apply_layout(taffy, node, origin);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn update(&mut self, _state: &tilo_ui_base::widget::AppModel, _path: &tilo_ui_base::widget::ModelPath) -> bool {
        // What the session published, and whether it has the studio up, live
        // outside the document: a scene the studio has not shown yet is what
        // this frame is for, so it is never a frame that syncs nothing.
        let behind = self.behind();
        if behind {
            self.catch_up();
        }
        behind
    }

    fn sync(&mut self, _state: &tilo_ui_base::widget::AppModel, _path: &tilo_ui_base::widget::ModelPath) {
        self.catch_up();
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.catch_up();
        match self.markup.as_mut() {
            // The editor is the whole studio while it is up: what it marks up
            // is the picture, not the bar that kept it.
            Some(markup) => {
                markup
                    .widget
                    .draw_child(scene, transform, text_renderer, theme, &env);
            }
            None => self
                .widget
                .draw_child(scene, transform, text_renderer, theme, &env),
        }
        self.take_markup_commands();
        self.take_intents();
        self.chip = match &self.saying {
            Some((said, _)) if self.markup.is_none() => {
                said_chip(scene, transform, text_renderer, theme, self.rect, said)
            }
            _ => Rect::ZERO,
        };
        // What this frame drew is what the link and the session said of it.
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match self.markup.as_mut() {
            Some(markup) => markup.widget.handle_event(event, ctx),
            None => self.widget.handle_event(event, ctx),
        }
        self.take_markup_commands();
        self.take_intents();
    }

    fn needs_redraw(&self) -> bool {
        if let Some(markup) = &self.markup {
            return self.dirty
                || self.behind()
                || markup.widget.needs_redraw()
                || markup.widget.needs_animation_frame();
        }
        self.dirty
            || self.behind()
            || (self.shown_open()
                && (self.widget.needs_redraw() || self.widget.needs_animation_frame()))
    }

    fn needs_animation_frame(&self) -> bool {
        if let Some(markup) = &self.markup {
            return markup.widget.needs_animation_frame();
        }
        // With the bar down nothing of it is drawn: it runs for its actions
        // and the scene, not to animate what nobody sees.
        self.shown_open() && self.widget.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let said = match (self.sending, &self.saying) {
            (false, Some((_, since))) => since.checked_add(SENT_SHOWN),
            _ => None,
        };
        let shown = match &self.markup {
            Some(markup) => markup.widget.next_wake_deadline(),
            None => self.widget.next_wake_deadline(),
        };
        match (shown, said) {
            (Some(shown), Some(said)) => Some(shown.min(said)),
            (shown, said) => shown.or(said),
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        match &self.markup {
            Some(markup) => markup.widget.wants_pointer_capture(),
            None => self.widget.wants_pointer_capture(),
        }
    }

    fn cursor_shape(
        &self,
        point: ui::vello::kurbo::Point,
    ) -> Option<ui::winit::window::CursorIcon> {
        match &self.markup {
            Some(markup) => markup.widget.cursor_shape(point),
            None => self.widget.cursor_shape(point),
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        match &self.markup {
            Some(markup) => markup.widget.collect_damage(regions),
            None => self.widget.collect_damage(regions),
        }
    }

    /// The editor appearing or going changes the tree under the surface:
    /// its node comes or goes with it.
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.markup_changed);
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn children(&self) -> Vec<&dyn Widget> {
        match &self.markup {
            Some(markup) => vec![&markup.widget as &dyn Widget],
            None => Vec::new(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match &mut self.markup {
            Some(markup) => vec![&mut markup.widget as &mut (dyn Widget + '_)],
            None => Vec::new(),
        }
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        if let Some(markup) = &self.markup {
            return markup.widget.accessibility();
        }
        let said = self.saying.as_ref().map(|(said, _)| {
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Label)
                .with_name(said.clone())
                .with_live(tilo_ui_base::a11y::Politeness::Polite)
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
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let pressed = match self.markup.as_mut() {
            Some(markup) => markup
                .widget
                .perform_semantic_action(target, action, state, path),
            None => self
                .widget
                .perform_semantic_action(target, action, state, path),
        };
        self.take_markup_commands();
        self.take_intents();
        pressed
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = match &self.markup {
            Some(markup) => markup.widget.test_rects(),
            None => self.widget.test_rects(),
        };
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
            effect @ CaptureEffect::Annotate { .. } => return self.annotate(effect),
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

    /// A shot taken at the shell once the person put the studio away says so
    /// on the floating layer; the bar does not come back over the shell for it.
    #[test]
    fn a_shot_from_the_shell_does_not_bring_the_studio_back() {
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

        // A shot taken from the shell answers where the bar is down: on the
        // floating layer, with nothing opening over the shell.
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
    /// with it: the next capture answers on the shell, not behind a studio the
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
        // goes with it rather than over a shell nobody sees it on.
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
