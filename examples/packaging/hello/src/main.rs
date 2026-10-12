#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use runtime::{collab, fs, identity, is_tilo_native, portal, portal_xdg};

// Thin build only: pull the shared runtime SDK dylib
// (`libruntime_dylib.so`) into the link graph. Without this reference
// rustc's unused-crate elimination drops the wrapper, and
// `-C prefer-dynamic` then satisfies `runtime` from its static rlib
// instead of the dylib. Naming it here forces the dylib to be linked,
// so `runtime` (and its whole graph) is served dynamically and the
// `tilo-hello` binary keeps only its own code. Compiled out entirely in
// normal (static) builds.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    println!("tilo-hello: a Tilo-native app, running.");
    println!();

    println!("== environment ==");
    println!("  is_tilo_native      : {}", is_tilo_native());
    println!(
        "  TILO_WORKSPACE_ID   : {}",
        std::env::var("TILO_WORKSPACE_ID").unwrap_or_else(|_| "(unset)".into())
    );
    println!(
        "  TILO identity broker: {}",
        std::env::var("TILO_IDENTITY_BROKER_FD").unwrap_or_else(|_| "(unset)".into())
    );
    println!();

    println!("== services ==");
    report("collab   ", collab::try_connect().map(|_| ()));
    report("fs       ", fs::try_connect().map(|_| ()));
    // Identity: split from `report` because we want to surface the
    // loaded pubkey hex on success — that's the number a maintainer
    // greps their broker logs for when confirming "yes, that
    // presence event came from this device". Without it, the probe
    // told you the identity file loaded but never proved *which*
    // identity.
    match identity::load() {
        Ok(id) => {
            let hex = hex_encode(&id.group_public_key());
            println!("  identity : connected (pubkey={hex})");
        }
        Err(runtime::NotAvailable::ServiceFdMissing) => {
            println!("  identity : unavailable (env var unset — graceful)");
        }
        Err(runtime::NotAvailable::ConnectionFailed(e)) => {
            println!("  identity : unavailable (load failed: {e})");
        }
        Err(other) => println!("  identity : unavailable ({other:?})"),
    }
    println!();

    println!("== fds ==");
    for name in ["collab", "fs"] {
        match portal::service_fd(name) {
            Some(fd) => println!("  {name:9}: fd={fd} (present)"),
            None => println!("  {name:9}: absent"),
        }
    }
    println!();

    println!("== sandbox probes ==");
    // Paths that bwrap bind-mounts INTO the sandbox but Landlock does NOT
    // grant. If Landlock is enforcing, opening these fails EACCES/EPERM.
    // If Landlock is inactive, these succeed (bwrap alone can't stop them).
    for path in ["/proc/self/status", "/etc/passwd"] {
        // Bounded read — some /proc entries never EOF.
        let mut buf = [0u8; 128];
        match std::fs::File::open(path).and_then(|mut f| {
            use std::io::Read;
            f.read(&mut buf)
        }) {
            Ok(n) => println!("  read({path:>25}): OK ({n} bytes)"),
            Err(e) => println!(
                "  read({path:>25}): {} — blocked (bwrap or Landlock)",
                e.kind()
            ),
        }
    }
    // Paths Landlock DOES grant — should succeed.
    for path in ["/tmp", "/tilo-launcher"] {
        match std::fs::metadata(path) {
            Ok(_) => println!("  stat({path:>25}): OK"),
            Err(e) => println!("  stat({path:>25}): {} — unexpected!", e.kind()),
        }
    }
    // Writing to /tmp should succeed (Landlock rw).
    match std::fs::write("/tmp/tilo-hello-probe", b"hello") {
        Ok(()) => println!("  write(/tmp/tilo-hello-probe): OK"),
        Err(e) => println!("  write(/tmp/tilo-hello-probe): {} — unexpected!", e.kind()),
    }
    // Writing to a Landlock-denied location should fail. /var/tilo-apps
    // is bind-mounted r/o by bwrap so this would also fail without
    // Landlock — but Landlock adds defence-in-depth.
    match std::fs::write("/var/tilo-apps/probe", b"hello") {
        Ok(()) => println!("  write(/var/tilo-apps/probe): OK — unexpected!"),
        Err(e) => println!(
            "  write(/var/tilo-apps/probe): {} — blocked (bwrap or Landlock)",
            e.kind()
        ),
    }
    println!();

    println!("== xdg portal probes ==");
    // Cheap presence check first. If the bus isn't there, all the
    // portal calls will surface `NotAvailable` — the whole point of
    // the graceful-degradation contract.
    println!(
        "  is_portal_available: {}",
        portal_xdg::is_portal_available()
    );
    println!(
        "  DBUS_SESSION_BUS_ADDRESS: {}",
        std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| "(unset)".into())
    );
    // Notification: fire-and-forget — the portal returns as soon as
    // the bubble is enqueued, so we can call it inline.
    report_portal(
        "notifications",
        portal_xdg::Notification::send("tilo-hello reporting in"),
    );
    // Secret: this may prompt the user, so it can block on the portal
    // frontend. Wrap in a short timeout so this probe never hangs the
    // demo — a "timed-out" line still proves the bus is reachable.
    report_portal(
        "secret",
        with_timeout(std::time::Duration::from_millis(750), || {
            portal_xdg::Secret::retrieve().map(|_| ())
        }),
    );
    println!();

    println!("tilo-hello: done.");
}

/// Run a fallible closure on a worker thread with a deadline. If it
/// finishes in time we return its result; otherwise we return
/// `Cancelled` (the closest existing `PortalError` variant meaning
/// "no answer from the portal") — the worker thread is left running,
/// which is fine for a short-lived probe.
fn with_timeout<F>(dur: std::time::Duration, f: F) -> Result<(), portal_xdg::PortalError>
where
    F: FnOnce() -> Result<(), portal_xdg::PortalError> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(dur) {
        Ok(r) => r,
        Err(_) => Err(portal_xdg::PortalError::Cancelled),
    }
}

fn report_portal(name: &str, result: Result<(), portal_xdg::PortalError>) {
    match result {
        Ok(()) => println!("  {name:14}: OK"),
        Err(portal_xdg::PortalError::NotAvailable) => {
            println!("  {name:14}: unavailable — graceful (no bus / launcher didn't plumb)");
        }
        Err(portal_xdg::PortalError::PortalNotFound) => {
            println!("  {name:14}: bus reachable but no portal frontend installed");
        }
        Err(portal_xdg::PortalError::Cancelled) => {
            println!(
                "  {name:14}: no answer within timeout (portal likely blocking on user consent)"
            );
        }
        Err(other) => println!("  {name:14}: error ({other})"),
    }
}

/// Render a byte slice as lowercase hex. Used to print the loaded
/// Ed25519 pubkey (32 bytes → 64 hex chars) — a maintainer grepping
/// broker logs for a specific device's presence events wants the raw
/// hex, not the base64 the wire uses.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // Both indices are nibbles (0..=15) so they're always in bounds;
        // `.get()` + a fallback avoids `indexing_slicing` without changing behaviour.
        out.push(HEX.get((b >> 4) as usize).copied().unwrap_or(b'?') as char);
        out.push(HEX.get((b & 0xF) as usize).copied().unwrap_or(b'?') as char);
    }
    out
}

fn report(name: &str, result: Result<(), runtime::NotAvailable>) {
    match result {
        Ok(()) => println!("  {name}: connected"),
        Err(runtime::NotAvailable::ServiceFdMissing) => {
            println!("  {name}: unavailable (no fd — graceful)");
        }
        Err(runtime::NotAvailable::ConnectionFailed(e)) => {
            println!("  {name}: unavailable (connect failed: {e})");
        }
        Err(runtime::NotAvailable::ProtocolMismatch { expected, got }) => {
            println!("  {name}: protocol mismatch (want v{expected}, got v{got})");
        }
        Err(other) => println!("  {name}: unavailable ({other:?})"),
    }
}
