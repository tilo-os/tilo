//! The front door on loopback, with a person's host and runtime behind it,
//! for testing clients of the door (the SDKs, the CLI) against the real
//! thing ([`tilo_front_door::local`]).
//!
//! It prints one JSON line, `{"door": "http://127.0.0.1:<port>", "key":
//! "tilo_sk_…", "person": "<root hex>"}`, and serves until its standard input
//! closes:
//!
//! ```sh
//! t cargo build -p tilo-front-door --features local --bin tilo-door-local
//! ```

use std::io::{Read as _, Write as _};
use std::process::ExitCode;
use tilo_front_door::local::Local;

#[expect(
    clippy::significant_drop_tightening,
    reason = "the door serves until its input closes; `shutdown` consumes it then"
)]
fn main() -> ExitCode {
    // The host keeps workspaces in automerge documents, which run in worker
    // processes this same binary serves.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("tilo-door-local: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let local = match Local::start("Local key").await {
            Ok(local) => local,
            Err(error) => {
                eprintln!("tilo-door-local: {error}");
                return ExitCode::FAILURE;
            }
        };
        let line =
            serde_json::json!({"door": local.door, "key": local.key, "person": local.person});
        let printed = {
            let mut stdout = std::io::stdout();
            writeln!(stdout, "{line}").and_then(|()| stdout.flush())
        };
        if printed.is_err() {
            return ExitCode::FAILURE;
        }
        // Serve until whoever started it closes its input.
        let _ = tokio::task::spawn_blocking(|| {
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
        })
        .await;
        local.shutdown().await;
        ExitCode::SUCCESS
    })
}
