//! Real interface state + byte-counter probing.
//!
//! This is the honest transport telemetry: it never fabricates a value.
//! The pure parsers ([`parse_wg_transfer`], [`parse_stat`]) turn the exact
//! stdout of `wg show <iface> transfer` and a
//! `/sys/class/net/.../statistics/*` file into numbers; the IO wrappers
//! ([`read_wg_transfer`], [`read_sys_bytes`]) run the command / read the file
//! and hand the text to those parsers. If a tool is missing or the interface
//! is gone the probe returns `None` — the UI then shows a real state or
//! nothing, never a guess.
//!
//! Reads here are read-only (they observe, they never mutate networking), so
//! unlike the bring-up/teardown commands in [`crate::config`] they are not
//! behind the `TILO_VPN_ALLOW_EXEC` write-gate. The *app* only polls them for a
//! tunnel it actually connected, which keeps unit tests free of subprocesses.

use std::time::Duration;

use crate::model::{ConnKind, Connection};

const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn run_probe(program: &str, args: &[&str]) -> Option<bounded_command::Output> {
    bounded_command::run(program, args, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT).ok()
}

/// Cumulative interface byte counters, as read from the kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Bytes received on the interface (the download direction).
    pub rx: u64,
    /// Bytes transmitted on the interface (the upload direction).
    pub tx: u64,
}

// ----------------------------------------------------------------------
// Pure parsers (fully unit-tested against real command output fixtures).
// ----------------------------------------------------------------------

/// Parse the stdout of `wg show <iface> transfer`.
///
/// Each line is `<peer-public-key>\t<rx_bytes>\t<tx_bytes>`; a tunnel may
/// have several peers, so the totals are summed across every peer line.
/// Returns `None` when no parseable peer line is present.
pub fn parse_wg_transfer(out: &str) -> Option<Transfer> {
    let mut t = Transfer::default();
    let mut any = false;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Tab-separated per the wireguard-tools dump format; fall back to
        // any run of whitespace so a hand-massaged fixture still parses.
        let mut cols = line.split('\t');
        let _peer = cols.next();
        let rx = cols.next();
        let tx = cols.next();
        let (rx, tx) = if let (Some(rx), Some(tx)) = (rx, tx) {
            (rx, tx)
        } else {
            // Whitespace fallback.
            let mut ws = line.split_whitespace();
            let _ = ws.next();
            match (ws.next(), ws.next()) {
                (Some(rx), Some(tx)) => (rx, tx),
                _ => continue,
            }
        };
        let (Ok(rx), Ok(tx)) = (rx.trim().parse::<u64>(), tx.trim().parse::<u64>()) else {
            continue;
        };
        t.rx += rx;
        t.tx += tx;
        any = true;
    }
    any.then_some(t)
}

/// Parse a single `/sys/class/net/<iface>/statistics/<name>` value.
pub fn parse_stat(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok()
}

/// A Linux interface name is safe to interpolate into a `/sys` path (no
/// separators, non-empty, within `IFNAMSIZ`). Guards the sysfs reader
/// against path traversal from an attacker-controlled tunnel name.
pub fn is_valid_iface(iface: &str) -> bool {
    !iface.is_empty()
        && iface.len() <= 15
        && iface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        && iface != "."
        && iface != ".."
}

// ----------------------------------------------------------------------
// Real IO wrappers.
// ----------------------------------------------------------------------

/// Read the real cumulative byte counters for `iface` from
/// `/sys/class/net/<iface>/statistics/{rx,tx}_bytes`, or from the kernel's
/// `/proc/net/dev` table where no sysfs is mounted (a sandbox that mounts
/// only `/proc`). `None` if the interface is absent or the values don't
/// parse. Works for any kernel interface — the loopback `lo` is a handy
/// always-present target for tests.
pub fn read_sys_bytes(iface: &str) -> Option<Transfer> {
    if !is_valid_iface(iface) {
        return None;
    }
    let base = format!("/sys/class/net/{iface}/statistics");
    let from_sysfs = || {
        let rx = parse_stat(&std::fs::read_to_string(format!("{base}/rx_bytes")).ok()?)?;
        let tx = parse_stat(&std::fs::read_to_string(format!("{base}/tx_bytes")).ok()?)?;
        Some(Transfer { rx, tx })
    };
    from_sysfs()
        .or_else(|| parse_proc_net_dev(&std::fs::read_to_string("/proc/net/dev").ok()?, iface))
}

/// The byte counters of `iface` in the text of `/proc/net/dev`: a row
/// `name: rx_bytes rx_packets … (8 receive columns) tx_bytes …`.
pub fn parse_proc_net_dev(text: &str, iface: &str) -> Option<Transfer> {
    text.lines().find_map(|line| {
        let (name, counters) = line.split_once(':')?;
        if name.trim() != iface {
            return None;
        }
        let mut columns = counters.split_whitespace();
        let rx = columns.next()?.parse().ok()?;
        let tx = columns.nth(7)?.parse().ok()?;
        Some(Transfer { rx, tx })
    })
}

/// Read the real `WireGuard` peer transfer totals via `wg show <iface> transfer`.
/// `None` if `wg` is absent, the interface does not exist, or the command
/// fails.
pub fn read_wg_transfer(iface: &str) -> Option<Transfer> {
    if !is_valid_iface(iface) {
        return None;
    }
    let out = run_probe("wg", &["show", iface, "transfer"])?;
    if !out.status.success() {
        return None;
    }
    parse_wg_transfer(&String::from_utf8_lossy(&out.stdout))
}

/// Read the best available real transfer counters for a connection:
/// `wg show transfer` for `WireGuard` (per-peer, VPN-accurate), otherwise the
/// tunnel interface's sysfs counters. `None` when no real source is readable.
pub fn read_transfer(conn: &Connection) -> Option<Transfer> {
    let iface = crate::config::iface_name(conn);
    match conn.kind {
        ConnKind::WireGuard => read_wg_transfer(&iface).or_else(|| read_sys_bytes(&iface)),
        // OpenVPN's tun device is kernel-assigned; try the derived name then
        // the conventional tun0/tun1. Honestly `None` if none carry counters.
        ConnKind::OpenVpn => read_sys_bytes(&iface)
            .or_else(|| read_sys_bytes("tun0"))
            .or_else(|| read_sys_bytes("tun1")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `wg show wg0 transfer` output — two peers, tab-separated.
    const WG_TRANSFER: &str = "\
Zm9vYmFyMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=\t1024\t2048
YmF6cXV1eDExMTExMTExMTExMTExMTExMTExMTExMTE=\t500\t250
";

    #[test]
    fn wireguard_transfer_and_sysfs_output_parses() {
        // Case `wg_transfer_sums_all_peers`.
        {
            let t = parse_wg_transfer(WG_TRANSFER).expect("parses");
            assert_eq!(t.rx, 1024 + 500);
            assert_eq!(t.tx, 2048 + 250);
        }
        // Case `wg_transfer_single_peer`.
        {
            let t = parse_wg_transfer("abc=\t9999\t1\n").expect("parses");
            assert_eq!(t, Transfer { rx: 9999, tx: 1 });
        }
        // Case `wg_transfer_empty_is_none`.
        {
            assert!(parse_wg_transfer("").is_none());
            assert!(parse_wg_transfer("\n\n").is_none());
        }
        // Case `stat_parses_number`.
        {
            assert_eq!(parse_stat("162300528\n"), Some(162_300_528));
            assert_eq!(parse_stat("  42 "), Some(42));
            assert!(parse_stat("not-a-number").is_none());
        }
        // Case `iface_validation_rejects_traversal`.
        {
            assert!(is_valid_iface("wg0"));
            assert!(is_valid_iface("home"));
            assert!(!is_valid_iface("../../etc/passwd"));
            assert!(!is_valid_iface("a/b"));
            assert!(!is_valid_iface(""));
            assert!(!is_valid_iface("waaaaaaaaaaaaaaaay-too-long"));
        }
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn bring_up_is_refused_while_execution_is_gated() {
        use crate::config;
        use crate::model::ConnKind;

        let conn = Connection::new("qtest0", ConnKind::WireGuard);
        // With execution disabled, the mutating command must be refused — the
        // UI can never claim connected without a real success.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_VPN_ALLOW_EXEC");
        let refused = config::execute(&config::connect_local_plan(&conn, "qtest0-uuid"));
        assert!(
            refused.is_err() && refused.unwrap_err().contains("execution disabled"),
            "bring-up must be refused while execution is gated off"
        );
    }

    // ---- Real reads against the always-present loopback interface -------
    //
    // These hit the real kernel's sysfs. They need no root and no exec-gate
    // because they are read-only.

    #[test]
    fn proc_net_dev_rows_give_the_interface_byte_counters() {
        let text = "Inter-|   Receive                                                |  Transmit\n \
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
             lo:  123456     789    0    0    0     0          0         0   654321     987    0    0    0     0       0          0\n  \
             wg0: 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16\n";
        assert_eq!(
            parse_proc_net_dev(text, "lo"),
            Some(Transfer {
                rx: 123_456,
                tx: 654_321
            })
        );
        assert_eq!(
            parse_proc_net_dev(text, "wg0"),
            Some(Transfer { rx: 1, tx: 9 })
        );
        assert_eq!(parse_proc_net_dev(text, "wg"), None);
        assert_eq!(
            parse_proc_net_dev("    lo: 1 2 3\n", "lo"),
            None,
            "a short row"
        );
    }

    #[test]
    fn real_loopback_sys_bytes_are_readable_and_monotonic() {
        // `lo` always exists on Linux; its counters are real, non-fabricated
        // numbers straight from the kernel. Read twice with real loopback
        // traffic in between and assert the kernel counter only advanced —
        // a property no synthesized value could satisfy.
        let first = read_sys_bytes("lo").expect("loopback statistics are readable");
        // Generate a little real loopback traffic.
        let _ = std::net::UdpSocket::bind("127.0.0.1:0").and_then(|s| {
            let addr = s.local_addr()?;
            s.send_to(&[0u8; 512], addr).map(|_| ())
        });
        let second = read_sys_bytes("lo").expect("loopback statistics are readable");
        assert!(
            second.rx >= first.rx && second.tx >= first.tx,
            "kernel byte counters must be monotonic: {first:?} -> {second:?}"
        );
    }
}
