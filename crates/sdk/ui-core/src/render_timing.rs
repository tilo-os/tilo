//! Opt-in boundaries around native benchmark render handoffs.
use crate::native_scenario::host_clock::{self, HostClockBracket};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CallBounds {
    pub before: Option<HostClockBracket>,
    pub after: Option<HostClockBracket>,
}

#[derive(Clone, Copy)]
#[cfg(feature = "native")]
pub(crate) enum MainStage {
    BeforeProfilerCommit,
    AfterProfilerCommit,
    AfterNativeFrameLog,
    AfterDebugOutlines,
    AfterDamageMarkDrawn,
    AfterAccessibility,
    AfterHudRequest,
    AfterNativeRedrawLog,
}

#[cfg(feature = "native")]
const MAIN_STAGE_NAMES: [&str; 8] = [
    "before_profiler_commit",
    "after_profiler_commit",
    "after_native_frame_log",
    "after_debug_outlines",
    "after_damage_mark_drawn",
    "after_accessibility",
    "after_hud_request",
    "after_native_redraw_log",
];

#[derive(Default)]
#[cfg(feature = "native")]
pub(crate) struct MainStages {
    points: [Option<HostClockBracket>; 8],
}

#[cfg(feature = "native")]
impl MainStages {
    pub(crate) fn row(&self, cpu_frame_id: Option<u64>) -> serde_json::Value {
        let mut row = serde_json::Map::new();
        row.insert("cpu_frame_id".into(), serde_json::json!(cpu_frame_id));
        row.insert(
            "host_clock".into(),
            serde_json::json!(host_clock::CLOCK_NAME),
        );
        for (name, bounds) in MAIN_STAGE_NAMES.into_iter().zip(self.points) {
            row.insert(
                format!("{name}_host_lo_s"),
                serde_json::json!(bounds.map(|b| b.lo_s)),
            );
            row.insert(
                format!("{name}_host_hi_s"),
                serde_json::json!(bounds.map(|b| b.hi_s)),
            );
        }
        serde_json::Value::Object(row)
    }
}

#[cfg(feature = "native")]
pub(crate) fn record_main_stage(stages: &mut Option<MainStages>, stage: MainStage) {
    record_main_stage_with(stages, stage, capture);
}

#[cfg(feature = "native")]
fn record_main_stage_with(
    stages: &mut Option<MainStages>,
    stage: MainStage,
    clock: impl FnOnce() -> Option<HostClockBracket>,
) {
    if let Some(point) = stages
        .as_mut()
        .and_then(|stages| stages.points.get_mut(stage as usize))
    {
        *point = clock();
    }
}

pub(crate) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        cfg!(all(target_os = "macos", feature = "presentation-feedback"))
            && crate::host::environment().diagnostics.presentation_profile
    })
}

pub(crate) fn capture() -> Option<HostClockBracket> {
    host_clock::sample().1
}

pub(crate) fn around<T>(enabled: bool, operation: impl FnOnce() -> T) -> (T, Option<CallBounds>) {
    around_with(enabled, capture, operation)
}

fn around_with<T>(
    enabled: bool,
    mut clock: impl FnMut() -> Option<HostClockBracket>,
    operation: impl FnOnce() -> T,
) -> (T, Option<CallBounds>) {
    if !enabled {
        return (operation(), None);
    }
    let before = clock();
    let result = operation();
    let after = clock();
    (result, Some(CallBounds { before, after }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn main_stages_record_named_bounds_or_stay_explicitly_missing() {
        // Case `main_stages_keep_chronological_named_bounds_and_frame_identity`.
        {
            let mut stages = Some(MainStages::default());
            for (index, stage) in [
                MainStage::BeforeProfilerCommit,
                MainStage::AfterProfilerCommit,
                MainStage::AfterNativeFrameLog,
                MainStage::AfterDebugOutlines,
                MainStage::AfterDamageMarkDrawn,
                MainStage::AfterAccessibility,
                MainStage::AfterHudRequest,
                MainStage::AfterNativeRedrawLog,
            ]
            .into_iter()
            .enumerate()
            {
                record_main_stage_with(&mut stages, stage, || {
                    Some(HostClockBracket {
                        lo_s: 100.0 + index as f64,
                        hi_s: 100.25 + index as f64,
                    })
                });
            }
            let row = stages.unwrap().row(Some(1293));
            let decoded: serde_json::Value = serde_json::from_str(&row.to_string()).unwrap();
            assert_eq!(decoded["cpu_frame_id"], 1293);
            assert_eq!(decoded["host_clock"], host_clock::CLOCK_NAME);
            for (index, name) in MAIN_STAGE_NAMES.into_iter().enumerate() {
                assert_eq!(decoded[format!("{name}_host_lo_s")], 100.0 + index as f64);
                assert_eq!(decoded[format!("{name}_host_hi_s")], 100.25 + index as f64);
            }
            assert_eq!(decoded.as_object().unwrap().len(), 18);
        }

        // Case `disabled_main_stages_read_no_clock`.
        {
            record_main_stage_with(&mut None, MainStage::BeforeProfilerCommit, || {
                panic!("disabled stage read a clock")
            });
        }

        // Case `incomplete_main_stages_remain_explicitly_missing`.
        {
            let mut stages = Some(MainStages::default());
            record_main_stage_with(&mut stages, MainStage::BeforeProfilerCommit, || None);
            let row = stages.unwrap().row(None);
            for name in MAIN_STAGE_NAMES {
                assert_eq!(
                    row.get(format!("{name}_host_lo_s")),
                    Some(&serde_json::Value::Null)
                );
                assert_eq!(
                    row.get(format!("{name}_host_hi_s")),
                    Some(&serde_json::Value::Null)
                );
            }
            assert!(row["cpu_frame_id"].is_null());
        }

        // Case `unavailable_clock_bounds_are_not_fabricated`.
        {
            let (result, bounds) = around_with(true, || None, || 23);
            assert_eq!(result, 23);
            assert_eq!(
                bounds,
                Some(CallBounds {
                    before: None,
                    after: None
                })
            );
        }
    }

    #[test]
    fn timed_operations_are_bracketed_once_and_keep_their_result() {
        // Case `operation_is_bracketed_once_and_error_result_is_preserved`.
        {
            let calls = RefCell::new(Vec::new());
            let mut times = [
                HostClockBracket {
                    lo_s: 1.0,
                    hi_s: 2.0,
                },
                HostClockBracket {
                    lo_s: 4.0,
                    hi_s: 5.0,
                },
            ]
            .into_iter();
            let (result, bounds) = around_with(
                true,
                || {
                    calls.borrow_mut().push("clock");
                    times.next()
                },
                || {
                    calls.borrow_mut().push("operation");
                    Err::<(), _>("closed")
                },
            );
            assert_eq!(*calls.borrow(), ["clock", "operation", "clock"]);
            assert_eq!(result, Err("closed"));
            assert_eq!(
                bounds,
                Some(CallBounds {
                    before: Some(HostClockBracket {
                        lo_s: 1.0,
                        hi_s: 2.0
                    }),
                    after: Some(HostClockBracket {
                        lo_s: 4.0,
                        hi_s: 5.0
                    }),
                })
            );
            assert!(times.next().is_none());
        }

        // Case `disabled_path_executes_operation_without_reading_clock`.
        {
            let (result, bounds) = around_with(false, || panic!("disabled clock read"), || 17);
            assert_eq!(result, 17);
            assert!(bounds.is_none());
        }
    }
}

#[path = "render_handoff.rs"]
pub(crate) mod handoff;
