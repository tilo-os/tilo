//! The check every output gets after it is woken: from an idle blank, after
//! a resume from sleep, or when the session gets the hardware back.
//!
//! A wake is a chain of stages, and a screen that stays dark only says that
//! one of them did not happen: the commit that lights the CRTC, the link to
//! the display coming up (a vblank after the modeset), the CRTC reading back
//! active, and, on a built-in panel, its backlight unblanked. Each output
//! judges its own wake once those are known and writes one flight-record
//! line with what the kernel holds afterwards, then a second line when the
//! first frame after the wake reaches the screen.
//!
//! A stage that failed is recovered once per wake by redoing the modeset
//! from the CRTC switched off: a full modeset is what retrains a link and
//! re-enables the panel and its backlight, where re-lighting an
//! already-active CRTC with the same mode changes nothing in the kernel. A
//! second failure in the same wake is final for that wake; the next wake,
//! the power key or a display change tries again. Nothing retries on a
//! timer.

use std::time::Instant;

pub use crate::compositor::backend::WakeStage;

/// What the kernel holds for an output once it was lit, read back after the
/// wake rather than assumed from the commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LitState {
    /// The CRTC is active (atomic `ACTIVE`, or a mode bound for a legacy
    /// CRTC); `None` when it could not be read.
    pub crtc_active: Option<bool>,
    /// The connector's status: `connected`, `disconnected` or `unknown`.
    pub connector: &'static str,
    /// The built-in panel's backlight powered (`bl_power` 0); `None` for an
    /// output without one, or one that says nothing.
    pub backlight_powered: Option<bool>,
    /// The backlight as the kernel holds it, for the record.
    pub backlight: Option<String>,
}

impl LitState {
    /// The first stage this read-back shows did not happen.
    pub fn failed_stage(&self) -> Option<WakeStage> {
        if self.crtc_active == Some(false) {
            Some(WakeStage::Crtc)
        } else if self.backlight_powered == Some(false) {
            Some(WakeStage::Backlight)
        } else {
            None
        }
    }
}

/// What one judgement of a wake decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeVerdict {
    /// Every stage happened: the output is lit.
    Lit,
    /// `stage` did not happen; the modeset is redone, once.
    Redo(WakeStage),
    /// `stage` did not happen again after the modeset was redone.
    Failed(WakeStage),
}

impl WakeVerdict {
    fn outcome(self) -> &'static str {
        match self {
            Self::Lit => "lit",
            Self::Redo(_) => "redoing-modeset",
            Self::Failed(_) => "failed",
        }
    }

    fn stage(self) -> Option<WakeStage> {
        match self {
            Self::Lit => None,
            Self::Redo(stage) | Self::Failed(stage) => Some(stage),
        }
    }
}

/// One output's wake, from the moment it was asked to light until its first
/// frame after it reached the screen.
#[derive(Debug)]
pub struct WakeCheck {
    started: Instant,
    /// Why it woke, for the record: `resume`, `dpms`.
    reason: &'static str,
    redone: bool,
    /// A stage the next judgement takes as failed whatever it observed: the
    /// test-control fault, so a test can prove the recovery on hardware that
    /// never fails.
    fault: Option<WakeStage>,
    /// Judged lit; the first frame after the wake is still to reach the
    /// screen.
    awaiting_first_flip: bool,
}

impl WakeCheck {
    /// A wake asked for at `started`.
    pub fn new(started: Instant, reason: &'static str, fault: Option<WakeStage>) -> Self {
        Self {
            started,
            reason,
            redone: false,
            fault,
            awaiting_first_flip: false,
        }
    }

    /// Whether the modeset was already redone in this wake.
    pub fn redone(&self) -> bool {
        self.redone
    }

    /// Judge the wake with `failed` the first stage observed not to happen.
    /// The first failure in a wake asks for the modeset to be redone; a
    /// second one is final.
    pub fn judge(&mut self, failed: Option<WakeStage>) -> WakeVerdict {
        let verdict = match self.fault.take().or(failed) {
            None => WakeVerdict::Lit,
            Some(stage) if !self.redone => {
                self.redone = true;
                WakeVerdict::Redo(stage)
            }
            Some(stage) => WakeVerdict::Failed(stage),
        };
        self.awaiting_first_flip = verdict == WakeVerdict::Lit;
        verdict
    }

    /// The flight-record line of a judgement, with what the kernel holds.
    pub fn record(
        &self,
        connector: u32,
        verdict: WakeVerdict,
        state: &LitState,
        now: Instant,
    ) -> String {
        let crtc = match state.crtc_active {
            Some(true) => "active",
            Some(false) => "inactive",
            None => "unknown",
        };
        format!(
            "tilo compositor: DRM wake check: connector={connector} reason={} outcome={} stage={} after_ms={} redone={} crtc={crtc} connector_status={} backlight={}",
            self.reason,
            verdict.outcome(),
            verdict.stage().map_or("none", WakeStage::name),
            now.saturating_duration_since(self.started).as_millis(),
            self.redone,
            state.connector,
            state.backlight.as_deref().unwrap_or("none"),
        )
    }

    /// A frame reached the screen at `now`: the record's closing line when it
    /// is the first one after a wake judged lit, else `None`.
    pub fn first_flip(&mut self, connector: u32, now: Instant) -> Option<String> {
        if !std::mem::take(&mut self.awaiting_first_flip) {
            return None;
        }
        Some(format!(
            "tilo compositor: DRM wake check: connector={connector} reason={} first_flip_after_ms={}",
            self.reason,
            now.saturating_duration_since(self.started).as_millis(),
        ))
    }

    /// Whether the wake is over: judged, and its first frame shown (or no
    /// frame is awaited).
    pub fn settled(&self) -> bool {
        !self.awaiting_first_flip
    }
}

/// The session has the CRTCs back: re-modeset every output, lit as `dpms_on` says.
pub(super) fn resume_scanouts(scanouts: &mut [super::ScanoutEntry], dpms_on: bool) {
    // A flip queued before the suspend still completes (DRM master is kept):
    // read its event now, or the re-modeset's vblank proof takes it for its own.
    super::drain_page_flip_completions(scanouts);
    for e in scanouts.iter_mut() {
        e.scanout.restore();
        e.set_power(dpms_on && !e.powered_off);
        e.direct_targets = None;
    }
    tracing::info!(
        outputs = scanouts.len(),
        dpms_on,
        "DRM present: resumed after re-modesetting every output"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn lit() -> LitState {
        LitState {
            crtc_active: Some(true),
            connector: "connected",
            backlight_powered: Some(true),
            backlight: Some("device=panel bl_power=0".into()),
        }
    }

    #[test]
    fn a_wake_whose_stages_all_happened_is_lit_and_records_its_first_frame() {
        let t0 = Instant::now();
        let mut wake = WakeCheck::new(t0, "resume", None);
        assert_eq!(wake.judge(lit().failed_stage()), WakeVerdict::Lit);
        let line = wake.record(
            43,
            WakeVerdict::Lit,
            &lit(),
            t0 + Duration::from_millis(120),
        );
        assert_eq!(
            line,
            "tilo compositor: DRM wake check: connector=43 reason=resume outcome=lit stage=none \
             after_ms=120 redone=false crtc=active connector_status=connected \
             backlight=device=panel bl_power=0"
        );
        assert!(!wake.settled(), "the first frame is still to come");
        assert_eq!(
            wake.first_flip(43, t0 + Duration::from_millis(150))
                .as_deref(),
            Some(
                "tilo compositor: DRM wake check: connector=43 reason=resume first_flip_after_ms=150"
            )
        );
        assert!(wake.settled());
        assert_eq!(
            wake.first_flip(43, t0 + Duration::from_millis(170)),
            None,
            "only the first"
        );
    }

    #[test]
    fn a_failed_stage_redoes_the_modeset_once_and_a_second_failure_is_final() {
        let mut wake = WakeCheck::new(Instant::now(), "dpms", None);
        assert_eq!(
            wake.judge(Some(WakeStage::Link)),
            WakeVerdict::Redo(WakeStage::Link)
        );
        assert!(wake.redone());
        assert!(wake.settled(), "no frame is awaited from a failed wake");
        assert_eq!(
            wake.judge(Some(WakeStage::Link)),
            WakeVerdict::Failed(WakeStage::Link)
        );

        let mut recovered = WakeCheck::new(Instant::now(), "dpms", None);
        assert_eq!(
            recovered.judge(Some(WakeStage::Backlight)),
            WakeVerdict::Redo(WakeStage::Backlight)
        );
        assert_eq!(recovered.judge(None), WakeVerdict::Lit);
    }

    #[test]
    fn a_forced_fault_fails_the_first_judgement_and_the_redo_recovers() {
        let mut wake = WakeCheck::new(Instant::now(), "resume", Some(WakeStage::Crtc));
        assert_eq!(wake.judge(None), WakeVerdict::Redo(WakeStage::Crtc));
        assert_eq!(wake.judge(None), WakeVerdict::Lit, "the fault applies once");
    }

    #[test]
    fn the_read_back_names_the_first_stage_that_did_not_happen() {
        assert_eq!(lit().failed_stage(), None);
        let dark_crtc = LitState {
            crtc_active: Some(false),
            backlight_powered: Some(false),
            ..lit()
        };
        assert_eq!(dark_crtc.failed_stage(), Some(WakeStage::Crtc));
        let blanked_backlight = LitState {
            backlight_powered: Some(false),
            ..lit()
        };
        assert_eq!(blanked_backlight.failed_stage(), Some(WakeStage::Backlight));
        let unreadable = LitState {
            crtc_active: None,
            backlight_powered: None,
            ..lit()
        };
        assert_eq!(
            unreadable.failed_stage(),
            None,
            "what cannot be read is not a failure"
        );
    }

    #[test]
    fn a_fault_names_a_stage_as_the_flight_record_does() {
        for stage in [
            WakeStage::Commit,
            WakeStage::Link,
            WakeStage::Crtc,
            WakeStage::Backlight,
        ] {
            let named = serde_json::Value::from(stage.name());
            assert_eq!(serde_json::from_value::<WakeStage>(named).ok(), Some(stage));
        }
        assert!(serde_json::from_value::<WakeStage>("vblank".into()).is_err());
    }
}
