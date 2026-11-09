//! The panel's brightness follows the light in the room.
//! `display.auto_brightness` records the policy; the session owns the displays, so the session
//! applies it, whether Settings is open or not. While it is on, the ambient light sensor is read
//! every couple of seconds and the panel's own backlight follows, in the steps
//! [`AmbientBrightnessController`] smooths them into. A machine without a sensor or a backlight
//! writes nothing, and while the idle dim holds the backlight the policy waits: the dim's restore
//! is not overruled behind the person's back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use os_actions::brightness::{BrightnessKind, BrightnessService};

use crate::Session;

/// How often the sensor is read while automatic brightness is on.
const SAMPLE: Duration = Duration::from_secs(2);

/// The session's automatic brightness.
pub(crate) struct AutoBrightness {
    controller: os_actions::AmbientBrightnessController,
    /// When the sensor was last read.
    last: Option<Instant>,
    /// Whether the person asked for it, as the settings watch last said.
    enabled: Arc<AtomicBool>,
}

impl AutoBrightness {
    /// Automatic brightness following `enabled` , which the settings watch keeps current.
    fn new(enabled: Arc<AtomicBool>) -> Self {
        Self {
            controller: os_actions::AmbientBrightnessController::default(),
            last: None,
            enabled,
        }
    }

    /// Automatic brightness following the settings, and the flag the watch keeps current.
    pub(crate) fn watched() -> (Self, Arc<AtomicBool>) {
        let enabled = Arc::new(AtomicBool::new(
            settings_store::hardware::AUTO_BRIGHTNESS.get(),
        ));
        (Self::new(Arc::clone(&enabled)), enabled)
    }

    /// Whether the person asked for it.
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Read the light in the room at `lux` , if it is time to, and say what the panel's backlight
    /// becomes. `None` when the policy is off, the sample is too early, the dim holds the
    /// backlight, or the change is inside the controller's deadband.
    pub(crate) fn follow(&mut self, now: Instant, dimmed: bool, lux: Option<f32>) -> Option<f32> {
        if !self.enabled() {
            self.controller.reset();
            self.last = None;
            return None;
        }
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < SAMPLE)
        {
            return None;
        }
        self.last = Some(now);
        if dimmed {
            return None;
        }
        self.controller.update(lux?)
    }

    /// How long until the light in the room is read again.
    pub(crate) fn wait(&self) -> Option<Duration> {
        self.enabled().then_some(SAMPLE)
    }
}

/// Take `document` , the settings as they now are, into `enabled` .
pub(crate) fn read(enabled: &AtomicBool, document: &serde_json::Map<String, serde_json::Value>) {
    enabled.store(
        settings_store::hardware::AUTO_BRIGHTNESS.from_document(document),
        Ordering::Relaxed,
    );
}

impl Session {
    /// The panel follows the light in the room while the person asked for
    /// it, and the night light takes what shows now. A dim that holds the
    /// backlight keeps the panel where the person's dimming left it.
    ///
    /// # Errors
    /// The compositor failed.
    pub(crate) fn follow_light(&mut self) -> Result<Option<u32>, tilo_compositor::Error> {
        if let Some(percent) = self.auto_brightness.follow(
            Instant::now(),
            self.compositor.dim_holds_backlight(),
            os_actions::ambient_lux(),
        ) {
            apply(percent);
        }
        self.night_light.follow(&mut self.compositor)?;
        Ok(self.night_light.kelvin())
    }
}

/// Move the built-in panel's own backlight to `percent` . A monitor across the desk is not moved:
/// the light in the room says nothing about it, and its DDC/CI level is the person's to set.
pub(crate) fn apply(percent: f32) {
    let service = BrightnessService::global();
    let panel = service.levels().into_iter().find(|level| {
        level.kind == BrightnessKind::Backlight
            && ["eDP", "LVDS", "DSI"]
                .iter()
                .any(|prefix| level.connector.starts_with(prefix))
    });
    let Some(panel) = panel else {
        return;
    };
    if !service.set(&panel.connector, percent) {
        tracing::warn!(
            connector = %panel.connector,
            "the automatic brightness change was not applied"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_follows_the_light_at_its_own_pace_and_never_through_the_dim() {
        let enabled = Arc::new(AtomicBool::new(false));
        let mut auto = AutoBrightness::new(Arc::clone(&enabled));
        let start = Instant::now();
        assert_eq!(auto.wait(), None, "nothing is read while the policy is off");
        assert_eq!(auto.follow(start, false, Some(10.0)), None);

        enabled.store(true, Ordering::Relaxed);
        assert_eq!(auto.wait(), Some(SAMPLE));
        let indoors = auto
            .follow(start, false, Some(40.0))
            .expect("the first reading moves the panel");
        assert!((40.0..=50.0).contains(&indoors), "{indoors}");
        assert_eq!(
            auto.follow(start + Duration::from_secs(1), false, Some(30_000.0)),
            None,
            "a sample inside the pace is not read"
        );
        assert_eq!(
            auto.follow(start + SAMPLE, true, Some(30_000.0)),
            None,
            "the idle dim owns the backlight until it lets go"
        );
        let daylight = auto
            .follow(start + 2 * SAMPLE, false, Some(30_000.0))
            .expect("a sustained change reaches the panel");
        assert!(daylight > indoors, "{daylight} brighter than {indoors}");

        enabled.store(false, Ordering::Relaxed);
        assert_eq!(
            auto.follow(start + 3 * SAMPLE, false, Some(1.0)),
            None,
            "nothing follows once the person turns it off"
        );
    }
}
