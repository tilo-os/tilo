//! Opt-in native input timelines. These schedule delivery, not display completion.
//!
//! Call `spawn` only after the native window has first presented. Deadlines use
//! one origin so slow handling cannot silently reduce the requested input rate.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[path = "native_host_clock.rs"]
pub(crate) mod host_clock;

#[path = "native_log.rs"]
pub(crate) mod log;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub interval_us: u64,
    /// Number of initial entries in `steps` excluded from measured samples.
    #[serde(default)]
    pub warmup_steps: u64,
    pub steps: Vec<Input>,
}

/// Positions are physical window coordinates, matching the native input bridge.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Hover {
        x: f64,
        y: f64,
    },
    Click {
        x: f64,
        y: f64,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    /// One Unicode scalar per press/release pair.
    Character {
        value: char,
    },
    Key {
        key: crate::synthetic_input::NativeKey,
    },
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub id: usize,
    pub scheduled_at: Instant,
    /// Captured immediately before invoking the delivery callback.
    pub sent_at: Instant,
    /// Direct host seconds around `sent_at`; absent on unsupported platforms.
    pub(crate) sent_host: Option<host_clock::HostClockBracket>,
    pub origin: Instant,
    pub input: Input,
    pub warmup: bool,
}

impl Scenario {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("native scenario name must not be empty".into());
        }
        if self.interval_us == 0 {
            return Err("native scenario interval_us must be positive".into());
        }
        if self.steps.is_empty() || self.steps.len() > 100_000 {
            return Err("native scenario must contain 1..=100000 steps".into());
        }
        if self.warmup_steps >= self.steps.len() as u64 {
            return Err("native scenario must contain a measured step after warmup".into());
        }
        for (id, input) in self.steps.iter().enumerate() {
            let finite = match input {
                Input::Hover { x, y } | Input::Click { x, y } => x.is_finite() && y.is_finite(),
                Input::Wheel { dx, dy } => dx.is_finite() && dy.is_finite(),
                Input::Character { .. } | Input::Key { .. } => true,
            };
            if !finite {
                return Err(format!(
                    "native scenario step {id} contains a non-finite input"
                ));
            }
        }
        deadline(Instant::now(), self.interval_us, self.steps.len() - 1)?;
        Ok(())
    }
}

fn parse(contents: &str) -> Result<Scenario, String> {
    let scenario: Scenario = serde_json::from_str(contents)
        .map_err(|error| format!("invalid native scenario JSON: {error}"))?;
    scenario.validate()?;
    Ok(scenario)
}

pub fn load_from_env() -> Result<Option<Scenario>, String> {
    let Some(path) = std::env::var_os("QOS_NATIVE_SCENARIO") else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(path);
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read native scenario {}: {error}", path.display()))?;
    parse(&contents).map(Some)
}

/// Compute an absolute deadline without wrapping either duration or clock range.
pub fn deadline(origin: Instant, interval_us: u64, index: usize) -> Result<Instant, String> {
    if interval_us == 0 {
        return Err("native scenario interval_us must be positive".into());
    }
    let index = u64::try_from(index).map_err(|_| "native scenario index overflow".to_owned())?;
    let offset_us = interval_us
        .checked_mul(index)
        .ok_or_else(|| "native scenario duration overflow".to_owned())?;
    origin
        .checked_add(Duration::from_micros(offset_us))
        .ok_or_else(|| "native scenario deadline overflow".to_owned())
}

/// Starts an absolute timeline; `false` from the callback stops delivery.
///
/// Joining this thread proves only delivery has ended, not that inputs were
/// handled, changed application state, rendered, or appeared on a display.
pub fn spawn(
    scenario: Scenario,
    mut deliver: impl FnMut(Packet) -> bool + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    scenario.validate()?;
    let origin = Instant::now();
    // Resolve every checked deadline before starting: no partial timeline on error.
    let packets = scenario
        .steps
        .into_iter()
        .enumerate()
        .map(|(id, input)| {
            Ok(Packet {
                id,
                scheduled_at: deadline(origin, scenario.interval_us, id)?,
                origin,
                sent_at: origin,
                sent_host: None,
                input,
                warmup: (id as u64) < scenario.warmup_steps,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    std::thread::Builder::new()
        .name("qos-native-scenario".into())
        .spawn(move || {
            for mut packet in packets {
                // A late callback never shifts the remaining schedule. Checking
                // again after sleep also guards against an early clock wakeup.
                while let Some(remaining) =
                    packet.scheduled_at.checked_duration_since(Instant::now())
                {
                    if remaining.is_zero() {
                        break;
                    }
                    std::thread::sleep(remaining);
                }
                (packet.sent_at, packet.sent_host) = host_clock::sample();
                if !deliver(packet) {
                    break;
                }
            }
        })
        .map_err(|error| format!("cannot start native scenario thread: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn effect_probe_reads_current_fixture_state_without_changing_it() {
        let offset = std::rc::Rc::new(std::cell::Cell::new(12_u32));
        let observed = offset.clone();
        let mut app = crate::App::new(
            crate::State::new(),
            Box::new(crate::widget::Text::new("probe")),
        );
        assert!(app.native_input_snapshot().is_none());
        app.set_native_input_probe(move || serde_json::json!({"offset": observed.get()}));
        assert_eq!(app.native_input_snapshot().unwrap()["offset"], 12);
        offset.set(24);
        assert_eq!(app.native_input_snapshot().unwrap()["offset"], 24);
        assert_eq!(offset.get(), 24);
    }

    #[test]
    fn keyboard_schema_preserves_unicode_and_rejects_ambiguous_input() {
        let scenario = parse(r#"{"name":"typing","interval_us":16667,"steps":[{"kind":"character","value":"é"},{"kind":"key","key":"backspace"}]}"#).unwrap();
        assert_eq!(
            serde_json::to_value(scenario.steps[0]).unwrap()["value"],
            "é"
        );
        for input in [
            r#"{"kind":"character","value":""}"#,
            r#"{"kind":"character","value":"ab"}"#,
            r#"{"kind":"key","key":"unknown"}"#,
            r#"{"kind":"character","value":"x","extra":true}"#,
        ] {
            assert!(serde_json::from_str::<Input>(input).is_err(), "{input}");
        }
    }

    fn scenario() -> Scenario {
        Scenario {
            name: "native wheel".into(),
            interval_us: 1_000,
            warmup_steps: 1,
            steps: vec![Input::Wheel { dx: 0.0, dy: 12.0 }; 3],
        }
    }

    #[test]
    fn delayed_delivery_does_not_shift_future_deadlines() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        spawn(scenario(), move |packet| {
            if packet.id == 0 {
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            output.lock().unwrap().push(packet);
            true
        })
        .unwrap()
        .join()
        .unwrap();
        let packets = observed.lock().unwrap();
        assert_eq!(packets.len(), 3);
        assert!(
            packets
                .iter()
                .all(|packet| packet.sent_at >= packet.scheduled_at)
        );
        assert!(packets[1].sent_at.duration_since(packets[0].origin) >= Duration::from_millis(8));

        assert_eq!(
            packets[1]
                .scheduled_at
                .duration_since(packets[0].origin)
                .as_micros(),
            1_000
        );
        assert_eq!(
            packets[2]
                .scheduled_at
                .duration_since(packets[0].origin)
                .as_micros(),
            2_000
        );
        assert!(packets[0].warmup);
        assert!(!packets[1].warmup);
        // Every packet, including warmup and the final delivery, carries its
        // own bounds. Delayed delivery must not reuse an origin calibration.
        for packet in packets.iter() {
            #[cfg(target_os = "macos")]
            {
                let bounds = packet.sent_host.expect("per-packet host bounds");
                assert!(bounds.lo_s.is_finite() && bounds.lo_s > 0.0);
                assert!(bounds.hi_s.is_finite() && bounds.hi_s >= bounds.lo_s);
            }
            #[cfg(not(target_os = "macos"))]
            assert!(packet.sent_host.is_none());
        }
        #[cfg(target_os = "macos")]
        for pair in packets.windows(2) {
            assert!(pair[0].sent_host.unwrap().hi_s <= pair[1].sent_host.unwrap().lo_s);
        }
    }

    #[test]
    fn closed_receiver_stops_scheduling() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        spawn(scenario(), move |packet| {
            output.lock().unwrap().push(packet.id);
            false
        })
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(*observed.lock().unwrap(), vec![0]);
    }

    #[test]
    fn rejects_invalid_scenario_before_scheduling() {
        let mut invalid = scenario();
        invalid.name = "  ".into();
        assert!(invalid.validate().is_err());
        invalid = scenario();
        invalid.interval_us = 0;
        assert!(invalid.validate().is_err());
        invalid = scenario();
        invalid.steps.clear();
        assert!(invalid.validate().is_err());
        invalid = scenario();
        invalid.warmup_steps = 3;
        assert!(invalid.validate().is_err());
        invalid = scenario();
        invalid.steps = vec![Input::Hover { x: 0.0, y: 0.0 }; 100_001];
        assert!(invalid.validate().is_err());
        for input in [
            Input::Hover {
                x: f64::NAN,
                y: 0.0,
            },
            Input::Click {
                x: 0.0,
                y: f64::INFINITY,
            },
            Input::Wheel {
                dx: f32::NEG_INFINITY,
                dy: 1.0,
            },
        ] {
            invalid = scenario();
            invalid.steps[0] = input;
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn checked_timeline_rejects_overflow() {
        let origin = std::time::Instant::now();
        assert!(deadline(origin, u64::MAX, 2).is_err());
        let mut invalid = scenario();
        invalid.interval_us = u64::MAX;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn loads_validated_json_with_optional_warmup() {
        let loaded = parse(
            r#"{"name":"hover","interval_us":16667,"steps":[{"kind":"hover","x":10,"y":20}]}"#,
        )
        .unwrap();
        assert_eq!(loaded.warmup_steps, 0);
        assert!(matches!(loaded.steps[0], Input::Hover { x: 10.0, y: 20.0 }));
        assert!(
            parse(r#"{"name":"hover","interval_us":0,"steps":[{"kind":"hover","x":10,"y":20}]}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"name":"typo","interval_us":1,"steps":[{"kind":"whell","dx":0,"dy":1}]}"#)
                .is_err()
        );
    }
}
