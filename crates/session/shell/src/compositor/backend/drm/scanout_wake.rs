//! An output's wakes ([`super::super::wake_check`]): when one starts, what
//! the kernel holds once it is lit, its judgement and its first frame.

use std::time::Instant;

use smithay::reexports::drm::control::{Device as _, connector};

use super::super::wake_check::{LitState, WakeCheck, WakeStage, WakeVerdict};
use super::{DrmScanout, find_property};

/// An output's wakes: the one being judged and what the next one needs.
#[derive(Debug)]
pub(super) struct OutputWake {
    /// A panel built into the machine (eDP, LVDS, DSI): its wake is lit only
    /// once its backlight is unblanked too.
    pub(super) builtin: bool,
    /// The wake this output is in, from the moment it was asked to light
    /// until it was judged and its first frame shown.
    pub(super) check: Option<WakeCheck>,
    /// Why the next wake happens, when it is not an idle wake: a resume.
    pub(super) reason: Option<&'static str>,
    /// The stage the next wake takes as failed: the test-control fault that
    /// proves the recovery.
    pub(super) fault: Option<WakeStage>,
}

impl OutputWake {
    pub(super) fn new(builtin: bool) -> Self {
        Self {
            builtin,
            check: None,
            reason: None,
            fault: None,
        }
    }
}

impl DrmScanout {
    /// The output is asked to light after being dark: its wake starts here
    /// and is judged once the light-up is known ([`super::wake_check`]).
    pub(super) fn begin_wake(&mut self) {
        if self.wake.check.is_some() {
            return;
        }
        let reason = self.wake.reason.take().unwrap_or("dpms");
        self.wake.check = Some(WakeCheck::new(
            Instant::now(),
            reason,
            self.wake.fault.take(),
        ));
        if let Some((link, _)) = self.link.as_mut() {
            link.arm_wake_redo();
        }
    }

    /// Take `stage` as failed in this output's next wake, whatever it
    /// observes: proves the recovery on hardware that never fails.
    pub(in super::super) fn inject_wake_fault(&mut self, stage: WakeStage) {
        self.wake.fault = Some(stage);
    }

    /// What the kernel holds for this output now: the CRTC active, the
    /// connector's status and, on a built-in panel, its backlight.
    fn lit_state(&self) -> LitState {
        let crtc_active = if self.link.is_some() {
            find_property(&self.card, self.crtc, "ACTIVE").map(|(_, value, _)| value != 0)
        } else {
            self.card
                .get_crtc(self.crtc)
                .ok()
                .map(|info| info.mode().is_some())
        };
        let connector = match self
            .card
            .get_connector(self.connector, false)
            .map(|info| info.state())
        {
            Ok(connector::State::Connected) => "connected",
            Ok(connector::State::Disconnected) => "disconnected",
            _ => "unknown",
        };
        let (backlight_powered, backlight) = if self.wake.builtin {
            (
                os_actions::display::backlight_powered(),
                os_actions::display::backlight_state(),
            )
        } else {
            (None, None)
        };
        LitState {
            crtc_active,
            connector,
            backlight_powered,
            backlight,
        }
    }

    /// Judge this output's wake, `observed` the stage seen not to happen
    /// (`None`: the rest is read back from the kernel), and write its
    /// flight-record line. `None` outside a wake. The caller redoes the
    /// modeset on [`WakeVerdict::Redo`], unless the link already does.
    pub(super) fn judge_wake(&mut self, observed: Option<WakeStage>) -> Option<WakeVerdict> {
        self.wake.check.as_ref()?;
        let state = self.lit_state();
        let failed = observed.or_else(|| state.failed_stage());
        let connector = u32::from(self.connector);
        let wake = self.wake.check.as_mut()?;
        let verdict = wake.judge(failed);
        eprintln!(
            "{}",
            wake.record(connector, verdict, &state, Instant::now())
        );
        let flips = self.use_pageflip || self.link.is_some();
        match verdict {
            WakeVerdict::Lit => {
                if let Some((link, _)) = self.link.as_mut() {
                    link.disarm_wake_redo();
                }
                if !flips {
                    // No page flip will report the first frame: the record
                    // ends with the judgement.
                    self.wake.check = None;
                }
            }
            WakeVerdict::Redo(_) => {}
            WakeVerdict::Failed(_) => {
                if let Some((link, _)) = self.link.as_mut() {
                    link.disarm_wake_redo();
                }
                self.wake.check = None;
            }
        }
        Some(verdict)
    }

    /// A frame reached the screen: the first one after a wake closes its
    /// record.
    pub(super) fn note_wake_flip(&mut self) {
        let connector = u32::from(self.connector);
        let Some(wake) = self.wake.check.as_mut() else {
            return;
        };
        if let Some(line) = wake.first_flip(connector, Instant::now()) {
            eprintln!("{line}");
        }
        if wake.settled() {
            self.wake.check = None;
        }
    }
}
