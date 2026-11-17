//! A served run: a program started in a machine whose requests its host
//! answers.
//!
//! Some programs need things only their host may hold: a model a person pays
//! for with their own key, a workspace's actions, a sign-in. Such a program is
//! started through the agent program's own mode [`MODE`]
//! (`tilo-legacy-agent serve-run --endpoint NAME… -- PROGRAM ARGS…`), which
//! runs as the guest's ordinary user like any launched program. The launcher
//! opens one loopback address per endpoint, hands it to the program in place
//! of each [`placeholder`] in its arguments and variables, and carries every
//! request made there to the host over its own standard output, as frames;
//! the host's answers come back on its standard input:
//!
//! ```text
//! run  → Ready { version, endpoints }
//! run  → Started { pid }
//! run  → Out(..) | Err(..)                   what the program prints
//! run  → Asked(..), AskedBody { last }*      a request at an endpoint
//! host → Answer { status, headers }, AnswerBody { last }*
//! run  → AskedGone { id }                    the asker left unanswered
//! run  → Turned { endpoint }                 a caller that is not of the run
//! host → In(..) | CloseIn                    the program's input
//! host → Stop                                ask it to end, then end it
//! run  → Exited(ExitStatus)
//! ```
//!
//! An endpoint answers only the run's own processes: the launcher finds each
//! caller's socket among the processes it started and turns everything else
//! away without a byte. The channel is the run's and ends with it; a host
//! that goes away ends the program. No secret travels on it: what a request
//! carries to sign itself in is dropped before it leaves the machine, and
//! the host adds what it holds where the request leaves the host.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ExitStatus;

/// The frames' version, said in [`FromRun::Ready`].
pub const VERSION: u16 = 1;

/// The agent program's mode that serves a run.
pub const MODE: &str = "serve-run";

/// Where a hosted guest keeps the agent program.
pub const AGENT_PROGRAM: &str = "/usr/libexec/tilo/tilo-legacy-agent";

/// Names an endpoint to open; repeated for several.
pub const ENDPOINT_FLAG: &str = "--endpoint";

/// How long the program is given to end once asked, in milliseconds.
pub const STOP_GRACE_FLAG: &str = "--stop-grace-ms";

/// The most of a request's or an answer's body one frame carries.
pub const MAX_BODY_CHUNK: usize = 60 * 1024;

/// The most endpoints one run opens.
pub const MAX_ENDPOINTS: usize = 8;

/// How long a program asked to end is given by default.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// What stands for `endpoint`'s address in the program's arguments and in
/// the values of its variables: the launcher writes `http://127.0.0.1:PORT`
/// in its place.
#[must_use]
pub fn placeholder(endpoint: &str) -> String {
    format!("{{endpoint:{endpoint}}}")
}

/// Whether `name` may name an endpoint: a short lower-case word.
#[must_use]
pub fn endpoint_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The agent program's arguments that serve `argv` with `endpoints`, asked
/// to end within `stop_grace`: what follows the program's own path.
#[must_use]
pub fn arguments(endpoints: &[&str], stop_grace: Duration, argv: &[String]) -> Vec<String> {
    let mut arguments = vec![MODE.to_owned()];
    for endpoint in endpoints {
        arguments.push(ENDPOINT_FLAG.to_owned());
        arguments.push((*endpoint).to_owned());
    }
    arguments.push(STOP_GRACE_FLAG.to_owned());
    arguments.push(stop_grace.as_millis().to_string());
    arguments.push("--".to_owned());
    arguments.extend(argv.iter().cloned());
    arguments
}

/// One request made at an endpoint by the run's own processes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// The launcher's number for it: its body and its answer carry it.
    pub id: u64,
    pub endpoint: String,
    pub method: String,
    /// The request's target as written: a path, with its query.
    pub path: String,
    /// Its headers, names in lower case, without the ones that sign it in
    /// and the ones that only frame the connection.
    pub headers: Vec<(String, String)>,
}

/// What a served run tells its host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromRun {
    /// The launcher is up and serves these endpoints.
    Ready {
        version: u16,
        endpoints: Vec<String>,
    },
    /// The program was started.
    Started {
        pid: u32,
    },
    /// What the program wrote to its standard output.
    Out(Vec<u8>),
    /// What the program wrote to its standard error.
    Err(Vec<u8>),
    Asked(Asked),
    /// More of a request's body; `last` ends it. Every request has at least
    /// one, also one without a body.
    AskedBody {
        id: u64,
        bytes: Vec<u8>,
        last: bool,
    },
    /// The asker went away before its answer was whole.
    AskedGone {
        id: u64,
    },
    /// A caller that is not one of the run's own processes was turned away
    /// at `endpoint`, without a byte.
    Turned {
        endpoint: String,
    },
    /// The program could not be started, or the launcher was asked for
    /// something it does not do.
    Failed {
        message: String,
    },
    /// The program ended.
    Exited(ExitStatus),
}

/// What a host tells a served run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToRun {
    /// Bytes for the program's standard input.
    In(Vec<u8>),
    /// The program's standard input ends.
    CloseIn,
    /// The head of the answer to request `id`.
    Answer {
        id: u64,
        status: u16,
        headers: Vec<(String, String)>,
    },
    /// More of an answer's body; `last` ends it.
    AnswerBody { id: u64, bytes: Vec<u8>, last: bool },
    /// Ask the program to end, and end it when it has not within its grace.
    Stop,
}

/// A header that signs a request in, or that only frames one connection:
/// neither leaves the machine with a request, nor reaches the asker from an
/// answer.
#[must_use]
pub fn stays_behind(header: &str) -> bool {
    matches!(
        header,
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
            | "host"
            | "connection"
            | "keep-alive"
            | "content-length"
            | "transfer-encoding"
            | "expect"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{recv, send};

    tilo_test_env::scenario! {
        fn a_served_runs_frames_and_words_are_one_thing_on_both_sides;

        phase frames_travel_as_they_were_sent;
        {
            let mut wire = Vec::new();
            let asked = FromRun::Asked(Asked {
                id: 7,
                endpoint: "model".into(),
                method: "POST".into(),
                path: "/v1/messages?beta=true".into(),
                headers: vec![("content-type".into(), "application/json".into())],
            });
            let body = FromRun::AskedBody {
                id: 7,
                bytes: vec![1, 2, 3],
                last: true,
            };
            send(&mut wire, &asked).unwrap();
            send(&mut wire, &body).unwrap();
            let mut reader = wire.as_slice();
            assert_eq!(recv::<_, FromRun>(&mut reader).unwrap(), Some(asked));
            assert_eq!(recv::<_, FromRun>(&mut reader).unwrap(), Some(body));
            assert_eq!(recv::<_, FromRun>(&mut reader).unwrap(), None);

            let mut wire = Vec::new();
            let answer = ToRun::Answer {
                id: 7,
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
            };
            send(&mut wire, &answer).unwrap();
            send(&mut wire, &ToRun::Stop).unwrap();
            let mut reader = wire.as_slice();
            assert_eq!(recv::<_, ToRun>(&mut reader).unwrap(), Some(answer));
            assert_eq!(recv::<_, ToRun>(&mut reader).unwrap(), Some(ToRun::Stop));
        }

        phase a_body_chunk_fits_one_frame_with_room_to_spare;
        {
            let mut wire = Vec::new();
            send(
                &mut wire,
                &ToRun::AnswerBody {
                    id: u64::MAX,
                    bytes: vec![0xff; MAX_BODY_CHUNK],
                    last: false,
                },
            )
            .unwrap();
            assert!(wire.len() < crate::MAX_FRAME_BYTES);
        }

        phase the_launchers_arguments_name_its_endpoints_then_the_program;
        {
            let program = vec!["claude".to_owned(), "-p".to_owned(), "--".to_owned()];
            assert_eq!(
                arguments(&["model", "tilo"], Duration::from_millis(2500), &program),
                [
                    "serve-run", "--endpoint", "model", "--endpoint", "tilo", "--stop-grace-ms",
                    "2500", "--", "claude", "-p", "--"
                ]
            );
            assert_eq!(placeholder("model"), "{endpoint:model}");
            assert!(endpoint_name("model") && endpoint_name("git-2"));
            assert!(!endpoint_name("") && !endpoint_name("Model") && !endpoint_name("a b"));
        }

        phase what_signs_a_request_in_stays_in_the_machine;
        {
            for header in ["authorization", "x-api-key", "cookie", "proxy-authorization"] {
                assert!(stays_behind(header), "{header}");
            }
            for header in ["content-type", "anthropic-version", "accept", "mcp-session-id"] {
                assert!(!stays_behind(header), "{header}");
            }
        }
    }
}
