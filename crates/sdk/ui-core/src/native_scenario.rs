//! Opt-in native input timelines. These schedule delivery, not display completion.
//!
//! Call `spawn` only after the native window has first presented. Deadlines use
//! one origin so slow handling cannot silently reduce the requested input rate.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: input timelines pace real window delivery"
)]

use crate::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

#[path = "native_host_clock.rs"]
pub(crate) mod host_clock;

#[path = "native_log.rs"]
pub(crate) mod log;

// Namespace-local PIDs and render counters can collide across normal sandboxes.
// This lifetime identity exposes no host PID or host namespace information.
#[cfg(feature = "native")]
pub(crate) fn observer_session_id() -> uuid::Uuid {
    static SESSION: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();
    *SESSION.get_or_init(uuid::Uuid::new_v4)
}

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
    /// # Errors
    /// Rejects invalid names, intervals, step counts, warmup bounds, non-finite inputs,
    /// or timelines whose final deadline exceeds the clock range.
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
        deadline(
            crate::time::now(),
            self.interval_us,
            self.steps.len().saturating_sub(1),
        )?;
        Ok(())
    }
}

fn parse(contents: &str) -> Result<Scenario, String> {
    let scenario: Scenario = serde_json::from_str(contents)
        .map_err(|error| format!("invalid native scenario JSON: {error}"))?;
    scenario.validate()?;
    Ok(scenario)
}

/// Loads a scenario and verifies any opted-in collector's exact source hash.
///
/// # Errors
/// Returns an error for invalid gate configuration, unreadable scenario bytes,
/// a mismatched gated source hash, or an invalid scenario.
pub fn load_from_env() -> Result<Option<Scenario>, String> {
    let gate = GateConfig::from_env()?;
    let Some(path) = std::env::var_os("TILO_NATIVE_SCENARIO") else {
        return if gate.is_some() {
            Err("native input gate requires a scenario file".into())
        } else {
            Ok(None)
        };
    };
    let path = std::path::PathBuf::from(path);
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read native scenario {}: {error}", path.display()))?;
    if let Some(gate) = gate {
        gate.verify_source(&contents)?;
    }
    parse(&contents).map(Some)
}

/// Opt-in diagnostic protocol only. Each non-hover step needs its own fresh
/// collector reply; ordinary benchmark timelines do not create or use a gate.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GateConfig {
    fd: i32,
    run_nonce: String,
    scenario_sha256: String,
}

impl GateConfig {
    fn from_env() -> Result<Option<Self>, String> {
        let contents = match std::env::var("TILO_NATIVE_INPUT_GATE") {
            Ok(contents) => contents,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(format!("invalid native input gate environment: {error}")),
        };
        let config: Self = serde_json::from_str(&contents)
            .map_err(|error| format!("invalid native input gate configuration: {error}"))?;
        let hex = |value: &str, size| {
            value.len() == size
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if config.fd < 3 || !hex(&config.run_nonce, 32) || !hex(&config.scenario_sha256, 64) {
            return Err("invalid native input gate descriptor or identity".into());
        }
        Ok(Some(config))
    }

    fn verify_source(&self, contents: &str) -> Result<(), String> {
        use sha2::{Digest, Sha256};
        if hex::encode(Sha256::digest(contents.as_bytes())) != self.scenario_sha256 {
            return Err("native input gate scenario byte hash mismatch".into());
        }
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GateRequest {
    pid: u32,
    run_nonce: String,
    scenario_sha256: String,
    step_id: usize,
    challenge: String,
}

#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReply {
    request: GateRequest,
    approved: bool,
}

struct InputGate {
    #[cfg(unix)]
    stream: std::os::unix::net::UnixStream,
    #[cfg_attr(not(unix), allow(dead_code, reason = "the gate only opens on Unix"))]
    config: GateConfig,
}

impl InputGate {
    fn from_env(scenario: &Scenario) -> Result<Option<Self>, String> {
        let Some(config) = GateConfig::from_env()? else {
            return Ok(None);
        };
        // Re-read at thread creation: the exact bytes must match the collector
        // hash, and those bytes must describe the scenario we will schedule.
        let path = std::env::var_os("TILO_NATIVE_SCENARIO")
            .ok_or("native input gate requires a scenario file")?;
        let contents = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read gated native scenario: {error}"))?;
        config.verify_source(&contents)?;
        let loaded = parse(&contents)?;
        if serde_json::to_value(loaded).map_err(|error| error.to_string())?
            != serde_json::to_value(scenario).map_err(|error| error.to_string())?
        {
            return Err("native input gate scenario differs from scheduling input".into());
        }
        #[cfg(not(unix))]
        {
            Err("native input gate requires Unix stream support".into())
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            // SAFETY: fcntl validates the integer descriptor. A successful dup
            // gives this gate a distinct owned fd; the inherited original stays
            // owned by the caller. Drop closes only our duplicate.
            let duplicate = unsafe { libc::fcntl(config.fd, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return Err(format!(
                    "cannot duplicate native gate descriptor: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: duplicate is a freshly returned owned descriptor.
            let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(duplicate) };
            let mut kind: libc::c_int = 0;
            let mut size = libc::socklen_t::try_from(std::mem::size_of_val(&kind))
                .map_err(|_| "native gate socket option size overflow")?;
            // SAFETY: kind/size are live writable outputs with the exact stated
            // size. This rejects files and datagram sockets before any traffic.
            let status = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&raw mut kind).cast(),
                    &raw mut size,
                )
            };
            if status != 0 || kind != libc::SOCK_STREAM || stream.peer_addr().is_err() {
                return Err("native gate descriptor is not a connected Unix stream".into());
            }
            stream
                .set_nonblocking(false)
                .map_err(|error| error.to_string())?;
            Ok(Some(Self { stream, config }))
        }
    }

    /// The permit expires on the producer's monotonic clock. A stalled
    /// collector cannot open the gate; a stale reply cannot revive it. Denial
    /// or EOF revokes a pending request, but cannot retract an event already
    /// released. This is not an atomic guarantee of the OS session lock state.
    fn approve(&mut self, step_id: usize) -> Result<Instant, String> {
        #[cfg(not(unix))]
        {
            let _ = step_id;
            Err("native input gate unsupported".into())
        }
        #[cfg(unix)]
        {
            use std::io::{Read, Write};
            let expires = crate::time::now()
                .checked_add(Duration::from_secs(3))
                .ok_or("native gate deadline overflow")?;
            let request = GateRequest {
                pid: std::process::id(),
                run_nonce: self.config.run_nonce.clone(),
                scenario_sha256: self.config.scenario_sha256.clone(),
                step_id,
                challenge: uuid::Uuid::new_v4().simple().to_string(),
            };
            let mut bytes = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
            bytes.push(b'\n');
            self.stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .map_err(|error| error.to_string())?;
            self.stream
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            let mut reply = Vec::new();
            loop {
                let remaining = expires
                    .checked_duration_since(crate::time::now())
                    .filter(|duration| !duration.is_zero())
                    .ok_or("native gate permit timed out")?;
                self.stream
                    .set_read_timeout(Some(remaining))
                    .map_err(|error| error.to_string())?;
                let mut chunk = [0_u8; 512];
                let count = self
                    .stream
                    .read(&mut chunk)
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    return Err("native gate collector closed".into());
                }
                reply.extend(chunk.into_iter().take(count));
                if reply.len() > 4096 {
                    return Err("native gate reply too large".into());
                }
                if let Some(end) = reply.iter().position(|byte| *byte == b'\n') {
                    if end != reply.len().saturating_sub(1) {
                        return Err("native gate has extra queued replies".into());
                    }
                    break;
                }
            }
            let reply: GateReply =
                serde_json::from_slice(&reply).map_err(|error| error.to_string())?;
            if !reply.approved || reply.request != request || crate::time::now() >= expires {
                return Err("native gate permit denied, stale, or mismatched".into());
            }
            Ok(expires)
        }
    }
}

/// Compute an absolute deadline without wrapping either duration or clock range.
///
/// # Errors
/// Rejects a zero interval or an index, duration, or deadline that overflows.
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
///
/// # Errors
/// Returns an error for an invalid timeline, opted-in gate configuration/source/
/// descriptor, unsupported opted-in gate platform, or thread creation failure.
pub fn spawn(
    scenario: Scenario,
    mut deliver: impl FnMut(Packet) -> bool + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    scenario.validate()?;
    let mut gate = InputGate::from_env(&scenario)?;
    let origin = crate::time::now();
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
        .name("tilo-native-scenario".into())
        .spawn(move || {
            for mut packet in packets {
                // A late callback never shifts the remaining schedule. Checking
                // again after sleep also guards against an early clock wakeup.
                while let Some(remaining) =
                    packet.scheduled_at.checked_duration_since(crate::time::now())
                {
                    if remaining.is_zero() {
                        break;
                    }
                    std::thread::sleep(remaining);
                }
                let expires = if !matches!(packet.input, Input::Hover { .. })
                    && let Some(gate) = gate.as_mut() {
                    match gate.approve(packet.id) {
                        Ok(expires) => Some(expires),
                        Err(error) => {
                            log::emit_json("tilo-native-gate", serde_json::json!({
                                "event":"denied", "step_id":packet.id, "reason":error }));
                            break;
                        }
                    }
                } else { None };
                (packet.sent_at, packet.sent_host) = host_clock::sample();
                // Recheck immediately before the callback, including time spent
                // descheduled after receipt or sampling the diagnostic clock.
                if expires.is_some_and(|expires| crate::time::now() >= expires) {
                    log::emit_json("tilo-native-gate", serde_json::json!({
                        "event":"denied", "step_id":packet.id, "reason":"permit expired before delivery" }));
                    break;
                }
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

    // A subprocess isolates opt-in environment and descriptor ownership from
    // concurrently running tests. This is the real public load/spawn path.
    #[cfg(unix)]
    #[test]
    fn gate_subprocess_fixture() {
        if std::env::var_os("TILO_NATIVE_GATE_TEST_CHILD").is_none() {
            return;
        }
        let result = load_from_env().and_then(|scenario| {
            let mut scenario = scenario.expect("fixture scenario");
            if std::env::var("TILO_NATIVE_GATE_TEST_CHILD").as_deref()
                == Ok("changed-scheduling-input")
            {
                scenario.name = "different scheduling input".into();
            }
            spawn(scenario, |packet| {
                println!("gate-test-delivered {}", packet.id);
                true
            })
        });
        match result {
            Ok(thread) => thread.join().unwrap(),
            Err(error) => println!("gate-test-refused {error}"),
        }
    }

    #[cfg(unix)]
    struct GateChild {
        child: std::process::Child,
        socket: std::os::unix::net::UnixStream,
        output: std::path::PathBuf,
        _directory: tempfile::TempDir,
    }

    #[cfg(unix)]
    impl GateChild {
        fn start(interval_us: u64, mutate: impl FnOnce(&mut serde_json::Value)) -> Self {
            Self::start_mode(interval_us, mutate, "1")
        }

        fn start_mode(
            interval_us: u64,
            mutate: impl FnOnce(&mut serde_json::Value),
            mode: &str,
        ) -> Self {
            use sha2::{Digest, Sha256};
            use std::os::fd::AsRawFd;
            use std::os::unix::process::CommandExt;
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("scenario.json");
            let contents = format!(
                r#"{{"name":"gate fixture","interval_us":{interval_us},"warmup_steps":1,"steps":[{{"kind":"hover","x":1,"y":1}},{{"kind":"click","x":2,"y":2}},{{"kind":"key","key":"enter"}}]}}"#
            );
            std::fs::write(&path, &contents).unwrap();
            let (socket, inherited) = std::os::unix::net::UnixStream::pair().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let non_socket = std::fs::File::open(&path).unwrap();
            let fd = if mode == "non-socket" {
                non_socket.as_raw_fd()
            } else {
                inherited.as_raw_fd()
            };
            let mut config = serde_json::json!({"fd":fd,"run_nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","scenario_sha256":hex::encode(Sha256::digest(contents.as_bytes()))});
            mutate(&mut config);
            let output = directory.path().join("output.log");
            let file = std::fs::File::create(&output).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "native_scenario::tests::gate_subprocess_fixture",
                    "--nocapture",
                ])
                .env("TILO_NATIVE_GATE_TEST_CHILD", mode)
                .env("TILO_NATIVE_SCENARIO", &path)
                .env("TILO_NATIVE_INPUT_GATE", config.to_string())
                .stdout(file.try_clone().unwrap())
                .stderr(file);
            // SAFETY: in the forked child, clear CLOEXEC only on our live socket;
            // the parent retains its own descriptor flags and ownership.
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            drop(inherited);
            Self {
                child,
                socket,
                output,
                _directory: directory,
            }
        }

        fn request(&self) -> serde_json::Value {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::BufReader::new(&self.socket)
                .read_line(&mut line)
                .unwrap();
            serde_json::from_str(&line).expect("real producer must request permission before input")
        }

        fn reply(&mut self, request: serde_json::Value, approved: bool) {
            use std::io::Write;
            let reply = serde_json::json!({"request":request,"approved":approved});
            writeln!(self.socket, "{reply}").unwrap();
        }

        fn finish(&mut self) -> String {
            tilo_test_env::wait_until("the fixture child to finish", || {
                self.child.try_wait().unwrap().is_some()
            });
            std::fs::read_to_string(&self.output).unwrap()
        }
    }

    #[cfg(unix)]
    impl Drop for GateChild {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(unix)]
    fn delivered_ids(output: &str) -> Vec<usize> {
        output
            .lines()
            .filter_map(|line| line.strip_prefix("gate-test-delivered "))
            .map(|id| id.parse().unwrap())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn gate_releases_only_exact_current_step_approvals() {
        // Case `exact_current_step_approvals_release_each_non_hover_once`.
        {
            let mut child = GateChild::start(1_000, |_| {});
            let first = child.request();
            assert_eq!(first["step_id"], 1);
            assert_eq!(first["pid"], child.child.id());
            child.reply(first.clone(), true);
            let second = child.request();
            assert_eq!(second["step_id"], 2);
            assert_ne!(first["challenge"], second["challenge"]);
            child.reply(second, true);
            assert_eq!(delivered_ids(&child.finish()), [0, 1, 2]);
        }

        // Case `revoked_wrong_or_replayed_permits_never_release_the_pending_step`.
        {
            for field in [
                "approved",
                "run_nonce",
                "scenario_sha256",
                "pid",
                "step_id",
                "challenge",
            ] {
                let mut child = GateChild::start(1_000, |_| {});
                let mut request = child.request();
                match field {
                    "run_nonce" => request[field] = "b".repeat(32).into(),
                    "scenario_sha256" => request[field] = "0".repeat(64).into(),
                    "pid" => request[field] = serde_json::json!(0),
                    "step_id" => request[field] = serde_json::json!(2),
                    "challenge" => request[field] = "f".repeat(32).into(),
                    _ => {}
                }
                child.reply(request, field != "approved");
                assert_eq!(delivered_ids(&child.finish()), [0], "{field}");
            }
            let mut child = GateChild::start(1_000, |_| {});
            let first = child.request();
            child.reply(first.clone(), true);
            let _second = child.request();
            child.reply(first, true);
            assert_eq!(delivered_ids(&child.finish()), [0, 1]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn stalled_collector_cannot_release_click_after_old_twelve_second_cadence() {
        let mut child = GateChild::start(12_000_000, |_| {});
        tilo_test_env::wait_until("the fixture's initial hover", || {
            !delivered_ids(&std::fs::read_to_string(&child.output).unwrap()).is_empty()
        });
        // No collector reads or approval writes for longer than the original
        // 12s cadence. Removing the producer gate delivers click1 in this gap.
        std::thread::sleep(Duration::from_millis(12_250));
        let _ = child.child.kill();
        child.child.wait().unwrap();
        assert_eq!(
            delivered_ids(&std::fs::read_to_string(&child.output).unwrap()),
            [0]
        );
    }

    #[cfg(unix)]
    #[test]
    fn gate_fails_closed_on_invalid_configuration_and_transport() {
        // Case `invalid_gate_configuration_and_unverified_source_hash_fail_closed`.
        {
            for (field, value) in [
                ("fd", serde_json::json!(-1)),
                ("fd", serde_json::json!(1)),
                ("run_nonce", serde_json::json!("")),
                ("scenario_sha256", serde_json::json!("0".repeat(64))),
            ] {
                let mut child = GateChild::start(1_000, |config| config[field] = value);
                let output = child.finish();
                assert!(output.contains("gate-test-refused"), "{output}");
                assert!(delivered_ids(&output).is_empty());
            }
        }

        // Case `gate_rejects_non_socket_and_changed_scheduling_input`.
        {
            let mut malformed = GateChild::start(1_000, |config| {
                *config = serde_json::json!("not a gate config")
            });
            let output = malformed.finish();
            assert!(output.contains("gate-test-refused"), "{output}");
            assert!(delivered_ids(&output).is_empty());
            for mode in ["non-socket", "changed-scheduling-input"] {
                let mut child = GateChild::start_mode(1_000, |_| {}, mode);
                let output = child.finish();
                assert!(output.contains("gate-test-refused"), "{output}");
                assert!(delivered_ids(&output).is_empty());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn closed_malformed_oversized_or_expired_gate_never_delivers_non_hover() {
        use std::io::Write;
        for bytes in [
            Vec::new(),
            b"not json\n".to_vec(),
            vec![b'x'; 4097],
            b"{\"approved\":".to_vec(),
        ] {
            let mut child = GateChild::start(1_000, |_| {});
            let _request = child.request();
            let _ = child.socket.write_all(&bytes);
            child.socket.shutdown(std::net::Shutdown::Write).unwrap();
            assert_eq!(delivered_ids(&child.finish()), [0]);
        }
        let mut child = GateChild::start(1_000, |_| {});
        let request = child.request();
        std::thread::sleep(Duration::from_millis(3_100));
        let reply = serde_json::json!({"request":request,"approved":true});
        let _ = writeln!(child.socket, "{reply}");
        assert_eq!(delivered_ids(&child.finish()), [0]);
    }

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

    fn scenario() -> Scenario {
        Scenario {
            name: "native wheel".into(),
            interval_us: 1_000,
            warmup_steps: 1,
            steps: vec![Input::Wheel { dx: 0.0, dy: 12.0 }; 3],
        }
    }

    #[test]
    fn scheduling_keeps_deadlines_and_stops_on_a_closed_receiver() {
        // Case `delayed_delivery_does_not_shift_future_deadlines`.
        {
            let observed = Arc::new(Mutex::new(Vec::new()));
            let output = observed.clone();
            spawn(scenario(), move |packet| {
                if packet.id == 0 {
                    std::thread::sleep(crate::time::Duration::from_millis(8));
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
            assert!(
                packets[1].sent_at.duration_since(packets[0].origin) >= Duration::from_millis(8)
            );

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
            let has_host_clock = match host_clock::CLOCK_NAME {
                "mach_absolute_seconds" | "clock_monotonic_seconds" => true,
                "unavailable" => false,
                other => panic!("unknown host clock: {other}"),
            };
            for packet in packets.iter() {
                if has_host_clock {
                    let bounds = packet.sent_host.expect("per-packet host bounds");
                    assert!(bounds.lo_s.is_finite() && bounds.lo_s > 0.0);
                    assert!(bounds.hi_s.is_finite() && bounds.hi_s >= bounds.lo_s);
                } else {
                    assert!(packet.sent_host.is_none());
                }
            }
            if has_host_clock {
                for pair in packets.windows(2) {
                    assert!(pair[0].sent_host.unwrap().hi_s <= pair[1].sent_host.unwrap().lo_s);
                }
            }
        }

        // Case `closed_receiver_stops_scheduling`.
        {
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
    }

    #[test]
    fn scenario_json_loads_validated_and_rejects_ambiguous_or_overflowing_input() {
        // Case `loads_validated_json_with_optional_warmup`.
        {
            let loaded = parse(
                r#"{"name":"hover","interval_us":16667,"steps":[{"kind":"hover","x":10,"y":20}]}"#,
            )
            .unwrap();
            assert_eq!(loaded.warmup_steps, 0);
            assert!(matches!(loaded.steps[0], Input::Hover { x: 10.0, y: 20.0 }));
            assert!(
                parse(
                    r#"{"name":"hover","interval_us":0,"steps":[{"kind":"hover","x":10,"y":20}]}"#
                )
                .is_err()
            );
            assert!(
                parse(
                    r#"{"name":"typo","interval_us":1,"steps":[{"kind":"whell","dx":0,"dy":1}]}"#
                )
                .is_err()
            );
        }

        // Case `keyboard_schema_preserves_unicode_and_rejects_ambiguous_input`.
        {
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

        // Case `rejects_invalid_scenario_before_scheduling`.
        {
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

        // Case `checked_timeline_rejects_overflow`.
        {
            let origin = crate::time::now();
            assert!(deadline(origin, u64::MAX, 2).is_err());
            let mut invalid = scenario();
            invalid.interval_us = u64::MAX;
            assert!(invalid.validate().is_err());
        }
    }
}
