//! The shell narrates its own chrome: with a recording speaker behind the
//! toolkit screen reader, moving focus in Quick Settings speaks the focused
//! control, a notification's arrival is spoken politely (an error cuts
//! in), and a hardware-key readout is announced.

use std::sync::{Arc, Mutex};

use settings_store::hardware::QuickSettingsConfig;
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::shell_narrator::ShellNarrator;
use taffy::{NodeId, TaffyTree};
use ui::screen_reader::ScreenReader;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::support::host::{layout_host, one_workspace_tree, paint, press_key};

#[derive(Clone, Default)]
struct RecordingSpeaker(Arc<Mutex<Vec<tts::Utterance>>>);

impl RecordingSpeaker {
    fn take(&self) -> Vec<tts::Utterance> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl tts::Speaker for RecordingSpeaker {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        self.0.lock().unwrap().push(utterance);
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

/// A frame's update, which runs before the frame is narrated: what
/// arrived since the last frame is announced in this one, before it is
/// painted.
fn next_frame(host: &mut ShellOverlayHost) {
    use ui::widget::Widget as _;
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
}

fn press(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, key: NamedKey) {
    press_key(host, taffy, node, Key::Named(key), ModifiersState::empty());
    // The panel answers on its next frame, as it would on screen.
    host.force_quick_settings_settled();
}

#[test]
fn tab_in_quick_settings_speaks_the_focused_control() {
    let (host, chrome) = build_tilo_overlay_host_with_chrome(&one_workspace_tree());
    let mut host = host.with_quick_settings_config(QuickSettingsConfig::default());
    let speaker = RecordingSpeaker::default();
    let mut narrator =
        ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(speaker.clone())));
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.brightness_available = true;
        // A volume slider shows only once the sound output has been read.
        state.quick_settings.status.volume = Some(media::audio::default_node::VolumeState {
            level: 0.4,
            muted: false,
        });
        state.bump();
    }
    let _ = layout_host(&mut host);
    host.force_quick_settings_settled();
    let (taffy, node) = layout_host(&mut host);
    // The panel takes the keyboard as it opens, on its first control.
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    let first = spoken
        .first()
        .expect("opening speaks the control it lands on");
    assert!(
        first.text.starts_with("Brightness, slider, "),
        "got {:?}",
        first.text
    );
    assert!(first.interrupt, "a focus move interrupts older speech");

    narrator.observe(&host.chrome_narration_snapshot());
    assert!(speaker.take().is_empty(), "an unchanged frame is silent");

    // The volume's icon is its mute switch, one stop before the slider.
    press(&mut host, &taffy, node, NamedKey::Tab);
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert!(
        spoken.first().is_some_and(|u| u.text.starts_with("Mute, ")),
        "got {spoken:?}"
    );
    press(&mut host, &taffy, node, NamedKey::Tab);
    narrator.observe(&host.chrome_narration_snapshot());
    drop(host);
    let spoken = speaker.take();
    assert!(
        spoken
            .first()
            .is_some_and(|u| u.text.starts_with("Volume, slider, ")),
        "got {spoken:?}"
    );
}

#[test]
fn a_notification_arrival_is_spoken_politely_and_an_error_cuts_in() {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&one_workspace_tree());
    let speaker = RecordingSpeaker::default();
    let mut narrator =
        ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(speaker.clone())));
    // The desktop has been painted, its banners with it.
    let _ = layout_host(&mut host);
    paint(&mut host);
    narrator.observe(&host.chrome_narration_snapshot());
    assert!(speaker.take().is_empty());

    chrome.borrow_mut().push_notification("New mail", "Lunch?");
    next_frame(&mut host);
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert_eq!(spoken.len(), 1, "got {spoken:?}");
    assert_eq!(spoken[0].text, "New mail. Lunch?");
    assert_eq!(spoken[0].priority, tts::Priority::Notification);
    assert!(!spoken[0].interrupt, "a polite arrival never interrupts");

    chrome
        .borrow_mut()
        .push_error_notification("Backup failed", "The drive is not connected");
    next_frame(&mut host);
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert_eq!(
        spoken
            .first()
            .map(|u| (u.text.as_str(), u.priority, u.interrupt)),
        Some((
            "Backup failed. The drive is not connected",
            tts::Priority::Important,
            true
        ))
    );

    chrome
        .borrow_mut()
        .show_osd(shell::shell::osd::Osd::volume(0.5, false));
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert!(
        spoken.iter().any(|u| u.text == "Volume 50%"),
        "a hardware-key readout is announced: {spoken:?}"
    );
}
