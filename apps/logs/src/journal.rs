//! Journal sources: live `journalctl -o json` parsing for the running
//! app, plus a bundled, realistic sample journal so `--shots` renders a
//! populated screen with no systemd access.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

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
        &[
            ("CODE_FILE", "crates/system/fs/src/mount.rs"),
            ("THREAD", "main"),
        ],
    ),
    (
        930,
        "quick-fs.service",
        "quick-fs",
        Priority::Notice,
        Some(very(641)),
        "stdout",
        "iroh endpoint bound: relay=eu-central, node_id=k51qzi5… (4 candidate paths)",
        &[("CODE_FILE", "crates/system/fs/src/p2p.rs")],
    ),
    (
        1180,
        "quick-fs.service",
        "quick-fs",
        Priority::Warning,
        Some(very(641)),
        "stdout",
        "metadata gossip slow: 1820ms for 3 peers (TTL 10s) — degraded projection",
        &[
            ("CODE_FILE", "crates/system/fs/src/gossip.rs"),
            ("PEER_COUNT", "3"),
        ],
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
        &[("CODE_FILE", "crates/session/shell/src/backend/drm.rs")],
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
        &[("CODE_FILE", "crates/session/shell/src/shell/state.rs")],
    ),
    (
        2240,
        "shell.service",
        "shell",
        Priority::Warning,
        Some(very(820)),
        "stdout",
        "sandbox: cluster 'web' requested /home/alex/Downloads — prompting user",
        &[
            ("CODE_FILE", "crates/session/shell/src/portal/access.rs"),
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
        &[
            ("CODE_FILE", "crates/session/shell/src/collab/mod.rs"),
            ("PEER", "mara"),
        ],
    ),
    (
        3650,
        "quick-fs.service",
        "quick-fs",
        Priority::Info,
        Some(very(641)),
        "stdout",
        "hydrated blob bafyb… (1.4MB) on first read of /var/workspace/notes/spec.md",
        &[
            ("CODE_FILE", "crates/system/fs/src/hydrate.rs"),
            ("SIZE", "1466368"),
        ],
    ),
    (
        3900,
        "sshd.service",
        "sshd",
        Priority::Notice,
        Some(very(1022)),
        "syslog",
        "Accepted publickey for alex from 10.42.0.5 port 51344 ssh2: ED25519",
        &[("SYSLOG_FACILITY", "4"), ("SSH_USER", "alex")],
    ),
    (
        4120,
        "sshd.service",
        "sshd",
        Priority::Warning,
        Some(very(1022)),
        "syslog",
        "Invalid user admin from 203.0.113.7 port 40122 — 3rd attempt this minute",
        &[("SYSLOG_FACILITY", "4"), ("SSH_REMOTE", "203.0.113.7")],
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
            ("CODE_FILE", "crates/session/shell/src/render/frame.rs"),
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
        &[("CODE_FILE", "crates/session/shell/src/render/frame.rs")],
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
            ("CODE_FILE", "crates/system/fs/src/conflict.rs"),
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
        &[("CODE_FILE", "crates/session/shell/src/focus.rs")],
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
            ("CODE_FILE", "crates/session/shell/src/collab/cursor.rs"),
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
        &[("CODE_FILE", "crates/system/fs/src/anti_entropy.rs")],
    ),
    (
        7900,
        "shell.service",
        "shell",
        Priority::Notice,
        Some(very(820)),
        "stdout",
        "workspace snapshot persisted: 3 clusters → ~/.local/state/tilo/wheel.json",
        &[("CODE_FILE", "crates/session/shell/src/persist.rs")],
    ),
];

/// A deliberately non-verbatim explanation of why the local journal could
/// not be read. `journalctl`'s stderr can contain host names, paths, and
/// policy details, so it must never be surfaced in the shared resource tree
/// or UI as an error string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalLoadError {
    /// `journalctl` was not executable in this sandbox or on this host.
    Unavailable,
    /// The journal command ran but the session was not authorised to read it.
    AccessDenied,
    /// The command failed for another local, recoverable reason.
    Failed,
}

impl JournalLoadError {
    /// Safe, localised-at-the-boundary text suitable for the UI.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "The system journal is not available on this device.",
            Self::AccessDenied => "This session is not allowed to read the system journal.",
            Self::Failed => "The system journal could not be read. Try again.",
        }
    }
}

/// One read of the journal: its records, oldest first, and the cursor of
/// the last one in the journal's order, where the next tail read starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub entries: Vec<LogEntry>,
    pub cursor: Option<String>,
}

/// `journalctl`'s arguments for `limit` records: the most recent ones, or,
/// `after` a cursor, the first ones written since it.
pub fn read_args(after: Option<&str>, limit: usize) -> Vec<String> {
    let mut args: Vec<String> = ["-o", "json", "--no-pager", "-n"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.push(limit.to_string());
    if let Some(cursor) = after {
        args.push(format!("--after-cursor={cursor}"));
    }
    args
}

/// Run `journalctl -o json` and parse the most recent `limit` records.
///
/// The error is intentionally classified rather than returning process
/// output: logs often contain sensitive machine context. An empty successful
/// result remains distinct from a failed read so the UI can offer recovery.
///
/// # Errors
///
/// Returns a safe classification when `journalctl` is unavailable, denied, or
/// otherwise fails. It never returns the command's stderr.
pub fn load_live(limit: usize) -> Result<Page, JournalLoadError> {
    read(None, limit)
}

/// The records written after `cursor`, at most `limit` of them: what live
/// tail reads each second, so a quiet journal costs an empty answer rather
/// than the whole page parsed again.
///
/// # Errors
///
/// As [`load_live`].
pub fn load_after(cursor: &str, limit: usize) -> Result<Page, JournalLoadError> {
    read(Some(cursor), limit)
}

fn read(after: Option<&str>, limit: usize) -> Result<Page, JournalLoadError> {
    let args = read_args(after, limit);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output_limit = limit
        .saturating_mul(64 * 1024)
        .clamp(1024 * 1024, 32 * 1024 * 1024);
    let output = bounded_command::run(
        "journalctl",
        &args,
        None,
        output_limit,
        Duration::from_secs(5),
    )
    .map_err(|error| match error {
        bounded_command::Error::Process(ref io_error)
            if io_error.kind() == std::io::ErrorKind::NotFound =>
        {
            JournalLoadError::Unavailable
        }
        _ => JournalLoadError::Failed,
    })?;
    if !output.status.success() {
        // Do not relay stderr. Its wording is neither stable nor safe to
        // publish, but the common permission failure deserves a useful
        // recovery state.
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        return Err(
            if stderr.contains("permission") || stderr.contains("access denied") {
                JournalLoadError::AccessDenied
            } else {
                JournalLoadError::Failed
            },
        );
    }
    Ok(parse_page(&String::from_utf8_lossy(&output.stdout)))
}

/// Export a pre-filtered local cache as JSON Lines.
///
/// The caller must obtain `path` from the host Save portal. It never follows
/// an app-owned default location, and returns only concise errors: exporting
/// logs is sensitive and should stay fully scoped to that portal choice.
pub fn export_json_lines(entries: &[&LogEntry], path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("export path must be absolute".into());
    }
    if path.as_os_str().is_empty() {
        return Err("export path is empty".into());
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|_| "could not write the selected export file".to_string())?;
    let mut writer = BufWriter::new(file);
    for entry in entries {
        serde_json::to_writer(&mut writer, entry)
            .map_err(|_| "could not encode journal entry for export".to_string())?;
        writer
            .write_all(b"\n")
            .map_err(|_| "could not write the requested export file".to_string())?;
    }
    writer
        .flush()
        .map_err(|_| "could not finish the requested export file".to_string())
}

/// Parse a newline-delimited `journalctl -o json` stream into entries,
/// sorted oldest → newest. Malformed lines are skipped, not fatal.
pub fn parse_json_stream(text: &str) -> Vec<LogEntry> {
    parse_page(text).entries
}

/// Parse a `journalctl -o json` stream into a [`Page`]: its entries sorted
/// oldest → newest, and the cursor of the last record as journalctl wrote
/// them.
pub fn parse_page(text: &str) -> Page {
    let mut cursor = None;
    let mut entries: Vec<LogEntry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| {
            if let Some(at) = field(&v, "__CURSOR") {
                cursor = Some(at);
            }
            entry_from_json(&v)
        })
        .collect();
    entries.sort_by_key(|e| e.realtime_us);
    Page { entries, cursor }
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

    let message = without_terminal_escapes(&field(v, "MESSAGE").unwrap_or_default());
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

/// `text` without the terminal's colour and cursor sequences a program
/// wrote for a console (`ESC [ … m`, `ESC ] … BEL`) and without other
/// control characters, which have no glyph: the words alone.
fn without_terminal_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // A control sequence ends at its final byte, `@` to `~`.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // An operating-system command ends at BEL or `ESC \`.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push(' '),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

// ===================================================================
// Bundled sample journal
// ===================================================================

/// A realistic offline journal: a Tilo boot sequence across the shell
/// compositor, the FUSE filesystem, the kernel, networking, audio, and a
/// collaboration session — seeded with a scatter of warnings and errors
/// so the severity colouring and filters have something to bite on.
pub fn sample_entries() -> Vec<LogEntry> {
    // The boot began a few minutes before now on the wall clock, which a
    // capture pins, so the sample reads as this morning's journal.
    let now_us = u64::try_from(ui::clock::local_now().timestamp_micros()).unwrap_or(0);
    let base: u64 = now_us.saturating_sub(4 * 60 * 1_000_000) / 1_000_000 * 1_000_000;
    let host = "tilo-station";
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
    for row in prev {
        entries.push(build(row, prev_base, host, boot_b));
    }

    // Rows are split across three consts purely to stay under the
    // per-function line-count limit; the boot sequence still reads as one
    // continuous timeline (offsets are relative to the same `base`).
    let rows = ROWS_BOOT.iter().chain(ROWS_SERVICES).chain(ROWS_SESSION);
    for row in rows {
        entries.push(build(row, base, host, boot_a));
    }

    entries.sort_by_key(|e| e.realtime_us);
    entries
}

/// Tiny helper so the sample table reads cleanly: most PIDs are stable.
const fn very(n: u32) -> u32 {
    n
}

/// The entry a sample `row` describes, logged on `hostname` during `boot_id`
/// at its offset from `base_us`.
fn build(row: &Row, base_us: u64, hostname: &str, boot_id: &str) -> LogEntry {
    let &(offset_ms, unit, identifier, priority, pid, transport, message, extra) = row;
    LogEntry {
        realtime_us: base_us + offset_ms * 1000,
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
    fn a_message_written_for_a_console_reads_as_its_words() {
        assert_eq!(
            without_terminal_escapes(
                "\u{1b}[2m2026-10-23T08:20:00Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m shell: closed"
            ),
            "2026-10-23T08:20:00Z ERROR shell: closed"
        );
        assert_eq!(
            without_terminal_escapes("\u{1b}]0;title\u{7}done\u{1b}]8;;x\u{1b}\\ok\u{8}"),
            "doneok"
        );
        assert_eq!(without_terminal_escapes("plain\ttext"), "plain text");
    }

    #[test]
    fn sample_has_mixed_severities_and_two_boots() {
        let e = sample_entries();
        assert!(e.len() > 20, "sample journal should be substantial");
        assert!(e.iter().any(|x| x.priority == Priority::Err));
        assert!(e.iter().any(|x| x.priority == Priority::Warning));
        assert!(e.iter().any(|x| x.priority == Priority::Crit));
        let boots: std::collections::HashSet<_> = e.iter().map(|x| x.boot_id.clone()).collect();
        assert_eq!(boots.len(), 2, "should span exactly two boots");
        assert!(e.windows(2).all(|w| w[0].realtime_us <= w[1].realtime_us));
    }

    #[test]
    fn parse_json_stream_reads_journalctl_shape_byte_messages_and_skips_malformed() {
        let line = r#"{"__REALTIME_TIMESTAMP":"1609459200000000","_SYSTEMD_UNIT":"x.service","PRIORITY":"3","MESSAGE":"boom","_PID":"42","_HOSTNAME":"h","_BOOT_ID":"b","_TRANSPORT":"stdout","CUSTOM":"keep"}"#;
        let parsed = parse_json_stream(line);
        assert_eq!(parsed.len(), 1);
        let e = &parsed[0];
        assert_eq!(e.unit, "x.service");
        assert_eq!(e.priority, Priority::Err);
        assert_eq!(e.message, "boom");
        assert_eq!(e.pid, Some(42));
        assert!(e.extra.iter().any(|(k, v)| k == "CUSTOM" && v == "keep"));

        // journald encodes non-UTF8 MESSAGE as a byte array.
        let line = r#"{"__REALTIME_TIMESTAMP":"1","MESSAGE":[104,105],"PRIORITY":"6"}"#;
        let parsed = parse_json_stream(line);
        assert_eq!(parsed[0].message, "hi");

        let stream = "not json\n{\"MESSAGE\":\"ok\",\"__REALTIME_TIMESTAMP\":\"5\"}\n";
        let parsed = parse_json_stream(stream);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].message, "ok");
    }

    /// A tail read starts after the newest record's cursor, which a page
    /// takes from the last record journalctl wrote.
    #[test]
    fn a_page_keeps_the_last_cursor_and_a_tail_read_starts_after_it() {
        let stream = concat!(
            r#"{"__CURSOR":"s=a;i=1","__REALTIME_TIMESTAMP":"20","MESSAGE":"one"}"#,
            "\n",
            r#"{"__CURSOR":"s=a;i=2","__REALTIME_TIMESTAMP":"10","MESSAGE":"two"}"#,
            "\n",
        );
        let page = parse_page(stream);
        assert_eq!(page.cursor.as_deref(), Some("s=a;i=2"), "journal order");
        assert_eq!(page.entries[0].message, "two", "entries by time");
        assert!(
            page.entries
                .iter()
                .all(|entry| { entry.extra.iter().all(|(key, _)| key != "__CURSOR") })
        );
        assert_eq!(parse_page("").cursor, None);

        assert_eq!(
            read_args(None, 2000),
            ["-o", "json", "--no-pager", "-n", "2000"]
        );
        assert_eq!(
            read_args(Some("s=a;i=2"), 2000),
            [
                "-o",
                "json",
                "--no-pager",
                "-n",
                "2000",
                "--after-cursor=s=a;i=2"
            ]
        );
    }

    #[test]
    fn export_writes_json_lines_only_to_absolute_portal_targets() {
        let entries = sample_entries();

        // Export requires an absolute portal target.
        assert_eq!(
            export_json_lines(&[&entries[0]], Path::new("logs.jsonl")).unwrap_err(),
            "export path must be absolute"
        );

        let path = std::env::temp_dir().join(format!(
            "tilo-logs-export-{}-{}.jsonl",
            std::process::id(),
            entries[0].realtime_us
        ));
        let _ = std::fs::remove_file(&path);
        export_json_lines(&[&entries[0], &entries[1]], &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("Journal stopped"));
        export_json_lines(&[&entries[0]], &path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        std::fs::remove_file(path).unwrap();

        // Export preserves a non-UTF-8 portal path.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let path = std::env::temp_dir().join(std::ffi::OsString::from_vec(vec![
                b'q', b'o', b's', b'-', 0xff, b'.', b'j', b's', b'o', b'n', b'l',
            ]));
            let _ = std::fs::remove_file(&path);
            export_json_lines(&[&entries[0]], &path).unwrap();
            assert!(path.is_file());
            std::fs::remove_file(path).unwrap();
        }
    }
}
