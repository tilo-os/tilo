//! `tilo mcp`: the agent's MCP server over stdio. On a Tilo desktop it is
//! the desktop's own MCP server (`agent.sock`), which the person admits the
//! agent at; elsewhere it is a connection to this computer's one daemon
//! (`tilo-runtime`), which serves as the device `tilo login` made of
//! this computer.
//!
//! What the daemon serves is the runtime's own projection of a workspace
//! ([`tilo_mcp::McpServer`], the agent surface: `tilo_ls`, `tilo_read`,
//! `tilo_do`, `tilo_find`, `tilo_status`), the same tools an agent gets on a
//! Tilo desktop. The host keeps one session per device, and a second session
//! of the same device cuts the first off: every agent on this computer runs
//! its own `tilo mcp` (each Claude Code window, `tilo agent check`), and each
//! one relays its stdio to the daemon ([`held`]), which holds the device's one
//! session for all of them and serves each connection as its own agent run.
//!
//! `tilo mcp --as-me` serves the same tools as the person, for a program of theirs that is no
//! agent ([`person`]).

pub(crate) mod held;
pub(crate) mod person;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tilo_api::daemon::{this_computer, unreachable_answer};

use crate::login::NOT_LOGGED_IN;

/// `tilo mcp`: on a Tilo desktop, the desktop's own MCP server, which asks the person to let the
/// agent in; elsewhere, a connection to this computer's daemon, which enters the person's
/// workspace wherever it is hosted ([`held`]), its service started when none answers.
pub fn serve() -> Result<ExitCode> {
    let desktop = tilo_mcp::session_api::desktop_server();
    if let Ok(Some(server)) = &desktop
        && tilo_mcp::session_api::answers(server)
    {
        tilo_mcp::session_api::bridge(server, tilo_mcp::session_api::session_name())?;
        return Ok(ExitCode::SUCCESS);
    }
    // No desktop answers here: one that ended leaves its socket behind, and one whose first login
    // is not complete has none yet. This computer's own login then reaches its daemon, as on any
    // other computer; without one the agent hears what it waits for.
    let login = crate::login::Place::here(crate::identity::paths())?;
    match (crate::login::served_device_key(login.keys()), desktop) {
        (Some(_), _) => {}
        (None, Ok(Some(server))) => {
            tilo_mcp::session_api::bridge(&server, tilo_mcp::session_api::session_name())?;
            return Ok(ExitCode::SUCCESS);
        }
        (None, Ok(None)) => return held::alone(NOT_LOGGED_IN),
        (None, Err(error)) => return held::alone(&error.to_string()),
    }
    let place = this_computer();
    let reached = place
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|place| held::reach(place).map(|stream| (stream, place.clone())));
    match reached {
        Ok((stream, place)) => held::relay(stream, &place),
        Err(why) => {
            // This connection starts no session of its own. The device has one live connection,
            // its daemon's: a session here would bind the device's endpoint too, take the
            // daemon's place at the server and then be taken by the daemon's, so neither serves
            // and every call fails with "another endpoint connected with the same endpoint id".
            // The daemon is started again in the background; every call says why until it is.
            eprintln!(
                "tilo mcp: {why} This connection waits for the device's daemon, which is started \
                 again in the background; it holds no session of its own."
            );
            if let Ok(place) = place {
                std::thread::spawn(move || {
                    loop {
                        let _ = held::ensure(&place);
                        if place.connect().is_some() {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(3));
                    }
                });
            }
            held::alone(&not_in(&why))
        }
    }
}

/// A connection to this computer's daemon's native endpoint, which the workspace commands acting
/// as the device call: its service started when none answers, so no command opens a session of
/// its own, which would cut the daemon's off.
///
/// # Errors
/// The daemon's sockets are not this account's, or no daemon answered in time.
pub(crate) fn reach_daemon() -> Result<std::os::unix::net::UnixStream, String> {
    held::reach_native(&this_computer()?)
}

/// Why a call answers nothing while this computer is not in the workspace: the reason, and that
/// the background sync keeps trying, so the agent asks again rather than dropping its tools.
fn not_in(why: &str) -> String {
    format!(
        "This computer isn't in the workspace right now ({why}). Tilo keeps trying in the \
         background; try again in a minute."
    )
}

/// The answer to one JSON-RPC line while the workspace cannot be reached, for `problem`, or
/// `None` for a notification.
fn answer_alone(line: &str, problem: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": error.to_string()}})
                    .to_string(),
            );
        }
    };
    Some(unreachable_answer(&request, problem)?.to_string())
}

/// How long a status read waits for the daemon, when a command reads one rather than checking
/// it: an answer comes in milliseconds, and a command that only reads waits no longer.
const PEER_ANSWERING: Duration = Duration::from_secs(5);

/// Ask this computer's daemon for its [`tilo_api::daemon::PEER_STATUS`]; `None` when none runs
/// (it is not started for asking).
pub(crate) fn ask_peer() -> Option<Value> {
    let login = crate::login::Place::here(crate::identity::paths()).ok()?;
    crate::login::served_device_key(login.keys())?;
    this_computer().ok()?.status(PEER_ANSWERING)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(problem: &str, line: &str) -> Option<Value> {
        answer_alone(line, problem).map(|answer| serde_json::from_str(&answer).unwrap())
    }

    /// A connection that reached no daemon holds no session of its own — the device has one
    /// live connection — and every call answers why while the daemon is started again.
    #[test]
    fn a_connection_that_reached_no_daemon_says_why() {
        let called = ask(
            &not_in("Tilo's background sync isn't answering on this computer."),
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tilo_ls"}}"#,
        )
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
        let said = called["result"]["content"][0]["text"].as_str().unwrap();
        assert!(said.contains("keeps trying in the background"), "{said}");
    }

    #[test]
    fn without_a_login_the_server_says_how_to_log_in_and_offers_nothing() {
        let init = ask(
            NOT_LOGGED_IN,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tilo");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("tilo login")
        );
        assert!(
            ask(
                NOT_LOGGED_IN,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
        let listed = ask(
            NOT_LOGGED_IN,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .unwrap();
        assert_eq!(listed["result"]["tools"], json!([]));
        let called = ask(
            NOT_LOGGED_IN,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_ls"}}"#,
        )
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
    }

    /// A Tilo desktop whose server is not open yet (its first login is not complete) is no computer
    /// that needs `tilo login`: the agent hears what it waits for, never "not logged in".
    #[test]
    fn a_desktop_whose_server_is_not_open_yet_never_says_not_logged_in() {
        let waiting = tilo_mcp::session_api::route(
            false,
            None,
            true,
            Some("/run/user/1000/tilo-session/agent.sock".into()),
            |_| false,
        )
        .unwrap_err()
        .to_string();
        let init = ask(
            &waiting,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        let said = init["result"]["instructions"].as_str().unwrap();
        assert_eq!(said, waiting);
        assert!(!said.contains("not logged in"), "{said}");
        assert!(said.contains("first login"), "{said}");
    }
}
