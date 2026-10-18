//! The session report program over recorded logins: its JSON, the render
//! gate and the typing-latency table.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; one recorded login per test"
)]

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use tilo_session_record::value::Value;

const REPORTER: &str = env!("CARGO_BIN_EXE_tilo-session-report");

fn report(root: &Path, extra: &[&str]) -> Output {
    Command::new(REPORTER)
        .arg("--root")
        .arg(root)
        .args(extra)
        .output()
        .expect("reporter")
}

/// The report's JSON, read back with the minimal parser below.
fn json(text: &str) -> Value {
    let mut parser = Parser {
        text: text.as_bytes(),
        at: 0,
    };
    let value = parser.value();
    parser.space();
    assert_eq!(parser.at, text.len(), "trailing text after the report");
    value
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.text[self.at..].starts_with(literal.as_bytes()) {
            self.at += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Value {
        self.space();
        for (literal, value) in [
            ("null", Value::Null),
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
        ] {
            if self.eat(literal) {
                return value;
            }
        }
        match self.text[self.at] {
            b'{' => {
                self.at += 1;
                let mut map = Value::map();
                self.space();
                if self.eat("}") {
                    return map;
                }
                loop {
                    self.space();
                    let Value::Str(key) = self.value() else {
                        panic!("a key is a string")
                    };
                    self.space();
                    assert!(self.eat(":"));
                    let value = self.value();
                    map.set(&key, value);
                    self.space();
                    if self.eat("}") {
                        return map;
                    }
                    assert!(self.eat(","));
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat("]") {
                    return Value::List(items);
                }
                loop {
                    items.push(self.value());
                    self.space();
                    if self.eat("]") {
                        return Value::List(items);
                    }
                    assert!(self.eat(","));
                }
            }
            b'"' => {
                self.at += 1;
                let mut text = String::new();
                loop {
                    match self.text[self.at] {
                        b'"' => {
                            self.at += 1;
                            return Value::Str(text);
                        }
                        b'\\' => {
                            let escaped = self.text[self.at + 1];
                            self.at += 2;
                            match escaped {
                                b'n' => text.push('\n'),
                                b't' => text.push('\t'),
                                b'r' => text.push('\r'),
                                b'u' => {
                                    let hex = std::str::from_utf8(&self.text[self.at..self.at + 4])
                                        .expect("hex");
                                    self.at += 4;
                                    let unit = u32::from_str_radix(hex, 16).expect("unit");
                                    text.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
                                }
                                other => text.push(char::from(other)),
                            }
                        }
                        byte => {
                            text.push(char::from(byte));
                            self.at += 1;
                        }
                    }
                }
            }
            _ => {
                let start = self.at;
                while self
                    .text
                    .get(self.at)
                    .is_some_and(|byte| byte.is_ascii_digit() || b"-+.eE".contains(byte))
                {
                    self.at += 1;
                }
                let number = std::str::from_utf8(&self.text[start..self.at]).expect("number");
                number
                    .parse::<i64>()
                    .map_or_else(|_| Value::Float(number.parse().expect("float")), Value::Int)
            }
        }
    }
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |value, key| {
        value
            .get(key)
            .unwrap_or_else(|| panic!("no {key} in {}", value.repr()))
    })
}

fn stat(count: i64, p50: i64, p95: i64, max: i64) -> Value {
    let mut stats = Value::map();
    stats.set("count", count);
    stats.set("p50", p50);
    stats.set("p95", p95);
    stats.set("max", max);
    stats
}

fn same(left: &Value, right: &Value) -> bool {
    left.to_json() == right.to_json()
}

#[test]
fn a_previous_login_is_summarised_into_private_json() {
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path();
    fs::write(
        root.join("previous.log"),
        "paint-stats: 4/7 painted | events=1 revision=0 anim=3 session=0 keepalive=0 | 144.027 Hz target | present=9 backpressure=12\n\
         application stopped before presenting a window \x1b[3mpackage_id\x1b[0m\x1b[2m=\x1b[0mdev.tilo.terminal \x1b[3mdisplay_name\x1b[0m\x1b[2m=\x1b[0mTerminal \x1b[3mexit_code\x1b[0m\x1b[2m=\x1b[0mSome(1)\n\
         Connected to Xwayland on :1\n\
         tilo compositor: DRM scanout: modeset complete connector=42 width=2880 height=1800 refresh_mhz=144000\n\
         frames=287 direct_frames=287 direct_dropped=3 flip_completions=286 unprofiled_frames=2 window_us=2000199 bridge_us=1700000 scanout_us=24000 readback_bytes=0 output_bytes=0 direct_scanout_status=enabled direct_scanout_reason=\"not-initialized\" DRM render phase summary\n\
         direct_scanout_status=enabled renderer_pci=0000:66:00.0 drm_bus_id=\"pci:0000:66:00.0\" DRM direct scanout enabled\n\
         package_id=chatgpt request_to_dispatch_us=1200 dispatching user-requested host application launch\n\
         package_id=chatgpt pid=123 request_to_spawn_us=8200 application launcher spawned\n\
         package_id=chatgpt surface_id=77 request_to_map_us=42000 spawn_to_map_us=33800 application toplevel mapped\n\
         package_id=chatgpt surface_id=77 frame_seq=1 request_to_first_present_us=50000 spawn_to_first_present_us=41800 map_to_first_present_us=8000 application first frame presented\n\
         paint-stats: 0/12025 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | 144.027 Hz target | present=9 backpressure=12\n\
         thread 'main' panicked at wayland-backend/src/rs/server_impl/common_poll.rs:284:38:\n\
         tilo-flight-record-v1 status=134 logout_request=no\n",
    )
    .expect("flight record");
    fs::write(
        root.join("previous-frame-trace.csv"),
        "seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,submit_us,testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,input_retirement_latency_us,backpressured,deadline_late_us\n\
         0,0,0,1,2,3,4,5,0,6,7,1,1,8,9000,0,500\n\
         1,20000,20000,2,3,4,5,6,0,7,8,1,1,20000,24000,1,3500\n",
    )
    .expect("frame trace");
    let output = root.join("reports").join("incident.json");
    let ran = report(
        root,
        &["--previous", "--output", output.to_str().expect("path")],
    );
    assert!(ran.status.success(), "{ran:?}");
    let text = fs::read_to_string(&output).expect("report");
    assert!(
        text.ends_with("}\n") && text.starts_with("{\n  \"flight\": {\n"),
        "{text}"
    );
    let report = json(&text);
    assert_eq!(
        at(&report, &["schema"]).as_str(),
        Some("tilo-session-report-v1")
    );
    let flight = at(&report, &["flight"]);
    assert!(same(at(flight, &["app_failures"]), &{
        let mut failures = Value::map();
        failures.set("dev.tilo.terminal", 1_i64);
        failures
    }));
    assert_eq!(
        at(flight, &["paint_last", "backpressure"]).as_str(),
        Some("12")
    );
    assert!(at(flight, &["xwayland_ready"]).is_true());
    assert!(at(flight, &["fatal_panic"]).is_true());
    assert_eq!(
        at(flight, &["idle_wake_storm_max_ticks"]).as_int(),
        Some(12025)
    );
    assert!(at(flight, &["direct_scanout_enabled"]).is_true());
    let status = at(flight, &["direct_scanout_status_last"]);
    assert_eq!(at(status, &["status"]).as_str(), Some("enabled"));
    assert_eq!(at(status, &["reason"]), &Value::Null);
    assert_eq!(
        at(flight, &["direct_scanout_fallback_tail"]),
        &Value::List(vec![])
    );
    assert_eq!(
        at(flight, &["render_phase_last"])
            .to_json()
            .replace([' ', '\n'], ""),
        "{\"bridge_us\":1700000,\"direct_dropped\":3,\"direct_frames\":287,\"flip_completions\":286,\"frames\":287,\"output_bytes\":0,\"readback_bytes\":0,\"scanout_us\":24000,\"unprofiled_frames\":2,\"window_us\":2000199}"
    );
    assert_eq!(
        at(flight, &["kms_modes"])
            .to_json()
            .replace([' ', '\n'], ""),
        "[{\"connector\":42,\"height\":1800,\"refresh_mhz\":144000,\"width\":2880}]"
    );
    let chatgpt = at(flight, &["launch_latency_us", "chatgpt"]);
    for (metric, value) in [
        ("map_to_first_present_us", 8000),
        ("request_to_dispatch_us", 1200),
        ("request_to_first_present_us", 50000),
        ("request_to_map_us", 42000),
        ("request_to_spawn_us", 8200),
        ("spawn_to_first_present_us", 41800),
        ("spawn_to_map_us", 33800),
    ] {
        assert!(
            same(at(chatgpt, &[metric]), &stat(1, value, value, value)),
            "{metric}"
        );
    }
    let frames = at(&report, &["frames"]);
    assert_eq!(at(frames, &["painted"]).as_int(), Some(2));
    assert_eq!(at(frames, &["painted_fps"]), &Value::Float(100.0));
    assert!(
        text.contains("\"painted_fps\": 100.0,"),
        "floats print as Python's"
    );
    assert_eq!(
        at(frames, &["input_latency_us", "p99"]).as_int(),
        Some(20000)
    );
    assert_eq!(
        at(frames, &["input_retirement_latency_us", "p99"]).as_int(),
        Some(24000)
    );
    assert_eq!(at(frames, &["backpressured_ticks"]).as_int(), Some(1));
    assert_eq!(
        at(frames, &["deadline_late_us", "p99"]).as_int(),
        Some(3500)
    );
    assert_eq!(
        at(frames, &["paint_period_us", "p99"]).as_int(),
        Some(20000)
    );
    assert_eq!(
        fs::metadata(&output)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

fn gate(root: &Path, log: &str, extra: &[&str]) -> Output {
    fs::write(root.join("latest.log"), log).expect("flight record");
    let mut args = vec!["--render-gate"];
    args.extend_from_slice(extra);
    report(root, &args)
}

#[test]
fn the_render_gate_holds_presentation_to_its_contract() {
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path();
    let enabled = "direct_scanout_status=enabled format=Xrgb8888 renderer_pci= earlier_failures=0 DRM direct scanout enabled\n";
    let degraded = "direct_scanout_status=degraded direct_scanout_reason=\"render: GPU-only absolute deadline exceeded\" output=1 hdr=false failures=1 retry_in_ms=500 DRM direct scanout degraded to readback\n";
    let readback_summary = "frames=40 direct_frames=0 direct_dropped=0 flip_completions=0 unprofiled_frames=0 window_us=2000000 bridge_us=1800000 scanout_us=3000 readback_bytes=866713600 output_bytes=866713600 avg_direct_us=0 max_direct_us=0 direct_failures=1 direct_scanout_status=unavailable direct_scanout_reason=\"render: GPU-only absolute deadline exceeded\" DRM render phase summary\n";
    let direct_summary = "frames=120 direct_frames=120 direct_dropped=0 flip_completions=120 unprofiled_frames=0 window_us=2000000 bridge_us=0 scanout_us=0 readback_bytes=0 output_bytes=0 avg_direct_us=6100 max_direct_us=9800 direct_failures=1 direct_scanout_status=enabled direct_scanout_reason=\"not-initialized\" DRM render phase summary\n";

    // A demotion that never recovers fails the gate.
    let failed = gate(
        root,
        &[enabled, direct_summary, degraded, readback_summary].concat(),
        &[],
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(
        stderr.contains("direct GPU scanout is not presenting"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "status=unavailable reason=\"render: GPU-only absolute deadline exceeded\" readback_bytes=866713600); degradations: ['direct_scanout_status=degraded"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("readback frames averaged 45000 us"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&failed.stdout);
    assert!(
        stdout.starts_with("render gate: last phase summary {'frames': 40, 'direct_frames': 0,"),
        "{stdout}"
    );
    assert!(
        stdout.contains("render gate: main-thread frame work None\n"),
        "{stdout}"
    );

    // A retried demotion passes.
    let passed = gate(
        root,
        &[enabled, degraded, enabled, direct_summary].concat(),
        &[],
    );
    assert!(passed.status.success(), "{passed:?}");
    assert!(String::from_utf8_lossy(&passed.stdout).contains("render gate: PASS"));

    // Direct frames over budget fail.
    let slow = gate(
        root,
        &[enabled, direct_summary].concat(),
        &["--render-budget-ms", "5"],
    );
    assert_eq!(slow.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&slow.stderr).contains("direct frames averaged 6100 us"));

    // A required direct presenter must start.
    let software = "direct_scanout_status=unavailable reason=\"renderer and scanout use different GPUs\" DRM direct scanout unavailable\n\
        frames=30 direct_frames=0 direct_dropped=0 flip_completions=0 unprofiled_frames=0 window_us=2000000 bridge_us=300000 scanout_us=3000 readback_bytes=1 output_bytes=1 direct_scanout_status=unavailable direct_scanout_reason=\"renderer and scanout use different GPUs\" DRM render phase summary\n";
    assert!(gate(root, software, &[]).status.success());
    assert_eq!(
        gate(root, software, &["--require-direct"]).status.code(),
        Some(1)
    );

    // No record at all.
    fs::remove_file(root.join("latest.log")).expect("remove");
    let missing = report(root, &["--render-gate"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no session flight record at"));
}

/// The device's latency bar, opt-in: keys show within the budget at the
/// median, and most direct frames repair only their damage or scan a
/// client out instead of rendering the whole output.
#[test]
fn the_render_gate_holds_typing_and_damage_to_the_latency_bar() {
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path();
    let enabled = "direct_scanout_status=enabled format=Xrgb8888 renderer_pci= earlier_failures=0 DRM direct scanout enabled\n";
    let summary = |sparse: u32, scanout: u32| {
        format!(
            "frames=100 direct_frames=100 direct_dropped=0 flip_completions=100 unprofiled_frames=0 window_us=2000000 bridge_us=0 scanout_us=0 readback_bytes=0 output_bytes=0 avg_direct_us=6000 max_direct_us=9000 direct_sparse_frames={sparse} client_scanout_frames={scanout} direct_failures=0 direct_scanout_status=enabled direct_scanout_reason=\"not-initialized\" DRM render phase summary\n"
        )
    };
    let keys = |present_ms: [u32; 3]| {
        present_ms.iter().fold(String::new(), |mut lines, ms| {
            let _ = writeln!(
                lines,
                "KEY-LATENCY app=org.alacritty key_to_commit_us=3000 key_to_submit_us=5000 key_to_present_us={}",
                ms * 1000
            );
            lines
        })
    };
    let bar = [
        "--key-budget-ms",
        "20",
        "--min-damage-limited-percent",
        "80",
    ];

    let fast = gate(
        root,
        &[enabled.to_owned(), summary(50, 40), keys([12, 15, 40])].concat(),
        &bar,
    );
    assert!(fast.status.success(), "{fast:?}");

    let dense = gate(
        root,
        &[enabled.to_owned(), summary(13, 0), keys([12, 15, 40])].concat(),
        &bar,
    );
    assert_eq!(dense.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&dense.stderr).contains(
            "13 of 100 direct frames repaired only their damage or scanned a client out, under 80 %"
        ),
        "{dense:?}"
    );

    let slow = gate(
        root,
        &[enabled.to_owned(), summary(90, 0), keys([50, 57, 89])].concat(),
        &bar,
    );
    assert_eq!(slow.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&slow.stderr).contains(
            "keys typed into org.alacritty took 57000 us to show at the median over 3 keys, budget 20000 us"
        ),
        "{slow:?}"
    );

    // Without the flags, neither is gated.
    assert!(
        gate(
            root,
            &[enabled.to_owned(), summary(13, 0), keys([50, 57, 89])].concat(),
            &[]
        )
        .status
        .success()
    );
}

#[test]
fn typed_keys_aggregate_per_app() {
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path();
    fs::write(
        root.join("latest.log"),
        "KEY-LATENCY app=so.tilo.notes key_to_commit_us=9000 key_to_submit_us=10000 key_to_present_us=18000\n\
         KEY-LATENCY app=so.tilo.notes key_to_commit_us=30000 key_to_submit_us=31000 key_to_present_us=50000\n\
         KEY-LATENCY app=so.tilo.notes unanswered=1\n",
    )
    .expect("flight record");
    let ran = report(root, &[]);
    assert!(ran.status.success());
    let report_json = json(&String::from_utf8_lossy(&ran.stdout));
    let notes = at(&report_json, &["flight", "key_latency_us", "so.tilo.notes"]);
    assert_eq!(at(notes, &["unanswered"]).as_int(), Some(1));
    let mut present = Value::map();
    present.set("count", 2_i64);
    present.set("p50", 18_000_i64);
    present.set("p95", 50_000_i64);
    present.set("p99", 50_000_i64);
    present.set("max", 50_000_i64);
    assert!(same(at(notes, &["key_to_present_us"]), &present));

    let table = report(root, &["--key-latency"]);
    assert!(table.status.success());
    assert_eq!(
        String::from_utf8_lossy(&table.stdout),
        "so.tilo.notes: unanswered=1\n\
         \x20 key_to_commit    n=2     p50=9.0ms p95=30.0ms p99=30.0ms max=30.0ms\n\
         \x20 key_to_submit    n=2     p50=10.0ms p95=31.0ms p99=31.0ms max=31.0ms\n\
         \x20 key_to_present   n=2     p50=18.0ms p95=50.0ms p99=50.0ms max=50.0ms\n"
    );
}

#[test]
fn a_long_session_reads_launch_events_from_the_event_record() {
    // The full record holds only measurement lines by now, and the launch
    // timing and failure live in the recorder's event record.
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path();
    fs::write(
        root.join("latest.log"),
        "paint-stats: 3/14 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | 60.000 Hz target | present=1 backpressure=0\n"
            .repeat(64),
    )
    .expect("flight record");
    fs::write(
        root.join("events.log"),
        "tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/shell\n\
         2026-10-11T02:22:31.204118Z  INFO tilo::launch_latency: package_id=com.onepassword.OnePassword pid=9 request_to_spawn_us=1200000 application launcher spawned\n\
         2026-10-11T02:22:45.760719Z ERROR tilo::app_launch: application stopped before presenting a window package_id=com.onepassword.OnePassword display_name=1Password exit_code=None\n",
    )
    .expect("event record");
    let output = root.join("current.json");
    let ran = report(root, &["--output", output.to_str().expect("path")]);
    assert!(ran.status.success());
    let flight_json = json(&fs::read_to_string(&output).expect("report"));
    let flight = at(&flight_json, &["flight"]);
    assert_eq!(
        at(flight, &["app_failures", "com.onepassword.OnePassword"]).as_int(),
        Some(1)
    );
    assert_eq!(
        at(
            flight,
            &[
                "launch_latency_us",
                "com.onepassword.OnePassword",
                "request_to_spawn_us",
                "max"
            ]
        )
        .as_int(),
        Some(1_200_000)
    );
    assert_eq!(at(flight, &["paint_last", "painted"]).as_str(), Some("3"));

    // An unknown option is a usage error, as argparse reports it.
    let bad = report(root, &["--no-such-option"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).starts_with("usage: tilo-session-report"));
}
