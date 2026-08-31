//! Journal sources: live `journalctl -o json` parsing for the running
//! app, plus a bundled, realistic sample journal so `--shots` renders a
//! populated screen with no systemd access.

use std::process::Command;

use serde_json::Value;

use crate::model::{LogEntry, Priority};

/// Fields the UI promotes to dedicated columns / struct members. Every
/// other field is preserved in [`LogEntry::extra`] for the detail pane.
const PROMOTED: &[&str] = &[
    "__REALTIME_TIMESTAMP",
    "_SYSTEMD_UNIT",
    "SYSLOG_IDENTIFIER",
    "_COMM",
    "PRIORITY",
    "MESSAGE",
    "_PID",
    "_HOSTNAME",
    "_BOOT_ID",
    "_TRANSPORT",
    "__CURSOR",
    "__MONOTONIC_TIMESTAMP",
];

/// (`offset_ms`, unit, identifier, priority, pid, transport, message, extra)
type Row = (
    u64,
    &'static str,
    &'static str,
    Priority,
    Option<u32>,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Sample rows: boot through early service startup.
const ROWS_BOOT: &[Row] = &[
    (
        0,
        "kernel",
        "kernel",
        Priority::Info,
        None,
        "kernel",
        "Linux version 7.0.9-ogc3.2.fc44.x86_64 (Fedora Bootc) #1 SMP PREEMPT_DYNAMIC",
        &[("SYSLOG_FACILITY", "0"), ("_KERNEL_SUBSYSTEM", "boot")],
    ),
    (
        61,
        "kernel",
        "kernel",
        Priority::Info,
        None,
        "kernel",
        "Command line: BOOT_IMAGE=/boot/vmlinuz root=UUID=… rw quiet rootflags=ro",
        &[("SYSLOG_FACILITY", "0")],
    ),
    (
        140,
        "kernel",
        "kernel",
        Priority::Warning,
        None,
        "kernel",
        "ACPI: _OSC failed (AE_NOT_FOUND); disabling ASPM on this platform",
        &[("SYSLOG_FACILITY", "0")],
    ),
    (
        320,
        "kernel",
        "kernel",
        Priority::Info,
        None,
        "kernel",
        "amdgpu: detected VRAM 8176M, BAR=256M; initializing GPUVM for vega",
        &[("_KERNEL_DEVICE", "+drm:card0")],
    ),
    (
        512,
        "systemd",
        "systemd",
        Priority::Info,
        Some(1),
        "stdout",
        "Reached target Basic System.",
        &[("CODE_FILE", "src/core/job.c"), ("UNIT", "basic.target")],
    ),
    (
        690,
        "quick-fs.service",
        "quick-fs",
        Priority::Info,
        Some(very(641)),
        "stdout",
        "Mounting collaborative workspace at /var/workspace (iroh node starting)",
        &[("CODE_FILE", "fs/src/mount.rs"), ("THREAD", "main")],
    ),
    (
        930,
        "quick-fs.service",
        "quick-fs",
        Priority::Notice,
        Some(very(641)),
        "stdout",
        "iroh endpoint bound: relay=eu-central, node_id=k51qzi5… (4 candidate paths)",
        &[("CODE_FILE", "fs/src/p2p.rs")],
    ),
    (
        1180,
        "quick-fs.service",
        "quick-fs",
        Priority::Warning,
        Some(very(641)),
        "stdout",
        "metadata gossip slow: 1820ms for 3 peers (TTL 10s) — degraded projection",
        &[("CODE_FILE", "fs/src/gossip.rs"), ("PEER_COUNT", "3")],
    ),
    (
        1400,
        "NetworkManager.service",
        "NetworkManager",
        Priority::Info,
        Some(very(703)),
        "syslog",
        "device (wlan0): supplicant interface state: scanning -> authenticating",
        &[("SYSLOG_FACILITY", "0"), ("NM_DEVICE", "wlan0")],
    ),
    (
        1610,
        "NetworkManager.service",
        "NetworkManager",
        Priority::Notice,
        Some(very(703)),
        "syslog",
        "device (wlan0): DHCPv4 lease acquired 10.42.0.18/24, gateway 10.42.0.1",
        &[("NM_DEVICE", "wlan0")],
    ),
    (
        1900,
        "shell.service",
        "shell",
        Priority::Info,
        Some(very(820)),
        "stdout",
        "compositor: smithay DRM backend up; output eDP-1 2560x1600@60 (hw cursor)",
        &[("CODE_FILE", "shell/src/backend/drm.rs")],
    ),
];

/// Sample rows: collaboration, filesystem, and network mid-boot activity.
const ROWS_SERVICES: &[Row] = &[
    (
        2010,
        "shell.service",
        "shell",
        Priority::Info,
        Some(very(820)),
        "stdout",
        "loaded 2D-cluster workspace tree: 3 clusters, 7 windows restored",
        &[("CODE_FILE", "shell/src/shell/state.rs")],
    ),
    (
        2240,
        "shell.service",
        "shell",
        Priority::Warning,
        Some(very(820)),
        "stdout",
        "sandbox: cluster 'web' requested /home/quinten/Downloads — prompting user",
        &[
            ("CODE_FILE", "shell/src/portal/access.rs"),
            ("CLUSTER", "web"),
        ],
    ),
    (
        2600,
        "pipewire.service",
        "pipewire",
        Priority::Info,
        Some(very(861)),
        "stdout",
        "audio: alsa node 'Generic Analog' linked; 48000Hz/2ch default sink",
        &[("CODE_FILE", "src/daemon/pipewire.c")],
    ),
    (
        2780,
        "pipewire.service",
        "pipewire",
        Priority::Notice,
        Some(very(861)),
        "stdout",
        "session manager: restored 4 stream routes from previous session",
        &[("CODE_FILE", "wireplumber")],
    ),
    (
        3100,
        "bluetooth.service",
        "bluetoothd",
        Priority::Err,
        Some(very(905)),
        "syslog",
        "Failed to set privacy: Rejected (0x0b) — adapter hci0 firmware quirk",
        &[("SYSLOG_FACILITY", "1"), ("BT_ADAPTER", "hci0")],
    ),
    (
        3400,
        "shell.service",
        "shell",
        Priority::Info,
        Some(very(820)),
        "stdout",
        "collab: peer 'mara@thinkpad' invited to cluster 'design' (gossip topic ok)",
        &[("CODE_FILE", "shell/src/collab/mod.rs"), ("PEER", "mara")],
    ),
    (
        3650,
        "quick-fs.service",
        "quick-fs",
        Priority::Info,
        Some(very(641)),
        "stdout",
        "hydrated blob bafyb… (1.4MB) on first read of /var/workspace/notes/spec.md",
        &[("CODE_FILE", "fs/src/hydrate.rs"), ("SIZE", "1466368")],
    ),
    (
        3900,
        "sshd.service",
        "sshd",
        Priority::Notice,
        Some(very(1022)),
        "syslog",
        "Accepted publickey for quinten from 10.42.0.5 port 51344 ssh2: ED25519",
        &[("SYSLOG_FACILITY", "4"), ("SSH_USER", "quinten")],
    ),
    (
        4120,
        "sshd.service",
        "sshd",
        Priority::Warning,
        Some(very(1022)),
        "syslog",
        "Invalid user admin from 193.118.52.7 port 40122 — 3rd attempt this minute",
        &[("SYSLOG_FACILITY", "4"), ("SSH_REMOTE", "193.118.52.7")],
    ),
];

/// Sample rows: session steady-state, warnings, and errors.
const ROWS_SESSION: &[Row] = &[
    (
        4500,
        "shell.service",
        "shell",
        Priority::Err,
        Some(very(820)),
        "stdout",
        "wgpu: surface lost (Outdated) during cluster crossfade — recreating swapchain",
        &[
            ("CODE_FILE", "shell/src/render/frame.rs"),
            ("BACKEND", "vulkan"),
        ],
    ),
    (
        4520,
        "shell.service",
        "shell",
        Priority::Crit,
        Some(very(820)),
        "stdout",
        "renderer stalled 2 frames recreating swapchain; input queue depth 14",
        &[("CODE_FILE", "shell/src/render/frame.rs")],
    ),
    (
        4900,
        "quick-fs.service",
        "quick-fs",
        Priority::Err,
        Some(very(641)),
        "stdout",
        "conflict copy written: spec.md ⇒ spec.conflict-mara.md (concurrent edit)",
        &[
            ("CODE_FILE", "fs/src/conflict.rs"),
            ("PATH", "notes/spec.md"),
        ],
    ),
    (
        5200,
        "systemd",
        "systemd",
        Priority::Notice,
        Some(1),
        "stdout",
        "quick-firstboot.service: Deactivated successfully (first-run setup complete)",
        &[("UNIT", "quick-firstboot.service")],
    ),
    (
        5600,
        "kernel",
        "kernel",
        Priority::Debug,
        None,
        "kernel",
        "usb 1-4: new high-speed USB device number 6 using xhci_hcd",
        &[("_KERNEL_DEVICE", "+usb:1-4")],
    ),
    (
        5900,
        "shell.service",
        "shell",
        Priority::Debug,
        Some(very(820)),
        "stdout",
        "focus moved: cluster[1].window[2] 'terminal' — directional nav (ArrowRight)",
        &[("CODE_FILE", "shell/src/focus.rs")],
    ),
    (
        6300,
        "chronyd.service",
        "chronyd",
        Priority::Info,
        Some(very(560)),
        "syslog",
        "Selected source 162.159.200.1 (time.cloudflare.com), offset +0.000412s",
        &[("SYSLOG_FACILITY", "0")],
    ),
    (
        6700,
        "shell.service",
        "shell",
        Priority::Info,
        Some(very(820)),
        "stdout",
        "collab: remote cursor stream from 'mara@thinkpad' attached (12 fps)",
        &[
            ("CODE_FILE", "shell/src/collab/cursor.rs"),
            ("PEER", "mara"),
        ],
    ),
    (
        7050,
        "pipewire.service",
        "pipewire",
        Priority::Warning,
        Some(very(861)),
        "stdout",
        "xrun on node 'Firefox' — buffer underrun, raising quantum 256 → 512",
        &[("CODE_FILE", "src/daemon/pipewire.c")],
    ),
    (
        7400,
        "quick-fs.service",
        "quick-fs",
        Priority::Info,
        Some(very(641)),
        "stdout",
        "anti-entropy sweep complete: 0 divergent objects across 3 peers (820ms)",
        &[("CODE_FILE", "fs/src/anti_entropy.rs")],
    ),
    (
        7900,
        "shell.service",
        "shell",
        Priority::Notice,
        Some(very(820)),
        "stdout",
        "workspace snapshot persisted: 3 clusters → ~/.local/state/qos/wheel.json",
        &[("CODE_FILE", "shell/src/persist.rs")],
    ),
];

/// Run `journalctl -o json` and parse the most recent `limit` records.
/// Returns an empty vec (never an error) when journald is unavailable —
/// the caller falls back to the bundled sample so the UI is never blank.
pub fn load_live(limit: usize) -> Vec<LogEntry> {
    let output = Command::new("journalctl")
        .args(["-o", "json", "--no-pager", "-n", &limit.to_string()])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_json_stream(&String::from_utf8_lossy(&output.stdout))
}

/// Parse a newline-delimited `journalctl -o json` stream into entries,
/// sorted oldest → newest. Malformed lines are skipped, not fatal.
pub fn parse_json_stream(text: &str) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| entry_from_json(&v))
        .collect();
    out.sort_by_key(|e| e.realtime_us);
    out
}

/// Read a journal field that may be a JSON string, an integer, or a byte
/// array (journald encodes non-UTF8 fields as arrays). Returns `None`
/// when absent.
fn field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(arr) => {
            // Byte array → lossy UTF-8.
            let bytes: Vec<u8> = arr
                .iter()
                .filter_map(|b| b.as_u64().map(|n| n as u8))
                .collect();
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

fn entry_from_json(v: &Value) -> Option<LogEntry> {
    let obj = v.as_object()?;

    let realtime_us = field(v, "__REALTIME_TIMESTAMP")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let identifier = field(v, "SYSLOG_IDENTIFIER")
        .or_else(|| field(v, "_COMM"))
        .unwrap_or_default();

    let unit = field(v, "_SYSTEMD_UNIT")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if identifier.is_empty() {
                "(unknown)".to_string()
            } else {
                identifier.clone()
            }
        });

    let priority = field(v, "PRIORITY")
        .and_then(|s| s.parse::<i64>().ok())
        .map_or(Priority::Info, Priority::from_level);

    let message = field(v, "MESSAGE").unwrap_or_default();
    let pid = field(v, "_PID").and_then(|s| s.parse::<u32>().ok());
    let hostname = field(v, "_HOSTNAME").unwrap_or_default();
    let boot_id = field(v, "_BOOT_ID").unwrap_or_default();
    let transport = field(v, "_TRANSPORT").unwrap_or_default();

    // Everything not already promoted, kept for the detail pane.
    let mut extra: Vec<(String, String)> = obj
        .iter()
        .filter(|(k, _)| !PROMOTED.contains(&k.as_str()))
        .filter_map(|(k, val)| {
            field(v, k)
                .map(|s| (k.clone(), s))
                .or_else(|| Some((k.clone(), val.to_string())))
        })
        .collect();
    extra.sort_by(|a, b| a.0.cmp(&b.0));

    Some(LogEntry {
        realtime_us,
        unit,
        identifier,
        priority,
        message,
        pid,
        hostname,
        boot_id,
        transport,
        extra,
    })
}

// ===================================================================
// Bundled sample journal
// ===================================================================

/// A realistic offline journal: a qOS boot sequence across the shell
/// compositor, the FUSE filesystem, the kernel, networking, audio, and a
/// collaboration session — seeded with a scatter of warnings and errors
/// so the severity colouring and filters have something to bite on.
pub fn sample_entries() -> Vec<LogEntry> {
    // Base: 2026-06-30 14:20:00.000 UTC.
    let base: u64 = {
        let days: u64 = 20_634; // days from 1970-01-01 to 2026-06-30
        (days * 86_400 + 14 * 3_600 + 20 * 60) * 1_000_000
    };
    let host = "qos-station";
    let boot_a = "9f2c1e7b4a6d4c0fb1e3a8d5c2f70a11";
    let boot_b = "5a1d8c3e2b9f4710a6c4e0d7b385f922";

    let prev: &[Row] = &[(
        0,
        "systemd-journald.service",
        "systemd-journald",
        Priority::Notice,
        Some(very(412)),
        "journal",
        "Journal stopped",
        &[("CODE_FUNC", "server_done"), ("SYSLOG_FACILITY", "3")],
    )];

    let mut entries = Vec::new();

    // A handful of records from the previous boot, so the boot filter has
    // two distinct boots to switch between.
    let prev_base = base - 9_000_000_000; // ~2.5h earlier
    for (off, unit, ident, prio, pid, transport, msg, extra) in prev {
        entries.push(build(
            prev_base + off * 1000,
            unit,
            ident,
            *prio,
            *pid,
            transport,
            msg,
            host,
            boot_b,
            extra,
        ));
    }

    // Rows are split across three consts purely to stay under the
    // per-function line-count limit; the boot sequence still reads as one
    // continuous timeline (offsets are relative to the same `base`).
    let rows = ROWS_BOOT.iter().chain(ROWS_SERVICES).chain(ROWS_SESSION);
    for (off, unit, ident, prio, pid, transport, msg, extra) in rows {
        entries.push(build(
            base + off * 1000,
            unit,
            ident,
            *prio,
            *pid,
            transport,
            msg,
            host,
            boot_a,
            extra,
        ));
    }

    entries.sort_by_key(|e| e.realtime_us);
    entries
}

/// Tiny helper so the sample table reads cleanly: most PIDs are stable.
const fn very(n: u32) -> u32 {
    n
}

#[allow(clippy::too_many_arguments)]
fn build(
    realtime_us: u64,
    unit: &str,
    identifier: &str,
    priority: Priority,
    pid: Option<u32>,
    transport: &str,
    message: &str,
    hostname: &str,
    boot_id: &str,
    extra: &[(&str, &str)],
) -> LogEntry {
    LogEntry {
        realtime_us,
        unit: unit.to_string(),
        identifier: identifier.to_string(),
        priority,
        message: message.to_string(),
        pid,
        hostname: hostname.to_string(),
        boot_id: boot_id.to_string(),
        transport: transport.to_string(),
        extra: extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_has_mixed_severities_and_two_boots() {
        let e = sample_entries();
        assert!(e.len() > 20, "sample journal should be substantial");
        assert!(e.iter().any(|x| x.priority == Priority::Err));
        assert!(e.iter().any(|x| x.priority == Priority::Warning));
        assert!(e.iter().any(|x| x.priority == Priority::Crit));
        let boots: std::collections::HashSet<_> = e.iter().map(|x| x.boot_id.clone()).collect();
        assert_eq!(boots.len(), 2, "should span exactly two boots");
        // Sorted ascending by time.
        assert!(e.windows(2).all(|w| w[0].realtime_us <= w[1].realtime_us));
    }

    #[test]
    fn parse_json_stream_reads_journalctl_shape() {
        let line = r#"{"__REALTIME_TIMESTAMP":"1609459200000000","_SYSTEMD_UNIT":"x.service","PRIORITY":"3","MESSAGE":"boom","_PID":"42","_HOSTNAME":"h","_BOOT_ID":"b","_TRANSPORT":"stdout","CUSTOM":"keep"}"#;
        let parsed = parse_json_stream(line);
        assert_eq!(parsed.len(), 1);
        let e = &parsed[0];
        assert_eq!(e.unit, "x.service");
        assert_eq!(e.priority, Priority::Err);
        assert_eq!(e.message, "boom");
        assert_eq!(e.pid, Some(42));
        assert!(e.extra.iter().any(|(k, v)| k == "CUSTOM" && v == "keep"));
    }

    #[test]
    fn message_byte_array_is_decoded() {
        // journald encodes non-UTF8 MESSAGE as a byte array.
        let line = r#"{"__REALTIME_TIMESTAMP":"1","MESSAGE":[104,105],"PRIORITY":"6"}"#;
        let parsed = parse_json_stream(line);
        assert_eq!(parsed[0].message, "hi");
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let stream = "not json\n{\"MESSAGE\":\"ok\",\"__REALTIME_TIMESTAMP\":\"5\"}\n";
        let parsed = parse_json_stream(stream);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].message, "ok");
    }
}
