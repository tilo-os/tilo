//! The shell narrating its own chrome.
//!
//! Every Tilo app speaks through the toolkit's built-in
//! [`ui::screen_reader::ScreenReader`]; the compositor's own surfaces — Quick
//! Settings, the session menu, the lock screen, notifications and hardware
//! readouts — are drawn by the shell, not by an app, so nothing spoke for
//! them. This runs the same reader, through the same speech backend the apps
//! use, over the semantic snapshot the shell already publishes to AT-SPI.
//!
//! Each frame the narrator is handed the chrome's accessibility tree:
//! a move of the focused node is spoken (interrupting), a state change on
//! the node that kept focus speaks only the new state, and a changed live
//! region is announced at its declared politeness.

use std::collections::HashMap;

use tilo_accessibility_settings::AccessibilitySettings;
use ui::a11y::{AccessNode, Politeness};
use ui::screen_reader::{ScreenReader, describe, describe_state};

/// The focused node of one snapshot, identified by its child-index path.
struct Focus {
    path: Vec<usize>,
    spoken: String,
    state: String,
}

#[derive(Default)]
pub struct ShellNarrator {
    reader: Option<ScreenReader>,
    focus: Option<(Vec<usize>, String, String)>,
    live: HashMap<String, String>,
    /// False until the first snapshot after enabling: live regions already
    /// on screen are recorded, not replayed.
    primed: bool,
}

impl std::fmt::Debug for ShellNarrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellNarrator")
            .field("enabled", &self.reader.is_some())
            .field("live_regions", &self.live.len())
            .finish_non_exhaustive()
    }
}

impl ShellNarrator {
    /// Narrate through `reader` (a recording speaker in tests).
    pub fn with_reader(reader: ScreenReader) -> Self {
        Self {
            reader: Some(reader),
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.reader.is_some()
    }

    /// Follow the `accessibility.screen_reader` setting. Turning it off stops
    /// speech mid-sentence; turning it on starts from the current snapshot
    /// without replaying what is already on screen.
    pub fn set_enabled(&mut self, enabled: bool) {
        match (enabled, self.reader.is_some()) {
            (true, false) => {
                // Rate and voice follow the same `accessibility` settings the
                // apps read, so the desktop speaks with one voice.
                let mut reader = ScreenReader::new();
                reader.configure(&AccessibilitySettings::load().screen_reader());
                self.reader = Some(reader);
            }
            (false, true) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.stop();
                }
                *self = Self::default();
            }
            _ => {}
        }
    }

    /// Speak what changed since the previous snapshot of the chrome.
    pub fn observe(&mut self, root: &AccessNode) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let mut focus = None;
        let mut live = Vec::new();
        let mut path = Vec::new();
        visit(root, &mut path, &mut focus, &mut live);

        match (focus, self.focus.as_ref()) {
            (Some(now), Some((path, spoken, _))) if &now.path == path && &now.spoken == spoken => {}
            (Some(now), Some((path, _, state))) if &now.path == path => {
                if now.state != *state && !now.state.is_empty() {
                    reader.announce_value_change(now.state.clone());
                }
                self.focus = Some((now.path, now.spoken, now.state));
            }
            (Some(now), _) => {
                reader.announce(now.spoken.clone(), tts::Priority::Message, true);
                self.focus = Some((now.path, now.spoken, now.state));
            }
            (None, _) => self.focus = None,
        }

        let mut present = std::collections::HashSet::new();
        for (key, politeness, text) in live {
            present.insert(key.clone());
            let changed = self.live.get(&key) != Some(&text);
            self.live.insert(key, text.clone());
            if !changed || text.is_empty() || !self.primed {
                continue;
            }
            let (priority, interrupt) = match politeness {
                Politeness::Assertive => (tts::Priority::Important, true),
                Politeness::Polite => (tts::Priority::Notification, false),
            };
            reader.announce(text, priority, interrupt);
        }
        self.live.retain(|key, _| present.contains(key));
        self.primed = true;
    }
}

fn visit(
    node: &AccessNode,
    path: &mut Vec<usize>,
    focus: &mut Option<Focus>,
    live: &mut Vec<(String, Politeness, String)>,
) {
    if node.state.hidden {
        return;
    }
    if node.state.focused {
        *focus = Some(Focus {
            path: path.clone(),
            spoken: describe(node),
            state: describe_state(node),
        });
    }
    if let (Some(politeness), Some(name)) = (node.live, node.name.as_deref()) {
        // A region with a value speaks the value ("New mail. Lunch?"), and
        // an empty value is silence; one without a value speaks its name
        // ("Notifications cleared").
        let text = node.value.clone().unwrap_or_else(|| name.to_string());
        live.push((name.to_string(), politeness, text));
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        visit(child, path, focus, live);
        path.pop();
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::compositor::lock_screen::{LockView, narration_tree};

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl tts::Speaker for Recorder {
        fn speak_utterance(&mut self, utterance: tts::Utterance) {
            if let Ok(mut spoken) = self.0.lock() {
                spoken.push(utterance.text);
            }
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

    fn view(password_len: usize, error: Option<&str>) -> LockView<'_> {
        LockView {
            password_len,
            error,
            clock: "09:41",
            date: "Friday, 11 July",
            notification_count: 0,
            verifying: false,
            now_playing: None,
            keyboard: None,
            alert: None,
        }
    }

    #[test]
    fn the_lock_screen_speaks_its_field_never_the_typed_length_and_errors_assertively() {
        let recorder = Recorder::default();
        let mut narrator =
            ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(recorder.clone())));
        narrator.observe(&narration_tree(&view(0, None)));
        narrator.observe(&narration_tree(&view(5, None)));
        narrator.observe(&narration_tree(&view(0, Some("Authentication failure"))));
        let spoken = recorder.0.lock().unwrap().clone();
        assert!(
            spoken
                .first()
                .is_some_and(|text| text.starts_with("Password") && text.contains("protected")),
            "the focused password field is announced as protected: {spoken:?}"
        );
        assert!(
            !spoken.iter().any(|text| text.contains('5')),
            "the typed length is never spoken: {spoken:?}"
        );
        assert_eq!(spoken.len(), 2, "field, then the failure: {spoken:?}");
    }

    #[test]
    fn turning_the_reader_off_silences_it() {
        let recorder = Recorder::default();
        let mut narrator =
            ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(recorder.clone())));
        narrator.set_enabled(false);
        assert!(!narrator.enabled());
        narrator.observe(&narration_tree(&view(0, None)));
        assert!(recorder.0.lock().unwrap().is_empty());
    }
}
