//! Session-owned two-switch accessibility policy.
//!
//! Physical switches normally identify as ordinary keyboards. We therefore
//! translate only a user-selected key pair into the universal Tab/Enter
//! navigation contract, inside the compositor's existing hardware-input
//! funnel. Disabled means byte-for-byte ordinary delivery; no virtual input
//! protocol or app-controlled injection capability is introduced.

use std::time::{Duration, Instant};

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchAction {
    Next,
    Activate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyPair {
    F6F7,
    F9F10,
    Digits12,
}

#[derive(Clone, Copy, Debug)]
struct Config {
    enabled: bool,
    pair: KeyPair,
    scan: Option<Duration>,
}

impl Config {
    fn load() -> Self {
        let section = settings_store::load_section("accessibility");
        let pair = match section
            .get("switch_key_pair")
            .and_then(serde_json::Value::as_str)
        {
            Some("f9_f10") => KeyPair::F9F10,
            Some("digits12") => KeyPair::Digits12,
            _ => KeyPair::F6F7,
        };
        let scan = section
            .get("switch_scan_ms")
            .and_then(serde_json::Value::as_u64)
            .filter(|ms| (500..=5_000).contains(ms))
            .map(Duration::from_millis);
        Self {
            enabled: section
                .get("switch_control")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            pair,
            scan,
        }
    }
}

pub struct SwitchControlRuntime {
    config: Config,
    next_reload: Instant,
    next_scan: Option<Instant>,
}

/// `now + delta`, saturating to `now` itself on the (cosmically unlikely) `Instant` overflow
/// rather than panicking — a scan or reload deadline that cannot be represented becomes "due
/// now" instead of crashing the compositor.
fn after(now: Instant, delta: Duration) -> Instant {
    now.checked_add(delta).unwrap_or(now)
}

impl SwitchControlRuntime {
    pub fn new(now: Instant) -> Self {
        let config = Config::load();
        Self {
            next_reload: after(now, Duration::from_millis(750)),
            next_scan: config
                .scan
                .filter(|_| config.enabled)
                .map(|d| after(now, d)),
            config,
        }
    }

    fn refresh(&mut self, now: Instant) {
        if now < self.next_reload {
            return;
        }
        self.next_reload = after(now, Duration::from_millis(750));
        let old = self.config;
        self.config = Config::load();
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
            (KeyPair::F6F7, Key::Named(NamedKey::F6))
            | (KeyPair::F9F10, Key::Named(NamedKey::F9)) => SwitchAction::Next,
            (KeyPair::Digits12, Key::Character(c)) if c == "1" => SwitchAction::Next,
            (KeyPair::F6F7, Key::Named(NamedKey::F7))
            | (KeyPair::F9F10, Key::Named(NamedKey::F10)) => SwitchAction::Activate,
            (KeyPair::Digits12, Key::Character(c)) if c == "2" => SwitchAction::Activate,
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
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

    fn key(logical_key: Key, pressed: bool) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = logical_key;
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed::<DeviceId>() },
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn switch_control_scans_and_maps_only_the_configured_pair() {
        // Phase `disabled_configuration_has_no_scan_deadline`.
        {
            let now = Instant::now();
            let mut runtime = SwitchControlRuntime {
                config: Config {
                    enabled: false,
                    pair: KeyPair::F6F7,
                    scan: Some(Duration::from_secs(1)),
                },
                next_reload: now + Duration::from_secs(60),
                next_scan: None,
            };
            assert_eq!(runtime.tick(now + Duration::from_secs(2)), None);
        }
        // Phase `automatic_scan_advances_and_reschedules`.
        {
            let now = Instant::now();
            let mut runtime = SwitchControlRuntime {
                config: Config {
                    enabled: true,
                    pair: KeyPair::F6F7,
                    scan: Some(Duration::from_secs(2)),
                },
                next_reload: now + Duration::from_secs(60),
                next_scan: Some(now + Duration::from_secs(2)),
            };
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
            let mut runtime = SwitchControlRuntime {
                config: Config {
                    enabled: true,
                    pair: KeyPair::F9F10,
                    scan: None,
                },
                next_reload: now + Duration::from_secs(60),
                next_scan: None,
            };
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
            let mut runtime = SwitchControlRuntime {
                config: Config {
                    enabled: false,
                    pair: KeyPair::F6F7,
                    scan: None,
                },
                next_reload: now + Duration::from_secs(60),
                next_scan: None,
            };
            assert_eq!(
                runtime.translate(&key(Key::Named(NamedKey::F6), true), now),
                None
            );
        }
    }
}
