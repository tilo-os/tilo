#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use runtime_collab as collab;
use runtime_fs as fs;
use runtime_identity as identity;
use runtime_portal::{self as portal, NotAvailable, is_tilo_native};

// Thin build only: pull the shared runtime SDK dylib
// (`libruntime_dylib.so`) into the link graph. Without this reference
// rustc's unused-crate elimination drops the wrapper, and
// `-C prefer-dynamic` then satisfies the runtime crates from their static rlibs
// instead of the dylib. Naming it here forces the dylib to be linked,
// so the runtime crates (and their whole graph) are served dynamically and the
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
            let hex = hex::encode(id.metadata().group_public_key());
            println!("  identity : connected (pubkey={hex})");
        }
        Err(NotAvailable::ServiceFdMissing) => {
            println!("  identity : unavailable (env var unset — graceful)");
        }
        Err(NotAvailable::ConnectionFailed(e)) => {
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

fn report(name: &str, result: Result<(), NotAvailable>) {
    match result {
        Ok(()) => println!("  {name}: connected"),
        Err(NotAvailable::ServiceFdMissing) => {
            println!("  {name}: unavailable (no fd — graceful)");
        }
        Err(NotAvailable::ConnectionFailed(e)) => {
            println!("  {name}: unavailable (connect failed: {e})");
        }
        Err(NotAvailable::ProtocolMismatch { expected, got }) => {
            println!("  {name}: protocol mismatch (want v{expected}, got v{got})");
        }
        Err(other) => println!("  {name}: unavailable ({other:?})"),
    }
}
