//! greetd IPC client — the authenticate-then-start exchange.
//!
//! greetd speaks a small request/response protocol over the Unix socket
//! named by `$GREETD_SOCK`. The greeter:
//!
//! 1. `CreateSession { username }`
//! 2. loops over `AuthMessage`s, answering each via
//!    `PostAuthMessageResponse { response }`:
//!      * `Secret`  → the password (hidden prompt, e.g. "Password:")
//!      * `Visible` → the username (shown prompt, some PAM stacks ask)
//!      * `Info` / `Error` → informational, answered with `None`
//! 3. on `Success`, `StartSession { cmd, env }` and (on the second
//!    `Success`) the session is launching — the greeter exits.
//! 4. on `Error`, `CancelSession` and report the description.
//!
//! The protocol loop is factored into [`run_exchange`], generic over any
//! `Read + Write` stream, so it can be exercised with an in-memory socket
//! pair in tests and — later — lifted into the UI for true multi-prompt
//! PAM (the loop already handles an arbitrary sequence of auth messages).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use greetd_ipc::codec::SyncCodec;
use greetd_ipc::{AuthMessageType, Request, Response};

/// Connect to greetd at `sock_path` and run the full
/// authenticate-and-start exchange. On success this **never returns** —
/// it `process::exit(0)`s so greetd takes over the VT. On failure it
/// returns a human-readable description for the status line.
pub fn authenticate_and_start(
    sock_path: &str,
    username: &str,
    password: &str,
    cmd: &[String],
    env: &[String],
) -> Result<(), String> {
    let mut stream = UnixStream::connect(sock_path)
        .map_err(|e| format!("cannot connect to greetd ({sock_path}): {e}"))?;

    run_exchange(&mut stream, username, password, cmd, env)?;

    // Session accepted — hand the VT to greetd. The greeter must exit so
    // greetd can reparent the started session.
    std::process::exit(0);
}

/// The greetd protocol loop, generic over the transport so it is unit
/// testable against an in-memory `UnixStream` pair. Returns `Ok(())` once
/// greetd has accepted `StartSession` (the caller then exits the process).
pub fn run_exchange<S: Read + Write>(
    stream: &mut S,
    username: &str,
    password: &str,
    cmd: &[String],
    env: &[String],
) -> Result<(), String> {
    Request::CreateSession {
        username: username.to_string(),
    }
    .write_to(stream)
    .map_err(|e| format!("CreateSession: {e}"))?;

    loop {
        let response = Response::read_from(stream).map_err(|e| format!("read response: {e}"))?;
        match response {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let reply = match auth_message_type {
                    AuthMessageType::Secret => Some(password.to_string()),
                    AuthMessageType::Visible => Some(username.to_string()),
                    AuthMessageType::Info | AuthMessageType::Error => {
                        // Informational only — surface for diagnostics and
                        // answer with no data so the PAM conversation
                        // advances.
                        tracing::info!(prompt = %auth_message, "greetd auth info");
                        None
                    }
                };
                Request::PostAuthMessageResponse { response: reply }
                    .write_to(stream)
                    .map_err(|e| format!("PostAuthMessageResponse: {e}"))?;
            }
            Response::Success => {
                // Authentication complete — request the session start.
                Request::StartSession {
                    cmd: cmd.to_vec(),
                    env: env.to_vec(),
                }
                .write_to(stream)
                .map_err(|e| format!("StartSession: {e}"))?;

                match Response::read_from(stream)
                    .map_err(|e| format!("read start response: {e}"))?
                {
                    Response::Success => return Ok(()),
                    Response::Error { description, .. } => return Err(description),
                    Response::AuthMessage { auth_message, .. } => {
                        return Err(format!(
                            "unexpected auth prompt after StartSession: {auth_message}"
                        ));
                    }
                }
            }
            Response::Error {
                error_type,
                description,
            } => {
                // Reset greetd's session slot so the user can retry without
                // a stuck half-open session.
                let _ = Request::CancelSession.write_to(stream);
                tracing::warn!(?error_type, %description, "greetd auth error");
                return Err(description);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// Drive a successful Secret-prompt exchange against a mock greetd
    /// running on the other end of a `UnixStream` pair. Proves the client
    /// answers the password and completes `StartSession`.
    #[test]
    fn successful_secret_exchange_completes() {
        let (client, mut server) = UnixStream::pair().unwrap();

        let server_thread = thread::spawn(move || {
            // 1. CreateSession
            let req = Request::read_from(&mut server).unwrap();
            assert!(matches!(req, Request::CreateSession { username } if username == "alice"));

            // 2. Ask for the secret.
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".to_string(),
            }
            .write_to(&mut server)
            .unwrap();

            // 3. Receive the password.
            let req = Request::read_from(&mut server).unwrap();
            assert!(
                matches!(req, Request::PostAuthMessageResponse { response } if response == Some("s3cr3t".to_string()))
            );

            // 4. Auth complete.
            Response::Success.write_to(&mut server).unwrap();

            // 5. StartSession with our argv.
            let req = Request::read_from(&mut server).unwrap();
            assert!(matches!(req, Request::StartSession { cmd, .. } if cmd == vec!["/usr/bin/shell".to_string(), "--backend".to_string(), "drm".to_string()]));

            // 6. Session accepted.
            Response::Success.write_to(&mut server).unwrap();
        });

        let mut client = client;
        let cmd = vec![
            "/usr/bin/shell".to_string(),
            "--backend".to_string(),
            "drm".to_string(),
        ];
        let result = run_exchange(&mut client, "alice", "s3cr3t", &cmd, &[]);
        server_thread.join().unwrap();
        assert_eq!(result, Ok(()));
    }

    /// A bad password yields `Response::Error`; the client cancels and
    /// returns the description.
    #[test]
    fn auth_error_is_reported_and_cancelled() {
        let (client, mut server) = UnixStream::pair().unwrap();

        let server_thread = thread::spawn(move || {
            let _ = Request::read_from(&mut server).unwrap(); // CreateSession
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".to_string(),
            }
            .write_to(&mut server)
            .unwrap();
            let _ = Request::read_from(&mut server).unwrap(); // PostAuthMessageResponse
            Response::Error {
                error_type: greetd_ipc::ErrorType::AuthError,
                description: "Login incorrect".to_string(),
            }
            .write_to(&mut server)
            .unwrap();
            // The client should send CancelSession to clean up.
            let req = Request::read_from(&mut server).unwrap();
            assert!(matches!(req, Request::CancelSession));
        });

        let mut client = client;
        let result = run_exchange(&mut client, "alice", "wrong", &[], &[]);
        server_thread.join().unwrap();
        assert_eq!(result, Err("Login incorrect".to_string()));
    }
}
