use qos_runtime::{collab, fs, identity, is_qos_native, portal};

fn main() {
    println!("qos-hello: a qOS-native app, running.");
    println!();

    println!("== environment ==");
    println!("  is_qos_native      : {}", is_qos_native());
    println!(
        "  QOS_WORKSPACE_ID   : {}",
        std::env::var("QOS_WORKSPACE_ID").unwrap_or_else(|_| "(unset)".into())
    );
    println!(
        "  QOS_USER_IDENTITY  : {}",
        std::env::var("QOS_USER_IDENTITY").unwrap_or_else(|_| "(unset)".into())
    );
    println!();

    println!("== services ==");
    report("collab   ", collab::try_connect().map(|_| ()));
    report("fs       ", fs::try_connect().map(|_| ()));
    report("identity ", identity::load().map(|_| ()));
    println!();

    println!("== fds ==");
    for name in ["collab", "fs", "spellcheck"] {
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
    for path in ["/tmp", "/qos-launcher"] {
        match std::fs::metadata(path) {
            Ok(_) => println!("  stat({path:>25}): OK"),
            Err(e) => println!("  stat({path:>25}): {} — unexpected!", e.kind()),
        }
    }
    // Writing to /tmp should succeed (Landlock rw).
    match std::fs::write("/tmp/qos-hello-probe", b"hello") {
        Ok(()) => println!("  write(/tmp/qos-hello-probe): OK"),
        Err(e) => println!("  write(/tmp/qos-hello-probe): {} — unexpected!", e.kind()),
    }
    // Writing to a Landlock-denied location should fail. /var/qos-apps
    // is bind-mounted r/o by bwrap so this would also fail without
    // Landlock — but Landlock adds defence-in-depth.
    match std::fs::write("/var/qos-apps/probe", b"hello") {
        Ok(()) => println!("  write(/var/qos-apps/probe): OK — unexpected!"),
        Err(e) => println!("  write(/var/qos-apps/probe): {} — blocked (bwrap or Landlock)", e.kind()),
    }
    println!();

    println!("qos-hello: done.");
}

fn report(name: &str, result: Result<(), qos_runtime::NotAvailable>) {
    match result {
        Ok(()) => println!("  {name}: connected"),
        Err(qos_runtime::NotAvailable::ServiceFdMissing) => println!("  {name}: unavailable (no fd — graceful)"),
        Err(qos_runtime::NotAvailable::ConnectionFailed(e)) => println!("  {name}: unavailable (connect failed: {e})"),
        Err(qos_runtime::NotAvailable::ProtocolMismatch { expected, got }) => {
            println!("  {name}: protocol mismatch (want v{expected}, got v{got})")
        }
        Err(other) => println!("  {name}: unavailable ({other:?})"),
    }
}
