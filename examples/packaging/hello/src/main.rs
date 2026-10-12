#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use runtime::{collab, fs, identity, is_tilo_native, portal};

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

    println!("== xdg portals ==");
    // The launcher proxies the session bus in only for the portals the
    // manifest's `[sandbox.portals]` declares.
    println!(
        "  DBUS_SESSION_BUS_ADDRESS: {}",
        std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| "(unset)".into())
    );
    println!();

    println!("tilo-hello: done.");
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
