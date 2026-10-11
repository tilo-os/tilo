//! Session-owned two-switch accessibility policy.
//!
//! Physical switches normally identify as ordinary keyboards. We therefore
//! translate only a user-selected key pair into the universal Tab/Enter
//! navigation contract, inside the compositor's existing hardware-input
//! funnel. Disabled means byte-for-byte ordinary delivery; no virtual input
//! protocol or app-controlled injection capability is introduced.

use std::time::{Duration, Instant};

use settings_store::accessibility::{AccessibilitySettings, SwitchKeyPair};

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchAction {
    Next,
    Activate,
}

#[derive(Clone, Copy, Debug)]
struct Config {
    enabled: bool,
    pair: SwitchKeyPair,
    scan: Option<Duration>,
}

impl Config {
    fn from_settings(settings: &settings_store::live::Snapshot) -> Self {
        Self::from_section(&settings.section(settings_store::accessibility::SECTION))
    }

    fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let settings = AccessibilitySettings::from_section(section);
        Self {
            enabled: settings.switch_control,
            pair: settings.switch_key_pair,
            scan: settings.switch_scan(),
        }
    }
}

/// Follows the session's live settings: a change made in Settings applies on
/// the next key or loop tick after the settings thread has read it, and a
/// key between changes costs one atomic load — never a file read.
pub struct SwitchControlRuntime {
    settings: settings_store::live::LiveSettings,
    /// The settings generation `config` was read from.
    seen: u64,
    config: Config,
    next_scan: Option<Instant>,
}

/// `now + delta`, saturating to `now` itself on the (cosmically unlikely) `Instant` overflow
/// rather than panicking — a scan deadline that cannot be represented becomes "due now"
/// instead of crashing the compositor.
fn after(now: Instant, delta: Duration) -> Instant {
    now.checked_add(delta).unwrap_or(now)
}

impl SwitchControlRuntime {
    pub fn new(now: Instant, settings: &settings_store::live::LiveSettings) -> Self {
        let snapshot = settings.snapshot();
        let config = Config::from_settings(&snapshot);
        Self {
            settings: settings.clone(),
            seen: snapshot.generation,
            next_scan: config
                .scan
                .filter(|_| config.enabled)
                .map(|d| after(now, d)),
            config,
        }
    }

    fn refresh(&mut self, now: Instant) {
        if self.settings.generation() == self.seen {
            return;
        }
        let snapshot = self.settings.snapshot();
        self.seen = snapshot.generation;
        let old = self.config;
        self.config = Config::from_settings(&snapshot);
        if old.enabled != self.config.enabled || old.scan != self.config.scan {
            self.next_scan = self
                .config
                .scan
                .filter(|_| self.config.enabled)
                .map(|d| after(now, d));
        }
    }

    /// A mapped physical key action and its press/release state. `None` is a
    /// strict pass-through, including every event while disabled.
    pub fn translate(&mut self, event: &WindowEvent, now: Instant) -> Option<(SwitchAction, bool)> {
        self.refresh(now);
        if !self.config.enabled {
            return None;
        }
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return None;
        };
        let action = match (self.config.pair, &event.logical_key) {
            (SwitchKeyPair::F6F7, Key::Named(NamedKey::F6))
            | (SwitchKeyPair::F9F10, Key::Named(NamedKey::F9)) => SwitchAction::Next,
            (SwitchKeyPair::Digits12, Key::Character(c)) if c == "1" => SwitchAction::Next,
            (SwitchKeyPair::F6F7, Key::Named(NamedKey::F7))
            | (SwitchKeyPair::F9F10, Key::Named(NamedKey::F10)) => SwitchAction::Activate,
            (SwitchKeyPair::Digits12, Key::Character(c)) if c == "2" => SwitchAction::Activate,
            _ => return None,
        };
        if action == SwitchAction::Next && event.state == ElementState::Pressed {
            self.next_scan = self.config.scan.map(|d| after(now, d));
        }
        Some((action, event.state == ElementState::Pressed))
    }

    pub fn tick(&mut self, now: Instant) -> Option<SwitchAction> {
        self.refresh(now);
        let due = self.config.enabled && self.next_scan.is_some_and(|at| now >= at);
        if !due {
            return None;
        }
        self.next_scan = self.config.scan.map(|d| after(now, d));
        Some(SwitchAction::Next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::SettingsSource as _;
    use settings_store::live::LiveSettings;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use winit::event::DeviceId;

    fn key(logical_key: Key, pressed: bool) -> WindowEvent {
        let event = ui::synthetic_input::key_event(
            logical_key,
            None,
            if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
        );
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed::<DeviceId>() },
            event,
            is_synthetic: false,
        }
    }

    /// A settings document that counts its reads, so a test can prove a
    /// key between changes reads nothing.
    struct CountingSource {
        document: settings_store::HostDocument,
        loads: AtomicUsize,
    }

    impl settings_store::SettingsSource for CountingSource {
        fn load(&self) -> serde_json::Map<String, serde_json::Value> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.document.load()
        }

        fn update_section(
            &self,
            name: &str,
            values: serde_json::Map<String, serde_json::Value>,
        ) -> std::io::Result<()> {
            self.document.update_section(name, values)
        }

        fn revision(&self) -> u64 {
            self.document.revision()
        }

        fn changes(&self) -> &settings_store::ChangeSignal {
            self.document.changes()
        }
    }

    fn accessibility(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        let serde_json::Value::Object(section) = value else {
            panic!("a settings section is an object");
        };
        section
    }

    /// A runtime holding `config` regardless of what its settings say.
    fn fixed(config: Config, next_scan: Option<Instant>) -> SwitchControlRuntime {
        let settings = LiveSettings::start_from(
            Arc::new(settings_store::HostDocument::new(
                serde_json::Map::new(),
                |_| Ok(()),
            )),
            || {},
        );
        SwitchControlRuntime {
            seen: settings.generation(),
            settings,
            config,
            next_scan,
        }
    }

    #[test]
    fn translate_does_no_io_between_changes() {
        let source = Arc::new(CountingSource {
            document: settings_store::HostDocument::new(serde_json::Map::new(), |_| Ok(())),
            loads: AtomicUsize::new(0),
        });
        source
            .update_section(
                "accessibility",
                accessibility(
                    serde_json::json!({"switch_control": true, "switch_key_pair": "f9_f10"}),
                ),
            )
            .unwrap();
        let settings = LiveSettings::start_from(source.clone(), || {});
        let now = Instant::now();
        let mut runtime = SwitchControlRuntime::new(now, &settings);
        let loads = source.loads.load(Ordering::SeqCst);
        for step in 0..1_000_u64 {
            let at = now + Duration::from_millis(step);
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F9), true), at),
                Some((SwitchAction::Next, true))
            );
            assert_eq!(runtime.tick(at), None);
        }
        assert_eq!(
            source.loads.load(Ordering::SeqCst),
            loads,
            "keys between settings changes read no settings"
        );

        source
            .update_section(
                "accessibility",
                accessibility(serde_json::json!({"switch_control": false})),
            )
            .unwrap();
        let _ = settings.wait_for_change(runtime.seen, tilo_test_env::hang_bound());
        assert_eq!(
            runtime.translate(&key(Key::Named(NamedKey::F9), true), now),
            None,
            "the next key after a change follows it"
        );
    }

    #[test]
    fn switch_control_scans_and_maps_only_the_configured_pair() {
        // Phase `disabled_configuration_has_no_scan_deadline`.
        {
            let now = Instant::now();
            let mut runtime = fixed(
                Config {
                    enabled: false,
                    pair: SwitchKeyPair::F6F7,
                    scan: Some(Duration::from_secs(1)),
                },
                None,
            );
            assert_eq!(runtime.tick(now + Duration::from_secs(2)), None);
        }
        // Phase `automatic_scan_advances_and_reschedules`.
        {
            let now = Instant::now();
            let mut runtime = fixed(
                Config {
                    enabled: true,
                    pair: SwitchKeyPair::F6F7,
                    scan: Some(Duration::from_secs(2)),
                },
                Some(now + Duration::from_secs(2)),
            );
            assert_eq!(runtime.tick(now + Duration::from_secs(1)), None);
            assert_eq!(
                runtime.tick(now + Duration::from_secs(2)),
                Some(SwitchAction::Next)
            );
            assert_eq!(runtime.tick(now + Duration::from_secs(3)), None);
            assert_eq!(
                runtime.tick(now + Duration::from_secs(4)),
                Some(SwitchAction::Next)
            );
        }
        // Phase `configured_pair_maps_next_and_activate_but_nothing_else`.
        {
            let now = Instant::now();
            let mut runtime = fixed(
                Config {
                    enabled: true,
                    pair: SwitchKeyPair::F9F10,
                    scan: None,
                },
                None,
            );
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F9), true), now),
                Some((SwitchAction::Next, true))
            );
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F10), true), now),
                Some((SwitchAction::Activate, true))
            );
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F6), true), now),
                None
            );
        }
        // Phase `disabled_mode_preserves_even_the_configured_keys`.
        {
            let now = Instant::now();
            let mut runtime = fixed(
                Config {
                    enabled: false,
                    pair: SwitchKeyPair::F6F7,
                    scan: None,
                },
                None,
            );
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F6), true), now),
                None
            );
        }
    }
}
