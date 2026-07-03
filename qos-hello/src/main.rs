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
