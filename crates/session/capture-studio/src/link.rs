//! What the capture studio asks of the session that shows it, and what the
//! session hands it.
//!
//! The studio draws, the session captures: only the session has the
//! compositor, so the studio puts what the person chose as [`Ask`]s and
//! reads what came of them from [`Scene`]. Both sides hold one [`Link`] in
//! one process.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tilo_sheets::{Presentation, Seat};
use ui::vello::kurbo::Rect;
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CapturePreview, CaptureTarget, RegionAnchor,
    ScreenshotKept, ScreenshotSendDevice, SourceMode,
};

/// How a capture is made: what of the desktop, and which parts of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    /// What is captured: a selection, windows, the display or the strip.
    pub source: SourceMode,
    /// The selections, when `source` is [`SourceMode::Region`], in the
    /// studio's own coordinates.
    pub regions: Vec<Rect>,
    /// One anchor per region made wholly inside a window, in the same
    /// order: what is captured is then that part of that window.
    pub anchors: Vec<RegionAnchor>,
    /// The windows captured whole ([`SourceMode::Window`]), by their
    /// [`CaptureTarget::id`].
    pub targets: Vec<String>,
    /// Whether the pointer is drawn into what is captured.
    pub include_pointer: bool,
    /// Which sound is recorded with it: the machine's own, a microphone, or
    /// both. A screenshot takes none.
    pub audio: AudioCaptureMode,
    /// Where what is captured goes: a file, the clipboard, or both.
    pub destination: CaptureDestination,
}

impl Default for Take {
    fn default() -> Self {
        Self {
            source: SourceMode::Region,
            regions: Vec::new(),
            anchors: Vec::new(),
            targets: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            destination: CaptureDestination::File,
        }
    }
}

/// A screenshot that was taken.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    /// What became of it.
    pub kept: ScreenshotKept,
    /// The file it was written to.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// The thumbnail the receipt shows.
    pub preview: CapturePreview,
}

/// A recording that runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    /// The file it is written to.
    pub path: PathBuf,
    pub paused: bool,
    /// Frames the encoder was too far behind to take so far: the file is
    /// shorter by them, and the person is told.
    pub skipped: u64,
}

/// A recording that ended, while its receipt says what it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// The file it wrote, while one is there to open: a recording that could
    /// not be finished leaves what it wrote, unless it wrote nothing.
    pub path: Option<PathBuf>,
    /// Frames the encoder was too far behind to take: what the file is
    /// missing.
    pub skipped: u64,
    /// Why the file could not be finished, when it could not: it carries no
    /// trailer, so it is not whole.
    pub failure: Option<String>,
}

/// What became of sending the last screenshot: to one of the person's other
/// devices, or into this workspace's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// Offered to one of the person's other devices, as the person sees it
    /// named: its person accepts it there.
    Offered {
        /// The device it went to.
        device: String,
    },
    /// Nothing was offered to one of the person's other devices, and why.
    Refused {
        /// The device it did not go to.
        device: String,
        /// Why it was not offered.
        why: Refusal,
    },
    /// Copied into this workspace's files under `name`: the name the person
    /// finds it by there.
    Saved {
        /// The file's name in the workspace's `Screenshots` folder.
        name: String,
    },
    /// Nothing was written into this workspace's files, and why.
    NotSaved {
        /// Why it was not saved.
        why: Refusal,
    },
}

impl Sent {
    /// Offered to `device`: its person accepts it there.
    #[must_use]
    pub fn offered(device: String) -> Self {
        Self::Offered { device }
    }

    /// Nothing was offered to `device`, and why.
    #[must_use]
    pub fn refused(device: String, why: Refusal) -> Self {
        Self::Refused { device, why }
    }

    /// Saved into the workspace's files under `name`.
    #[must_use]
    pub fn saved(name: String) -> Self {
        Self::Saved { name }
    }

    /// Nothing was saved into the workspace's files, and why.
    #[must_use]
    pub fn not_saved(why: Refusal) -> Self {
        Self::NotSaved { why }
    }
}

/// Why a capture the person took did not come out as they asked, in the
/// session's words: the bar says it where it says what became of a send, so
/// the reason stays on the thing the person touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A recording the person started did not begin, and why.
    RecordingNotStarted(String),
    /// A recording the person started stopped short, and why.
    RecordingStopped(String),
    /// The last half minute could not be kept, and why.
    ReplayNotStarted(String),
    /// The replay buffer stopped keeping the last half minute, and why.
    ReplayStopped(String),
    /// A source of a recording's sound stopped: it carries no more of it.
    SoundLost,
    /// A screenshot could not be read back off the desktop, and why.
    NotTaken(String),
    /// A screenshot's file could not be written: where it was to go, and what
    /// stopped it. The system's own error stays out of what the person reads.
    NotWritten {
        /// The folder the file was to be written in.
        folder: PathBuf,
        /// What stopped the write.
        cause: WriteCause,
    },
    /// A screenshot was written but not put on the clipboard, and why.
    NotCopied(String),
    /// A screenshot could not be read back for the clipboard, and why.
    NotRead(String),
    /// A clip of what just happened could not be saved, and why.
    ClipNotSaved(String),
    /// A capture could not be opened in the person's app for it, and why.
    NotOpened {
        /// The capture it did not open.
        path: PathBuf,
        /// What the system said, in its own words.
        error: String,
    },
}

/// Why a screenshot's file could not be written. The person reads it in their
/// own words — "Tilo can't write to this folder" rather than an error number —
/// so the reason a folder refused a file is told apart here, where both the
/// bar that says it and the card that offers to copy it instead can name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteCause {
    /// The folder is the person's, but not Tilo's to write in.
    Permission,
    /// The disk has no room left for the picture.
    NoRoom,
    /// The disk it would go on cannot be written at all.
    ReadOnly,
    /// There is no folder there to write into.
    Folder,
    /// Something else stopped the write.
    Other,
}

impl WriteCause {
    /// What stopped writing `error`: the system's own error kind, told apart
    /// without ever being shown to the person.
    #[must_use]
    pub fn of(error: &std::io::Error) -> Self {
        if let Some(code) = error.raw_os_error() {
            return match code {
                // EACCES, EPERM
                13 | 1 => Self::Permission,
                // ENOSPC, EDQUOT
                28 | 122 => Self::NoRoom,
                // EROFS
                30 => Self::ReadOnly,
                // ENOENT, ENOTDIR
                2 | 20 => Self::Folder,
                _ => Self::Other,
            };
        }
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => Self::Permission,
            std::io::ErrorKind::NotFound => Self::Folder,
            _ => Self::Other,
        }
    }

    /// The message a person reads of it, named the same by the bar and by the
    /// floating layer's card.
    #[must_use]
    pub fn words(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::NoRoom => "no-room",
            Self::ReadOnly => "read-only",
            Self::Folder => "folder",
            Self::Other => "other",
        }
    }
}

/// A failure of the last capture the person took, and a number that tells two
/// alike failures apart: the bar says each one again, so taking the same
/// capture twice says the same thing twice.
#[derive(Clone, Debug, PartialEq)]
pub struct Failed {
    /// What changes with every failure, so a repeat says itself again.
    pub number: u64,
    /// What went wrong.
    pub failure: Failure,
}

/// Why a screenshot did not go where it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// This session runs on no device fabric, so it reaches none of the
    /// person's other devices.
    SyncOff,
    /// The device stopped being one this session can reach before the send.
    Offline,
    /// The screenshot's file is no longer where the studio wrote it.
    Gone { path: PathBuf },
    /// Nothing was captured to send.
    NotKept,
    /// The file has no folder to be read from.
    NoFolder { path: PathBuf },
    /// Larger than one Nearby send carries.
    TooLarge { bytes: u64, limit: u64 },
    /// No workspace is mounted for this session, so its files are not here.
    NoWorkspace,
    /// The file has no name of its own to be saved under.
    NoFileName { path: PathBuf },
    /// The workspace's `Screenshots` folder could not be made, in the
    /// filesystem's words.
    CreateFailed { folder: PathBuf, error: String },
    /// The copy could not be written, in the filesystem's words.
    NotSaved(String),
    /// Every name the screenshot could be saved under in `folder` is taken.
    NoFreeName { folder: PathBuf },
    /// The device's fabric or spool would not take it, in its own words.
    Refused(String),
}

/// What an app asked the person to select, when it asked for something only
/// they can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// One window the person picks.
    Window,
    /// An area the person draws.
    Area,
}

impl Selection {
    /// What the studio captures for it.
    #[must_use]
    pub const fn source(self) -> SourceMode {
        match self {
            Self::Window => SourceMode::Window,
            Self::Area => SourceMode::Region,
        }
    }
}

/// What the session hands the studio: what can be captured, and what became
/// of what was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// The windows a capture may take, focused first.
    pub windows: Vec<CaptureTarget>,
    /// What an app asked the person to select: the bar comes up choosing
    /// exactly that, and settles the app's request once they took it.
    pub asking: Option<Selection>,
    /// Whether the machine's own sound and a microphone may be taken.
    pub system_audio: bool,
    pub microphone: bool,
    /// The last screenshot, while its receipt shows.
    pub kept: Option<Kept>,
    /// The person's other devices that are here now: who a screenshot may be
    /// sent to, and what became of the last send.
    pub devices: Vec<ScreenshotSendDevice>,
    /// What became of sending the last screenshot, to a device or into the
    /// workspace's files, while its receipt shows that.
    pub sent: Option<Sent>,
    /// Why the last capture the person took did not come out, while the bar
    /// says so.
    pub failed: Option<Failed>,
    /// The recording that runs, and whether it is paused.
    pub recording: Option<Recording>,
    /// The recording that ended, while its receipt says what it made of it:
    /// where its file is, and why it could not be finished when it could
    /// not.
    pub finished: Option<Finished>,
    /// Seconds of the last half minute kept ready to clip, while that runs.
    pub replay: Option<u8>,
    /// Which sound that buffer records, so the bar says what a clip will
    /// carry.
    pub replay_audio: AudioCaptureMode,
    /// How many clips the replay buffer gave: each one is a receipt.
    pub replay_saves: u64,
    /// Changes with every scene the session publishes: a studio reads the
    /// scene again when it differs from the one it last showed.
    pub generation: u64,
}

impl Scene {
    /// Whether `other` says the same of the studio: its generation aside,
    /// which counts the scenes published rather than what they say.
    #[must_use]
    fn same(&self, other: &Self) -> bool {
        *self
            == Self {
                generation: self.generation,
                ..other.clone()
            }
    }
}

/// What the studio asks the session.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// Take a screenshot of what `Take` names.
    Screenshot(Take),
    /// Record what `Take` names into a file.
    StartRecording(Take),
    PauseRecording,
    ResumeRecording,
    StopRecording,
    /// Keep the last half minute ready to clip.
    StartReplay(Take),
    /// Save what the replay buffer holds now.
    SaveReplay,
    StopReplay,
    /// Open the capture at `path` in the person's app for it.
    Open(PathBuf),
    /// Show the capture at `path` in Files.
    Reveal(PathBuf),
    /// Copy the last screenshot to the clipboard again.
    CopyLast,
    /// Offer the last screenshot to one of the person's other devices, by its
    /// [`Scene::devices`] id.
    SendLastTo(String),
    /// Copy the last screenshot into this workspace's files, under
    /// `Screenshots`.
    SendLastToWorkspace,
}

#[derive(Default)]
struct Shared {
    scene: Scene,
    asks: Vec<Ask>,
    /// What the studio opens on when something outside it opened it: the
    /// shell, for an app that asked for a capture.
    opens_on: Option<ui::widget::CaptureMode>,
    wake: Option<ui::WakeHandle>,
}

/// The studio's side of the conversation and the session's, shared between
/// the two in one process.
#[derive(Clone, Default)]
pub struct Link {
    shared: Rc<RefCell<Shared>>,
}

impl Link {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand the studio `scene`: what happened since it last read.
    pub fn publish(&self, mut scene: Scene) {
        {
            let mut shared = self.shared.borrow_mut();
            if shared.scene.same(&scene) {
                // Nothing changed: the studio is not woken for it.
                return;
            }
            scene.generation = shared.scene.generation.wrapping_add(1);
            shared.scene = scene;
        }
        self.wake();
    }

    /// The scene published now.
    #[must_use]
    pub fn scene(&self) -> Scene {
        self.shared.borrow().scene.clone()
    }

    /// Whether anything changed since generation `seen`.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.shared.borrow().scene.generation
    }

    /// Put what the person chose in front of the session.
    pub fn ask(&self, ask: Ask) {
        self.shared.borrow_mut().asks.push(ask);
    }

    /// Everything asked since the last take, in order.
    #[must_use]
    pub fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.shared.borrow_mut().asks)
    }

    /// Whether the studio is asked to do anything yet.
    #[must_use]
    pub fn has_asks(&self) -> bool {
        !self.shared.borrow().asks.is_empty()
    }

    /// Open the bar on `mode` the next time it comes up: what somebody
    /// outside the studio opened it for (an app's screenshot, a recording, a
    /// picture in a picture).
    pub fn open_on(&self, mode: ui::widget::CaptureMode) {
        self.shared.borrow_mut().opens_on = Some(mode);
    }

    /// What the bar opens on, once: nothing after it took it.
    #[must_use]
    pub fn take_open_on(&self) -> Option<ui::widget::CaptureMode> {
        self.shared.borrow_mut().opens_on.take()
    }

    /// Wake the studio when the session hands it a new scene.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        self.shared.borrow_mut().wake = Some(wake);
    }

    fn wake(&self) {
        let wake = self.shared.borrow().wake.clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

/// The capture studio's seat: whether it is up (shared with the session) and
/// what the session lends it ([`Link`]). The session keeps a clone, opens
/// the studio and carries out what it asks.
#[derive(Clone, Default)]
pub struct Studio {
    presentation: Presentation,
    link: Link,
}

impl Studio {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn link(&self) -> &Link {
        &self.link
    }
}

impl Seat for Studio {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_scene_is_new_once_and_asks_are_taken_in_order() {
        let link = Link::new();
        assert_eq!(link.generation(), 0, "nothing published yet");
        link.publish(Scene::default());
        assert_eq!(link.generation(), 0, "a scene that says nothing is not new");
        link.publish(Scene {
            system_audio: true,
            ..Scene::default()
        });
        let first = link.generation();
        assert_eq!(first, 1, "published once");
        link.publish(Scene {
            system_audio: true,
            ..Scene::default()
        });
        assert_eq!(link.generation(), first, "the same scene is not new");
        link.publish(Scene::default());
        assert_eq!(link.generation(), first + 1, "another scene is");
        link.ask(Ask::SaveReplay);
        link.ask(Ask::StopRecording);
        assert!(link.has_asks());
        assert_eq!(link.take_asks(), [Ask::SaveReplay, Ask::StopRecording]);
        assert!(!link.has_asks());
    }

    #[test]
    fn what_the_session_asks_the_person_to_select_wakes_the_studio_for_it() {
        let link = Link::new();
        link.publish(Scene::default());
        link.publish(Scene {
            asking: Some(Selection::Area),
            ..Scene::default()
        });
        assert_eq!(link.generation(), 1, "the bar comes up asking for an area");
        assert_eq!(link.scene().asking, Some(Selection::Area));
        assert_eq!(Selection::Area.source(), SourceMode::Region);
        assert_eq!(Selection::Window.source(), SourceMode::Window);
    }

    #[test]
    fn what_an_app_opened_the_bar_for_is_taken_once() {
        use ui::widget::CaptureMode;
        let link = Link::new();
        assert_eq!(link.take_open_on(), None, "nothing opened it yet");
        link.open_on(CaptureMode::Pip);
        assert_eq!(link.take_open_on(), Some(CaptureMode::Pip));
        assert_eq!(link.take_open_on(), None, "the bar opens on it once");
        link.open_on(CaptureMode::Recording);
        assert_eq!(link.take_open_on(), Some(CaptureMode::Recording));
    }

    #[test]
    fn what_the_session_hands_is_what_the_studio_reads_back() {
        let studio = Studio::new();
        studio.link().publish(Scene {
            system_audio: true,
            replay: Some(12),
            ..Scene::default()
        });
        let scene = studio.link().scene();
        assert!(scene.system_audio);
        assert_eq!(scene.replay, Some(12));
        assert!(!studio.presentation().is_drawn(), "closed until opened");
    }
}
