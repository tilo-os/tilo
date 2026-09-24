//! Host services that record every effect instead of performing it — for
//! tests, and for any host that must observe an app without letting it reach
//! the desktop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    FilePicker, HostServices, MediaSession, MediaSessionFactory, MediaSessionPublisher,
    Notification, Notifier, NotifyError, SpeechFactory, UriOpener, answered,
};
use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
use crate::mpris::{MprisConfig, PlayerSnapshot};
use crate::portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome,
};

#[derive(Default)]
struct Log {
    copies: Vec<(ClipboardTarget, String)>,
    opened_uris: Vec<String>,
    pick_requests: Vec<PickRequest>,
    launch_requests: Vec<LaunchRequest>,
    pick_outcomes: VecDeque<PortalOutcome>,
    multi_outcomes: VecDeque<MultiOutcome>,
    open_file_outcomes: VecDeque<OpenFileOutcome>,
    launch_outcomes: VecDeque<LaunchOutcome>,
    utterances: Vec<tts::Utterance>,
    notifications: Vec<Notification>,
    media_sessions: Vec<MprisConfig>,
    published: Vec<PlayerSnapshot>,
}

/// What a [`HostServices::recording`] host was asked to do, and the answers
/// it will give.
///
/// Copies round-trip through an in-process clipboard; pickers and launches
/// answer queued outcomes, then `Cancelled` (launches: `Opened`); URIs, utterances
/// and notifications are recorded; media sessions start and record what the
/// player publishes.
#[derive(Clone, Default)]
pub struct HostRecorder {
    log: Arc<Mutex<Log>>,
}

impl HostRecorder {
    fn log(&self) -> MutexGuard<'_, Log> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every text copy, in order.
    pub fn copies(&self) -> Vec<(ClipboardTarget, String)> {
        self.log().copies.clone()
    }

    /// Every URI handed to the opener, in order.
    pub fn opened_uris(&self) -> Vec<String> {
        self.log().opened_uris.clone()
    }

    /// Every chooser request, in order.
    pub fn pick_requests(&self) -> Vec<PickRequest> {
        self.log().pick_requests.clone()
    }

    /// Every desktop-open request, in order.
    pub fn launch_requests(&self) -> Vec<LaunchRequest> {
        self.log().launch_requests.clone()
    }

    /// Answer the next single-file chooser with `outcome`.
    pub fn respond_to_pick(&self, outcome: PortalOutcome) {
        self.log().pick_outcomes.push_back(outcome);
    }

    /// Answer the next multi-select chooser with `outcome`.
    pub fn respond_to_pick_multiple(&self, outcome: MultiOutcome) {
        self.log().multi_outcomes.push_back(outcome);
    }

    /// Answer the next descriptor-retaining chooser with `outcome`.
    pub fn respond_to_open_file(&self, outcome: OpenFileOutcome) {
        self.log().open_file_outcomes.push_back(outcome);
    }

    /// Answer the next desktop-open request with `outcome`.
    pub fn respond_to_launch(&self, outcome: LaunchOutcome) {
        self.log().launch_outcomes.push_back(outcome);
    }

    /// Everything the screen reader was asked to say, in order.
    pub fn utterances(&self) -> Vec<tts::Utterance> {
        self.log().utterances.clone()
    }

    /// Every notification shown, in order.
    pub fn notifications(&self) -> Vec<Notification> {
        self.log().notifications.clone()
    }

    /// The configuration of every media session started, in order.
    pub fn media_sessions(&self) -> Vec<MprisConfig> {
        self.log().media_sessions.clone()
    }

    /// Every player state published to a media session, in order.
    pub fn published_media_state(&self) -> Vec<PlayerSnapshot> {
        self.log().published.clone()
    }
}

impl HostServices {
    /// Services that record every effect in the returned [`HostRecorder`].
    pub fn recording() -> (Self, HostRecorder) {
        let recorder = HostRecorder::default();
        let services = Self {
            clipboard: Arc::new(RecordingClipboard {
                recorder: recorder.clone(),
                buffer: InProcessClipboard::default(),
            }),
            files: Arc::new(recorder.clone()),
            opener: Arc::new(recorder.clone()),
            speech: Arc::new(recorder.clone()),
            notifications: Arc::new(recorder.clone()),
            media_session: Arc::new(recorder.clone()),
        };
        (services, recorder)
    }
}

struct RecordingClipboard {
    recorder: HostRecorder,
    buffer: InProcessClipboard,
}

impl ClipboardBackend for RecordingClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.recorder.log().copies.push((target, text.to_string()));
        self.buffer.set_text(target, text)
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        self.buffer.text(target)
    }

    fn name(&self) -> &'static str {
        "recording"
    }
}

impl FilePicker for HostRecorder {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        let mut log = self.log();
        log.pick_requests.push(request);
        answered(
            log.pick_outcomes
                .pop_front()
                .unwrap_or(PortalOutcome::Cancelled),
        )
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        let mut log = self.log();
        log.pick_requests.push(request);
        answered(
            log.multi_outcomes
                .pop_front()
                .unwrap_or(MultiOutcome::Cancelled),
        )
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        let mut log = self.log();
        log.pick_requests.push(request);
        answered(
            log.open_file_outcomes
                .pop_front()
                .unwrap_or(OpenFileOutcome::Cancelled),
        )
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        let mut log = self.log();
        log.launch_requests.push(request);
        answered(
            log.launch_outcomes
                .pop_front()
                .unwrap_or(LaunchOutcome::Opened),
        )
    }
}

impl UriOpener for HostRecorder {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        self.log().opened_uris.push(uri.to_string());
        Ok(())
    }
}

struct RecordingSpeaker {
    recorder: HostRecorder,
}

impl tts::Speaker for RecordingSpeaker {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        self.recorder.log().utterances.push(utterance);
    }

    fn stop(&mut self) {}

    fn set_rate(&mut self, _rate: f32) {}

    fn set_voice(&mut self, _voice: &str) {}

    fn is_speaking(&self) -> bool {
        false
    }

    fn backend_name(&self) -> &'static str {
        "recording"
    }
}

impl SpeechFactory for HostRecorder {
    fn make(&self) -> Box<dyn tts::Speaker> {
        Box::new(RecordingSpeaker {
            recorder: self.clone(),
        })
    }
}

impl Notifier for HostRecorder {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.log().notifications.push(notification.clone());
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        self.log().notifications.retain(|shown| shown.tag != tag);
        Ok(())
    }
}

impl MediaSessionPublisher for HostRecorder {
    fn publish(&mut self, snapshot: PlayerSnapshot) {
        self.log().published.push(snapshot);
    }
}

impl MediaSessionFactory for HostRecorder {
    fn start(&self, config: MprisConfig, _initial: PlayerSnapshot) -> Option<MediaSession> {
        self.log().media_sessions.push(config);
        // The sender is dropped: a recording host never sends commands.
        let (_commands, receiver) = std::sync::mpsc::channel();
        Some(MediaSession {
            commands: receiver,
            publisher: Box::new(self.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolkit_entry_points_reach_the_recording_host() {
        let (services, recorder) = HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened("/tmp/picked.txt".into()));
        super::super::scoped(&services, || {
            crate::text_selection::clipboard_copy("copied");
            assert_eq!(
                crate::text_selection::clipboard_paste().as_deref(),
                Some("copied")
            );
            assert!(crate::widget::try_open_external_url("https://tilo.so").is_ok());
            let picked = PickRequest::open("Open", "Open").send().recv();
            assert_eq!(picked, Ok(PortalOutcome::Opened("/tmp/picked.txt".into())));
            let mut reader = crate::screen_reader::ScreenReader::new();
            reader.announce_focus(
                &crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name("Save"),
            );
        });
        assert_eq!(
            recorder.copies(),
            [(ClipboardTarget::Clipboard, "copied".to_string())]
        );
        assert_eq!(recorder.opened_uris(), ["https://tilo.so"]);
        assert_eq!(recorder.pick_requests().len(), 1);
        assert!(
            recorder
                .utterances()
                .iter()
                .any(|utterance| utterance.text.contains("Save"))
        );
    }

    #[test]
    fn media_sessions_record_published_state() {
        let (services, recorder) = HostServices::recording();
        let mut handle = super::super::scoped(&services, || {
            crate::mpris::MprisHandle::spawn(
                MprisConfig::new("player", "Player"),
                PlayerSnapshot::default(),
            )
        });
        assert!(handle.is_active());
        handle.publish(PlayerSnapshot {
            title: "Now playing".into(),
            ..PlayerSnapshot::default()
        });
        assert_eq!(recorder.media_sessions().len(), 1);
        assert_eq!(recorder.published_media_state()[0].title, "Now playing");
    }
}
