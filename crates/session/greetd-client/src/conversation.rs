//! Cancellation of the ordinary greetd exchange, with ordered complete frames.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use greetd_ipc::codec::SyncCodec;
use greetd_ipc::{AuthMessageType, Request, Response};

use crate::{AuthError, ExchangeIo, exchange, write_request};
use tilo_lock::MutexRecover as _;

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Authenticating,
    Starting,
    Finished,
}

#[derive(Default)]
struct ControlState {
    writer: Option<UnixStream>,
    cancelled: bool,
    phase: Phase,
    created: bool,
    pending: usize,
}

/// Transient control of one attempt. It contains no credentials and cannot
/// change the account or the session command admitted by its owning greeter.
#[derive(Clone, Default)]
pub struct ConversationControl(Arc<Mutex<ControlState>>);

impl ConversationControl {
    /// Queue normal `CancelSession` and forbid `StartSession` immediately. greetd
    /// handles requests serially, so completion waits for its pending responses.
    ///
    /// # Errors
    /// Returns a content-free refusal after session start has been sealed, or
    /// when its ordinary socket cannot carry the cancellation request.
    pub fn cancel(&self) -> Result<(), AuthError> {
        let mut state = self.0.lock_recover();
        if state.phase != Phase::Authenticating {
            return Err(AuthError::Greetd(
                "The authentication attempt is no longer cancellable".into(),
            ));
        }
        if state.cancelled {
            return Ok(());
        }
        state.cancelled = true;
        if state.created {
            let writer = state.writer.as_mut().ok_or_else(|| {
                AuthError::Transport("The authentication connection is unavailable".into())
            })?;
            // Hold one lock for the whole codec call, including its length
            // prefix, so cancellation cannot interleave another request frame.
            Request::CancelSession.write_to(writer).map_err(|_| {
                AuthError::Transport("Could not cancel the authentication connection".into())
            })?;
            state.pending = state.pending.checked_add(1).ok_or_else(|| {
                AuthError::Transport("Authentication response accounting exceeded its limit".into())
            })?;
        }
        drop(state);
        Ok(())
    }

    /// Whether the caller has requested cancellation of this attempt.
    pub fn is_cancelled(&self) -> bool {
        self.0.lock_recover().cancelled
    }

    /// Actual attempt readiness, without inspecting any authentication value.
    pub fn can_cancel(&self) -> bool {
        let state = self.0.lock_recover();
        state.phase == Phase::Authenticating && !state.cancelled
    }

    fn finish(&self) {
        let mut state = self.0.lock_recover();
        state.phase = Phase::Finished;
        state.writer.take();
    }
}

struct ConversationExchange {
    reader: UnixStream,
    control: ConversationControl,
}

impl ConversationExchange {
    fn connect(sock: &str, control: ConversationControl) -> Result<Self, AuthError> {
        let reader = UnixStream::connect(sock)
            .map_err(|_| AuthError::Transport("Could not reach the login service".into()))?;
        let writer = reader
            .try_clone()
            .map_err(|_| AuthError::Transport("Could not prepare the login connection".into()))?;
        control.0.lock_recover().writer = Some(writer);
        Ok(Self { reader, control })
    }

    fn read_response(&mut self) -> Result<Response, AuthError> {
        let response = Response::read_from(&mut self.reader).map_err(|_| {
            AuthError::Transport("Could not read the login service response".into())
        })?;
        let mut state = self.control.0.lock_recover();
        state.pending = state.pending.saturating_sub(1);
        drop(state);
        Ok(response)
    }
}

impl ExchangeIo for ConversationExchange {
    fn send(&mut self, request: Request) -> Result<(), AuthError> {
        let mut state = self.control.0.lock_recover();
        if state.cancelled {
            drop(state);
            return self.check_cancelled();
        }
        let created = matches!(request, Request::CreateSession { .. });
        let writer = state
            .writer
            .as_mut()
            .ok_or_else(|| AuthError::Transport("The login connection is unavailable".into()))?;
        write_request(request, writer)
            .map_err(|_| AuthError::Transport("Could not write to the login service".into()))?;
        state.created |= created;
        state.pending = state.pending.checked_add(1).ok_or_else(|| {
            AuthError::Transport("Authentication response accounting exceeded its limit".into())
        })?;
        drop(state);
        Ok(())
    }

    fn receive(&mut self) -> Result<Response, AuthError> {
        self.read_response()
    }

    fn check_cancelled(&mut self) -> Result<(), AuthError> {
        if !self.control.is_cancelled() {
            return Ok(());
        }
        // The original pending request and queued CancelSession each have a
        // response. Read both without treating either Success as authentication.
        loop {
            let pending = self.control.0.lock_recover().pending;
            if pending == 0 {
                break;
            }
            self.read_response()?;
        }
        Err(AuthError::Cancelled)
    }

    fn prepare_start(&mut self) -> Result<(), AuthError> {
        let mut state = self.control.0.lock_recover();
        if state.cancelled {
            drop(state);
            return self.check_cancelled();
        }
        state.phase = Phase::Starting;
        drop(state);
        Ok(())
    }

    fn terminal_failure(&mut self) -> Result<(), AuthError> {
        let mut state = self.control.0.lock_recover();
        if state.cancelled {
            drop(state);
            return self.check_cancelled();
        }
        // A mapped PAM refusal wins before the worker's cleanup acknowledgement;
        // a later remote cancellation cannot enqueue a second cleanup request.
        state.phase = Phase::Finished;
        drop(state);
        Ok(())
    }
}

impl Drop for ConversationExchange {
    fn drop(&mut self) {
        self.control.finish();
    }
}

/// Run the same login exchange with an interactive response callback and
/// cancellation control. The callback owns private transient response values;
/// this client neither publishes nor logs them.
///
/// # Errors
/// Returns the ordinary typed greetd error, or Cancelled after the normal
/// cancellation exchange ends. Success exits the greeter for greetd as usual.
#[allow(
    clippy::exit,
    reason = "greetd takes over the VT after the greeter exits on normal authenticated session start"
)]
pub fn authenticate_conversation<F, G>(
    sock: &str,
    username: &str,
    cmd: &[String],
    env: &[String],
    control: &ConversationControl,
    response: F,
    on_accepted: G,
) -> Result<(), AuthError>
where
    F: FnMut(AuthMessageType, String) -> Result<Option<String>, AuthError>,
    G: FnMut(),
{
    let mut connection =
        ConversationExchange::connect(sock, control.clone()).inspect_err(|_| {
            control.finish();
        })?;
    exchange(&mut connection, username, cmd, env, response, on_accepted)?;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Denial;
    use greetd_ipc::ErrorType;

    fn connection() -> (ConversationExchange, UnixStream) {
        let (reader, server) = UnixStream::pair().unwrap();
        let control = ConversationControl::default();
        control.0.lock().unwrap().writer = Some(reader.try_clone().unwrap());
        (ConversationExchange { reader, control }, server)
    }

    #[test]
    fn failed_connection_is_not_a_cancellable_attempt() {
        let control = ConversationControl::default();
        let result = authenticate_conversation(
            "/dev/null/tilo-greetd-not-a-socket",
            "owner",
            &["fixed-session".into()],
            &[],
            &control,
            |_, _| panic!("unreachable service cannot issue a challenge"),
            || panic!("unreachable service cannot accept credentials"),
        );
        assert!(matches!(result, Err(AuthError::Transport(_))));
        assert!(!control.can_cancel());
        assert!(control.cancel().is_err());
        assert!(!control.is_cancelled());
    }

    #[test]
    fn interactive_conversation_answers_each_actual_prompt_and_preserves_denial() {
        let (mut client, mut server) = connection();
        let daemon = std::thread::spawn(move || {
            assert!(
                matches!(Request::read_from(&mut server).unwrap(), Request::CreateSession { username } if username == "owner")
            );
            for (kind, expected) in [
                (AuthMessageType::Secret, "password"),
                (AuthMessageType::Visible, "second-factor"),
                (AuthMessageType::Secret, "new-passcode"),
            ] {
                Response::AuthMessage {
                    auth_message_type: kind,
                    auth_message: "Private challenge".into(),
                }
                .write_to(&mut server)
                .unwrap();
                assert!(
                    matches!(Request::read_from(&mut server).unwrap(), Request::PostAuthMessageResponse { response } if response.as_deref() == Some(expected))
                );
            }
            Response::Error {
                error_type: ErrorType::AuthError,
                description: "pam_authenticate: AUTH_ERR".into(),
            }
            .write_to(&mut server)
            .unwrap();
            assert!(matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CancelSession
            ));
            Response::Success.write_to(&mut server).unwrap();
        });
        let mut answers = ["password", "second-factor", "new-passcode"].into_iter();
        let result = exchange(
            &mut client,
            "owner",
            &["fixed-session".into()],
            &[],
            |_, _| Ok(Some(answers.next().unwrap().into())),
            || panic!("denial cannot accept credentials"),
        );
        assert!(matches!(
            result,
            Err(AuthError::Denied {
                reason: Denial::Credentials,
                ..
            })
        ));
        assert!(answers.next().is_none());
        daemon.join().unwrap();
    }

    #[test]
    fn cancellation_queues_complete_frames_and_cannot_credit_pending_auth_success() {
        let (mut client, mut server) = connection();
        let control = client.control.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let daemon = std::thread::spawn(move || {
            assert!(matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CreateSession { .. }
            ));
            ready_tx.send(()).unwrap();
            // The original request has no response yet. Cancellation is queued
            // behind it, as greetd's serial request handler does in production.
            assert!(matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CancelSession
            ));
            Response::Success.write_to(&mut server).unwrap();
            Response::Success.write_to(&mut server).unwrap();
            assert!(
                Request::read_from(&mut server).is_err(),
                "no StartSession after cancellation"
            );
        });
        let worker = std::thread::spawn(move || {
            exchange(
                &mut client,
                "owner",
                &["fixed-session".into()],
                &[],
                |_, _| panic!("no challenge issued"),
                || panic!("cancelled success cannot be accepted"),
            )
        });
        ready_rx.recv().unwrap();
        control.cancel().unwrap();
        assert!(matches!(worker.join().unwrap(), Err(AuthError::Cancelled)));
        // Drop the remaining control clone so the server can prove EOF.
        drop(control);
        daemon.join().unwrap();
    }

    #[test]
    fn accepted_start_is_sealed_before_the_acceptance_callback() {
        let (mut client, mut server) = connection();
        let control = client.control.clone();
        let daemon = std::thread::spawn(move || {
            assert!(matches!(
                Request::read_from(&mut server).unwrap(),
                Request::CreateSession { .. }
            ));
            Response::Success.write_to(&mut server).unwrap();
            assert!(
                matches!(Request::read_from(&mut server).unwrap(), Request::StartSession { cmd, env } if cmd == ["fixed-session"] && env == ["FIXED=1"])
            );
            Response::Success.write_to(&mut server).unwrap();
        });
        exchange(
            &mut client,
            "owner",
            &["fixed-session".into()],
            &["FIXED=1".into()],
            |_, _| panic!("no challenge issued"),
            || {
                assert!(matches!(control.cancel(), Err(AuthError::Greetd(_))));
            },
        )
        .unwrap();
        assert!(!control.is_cancelled());
        daemon.join().unwrap();
    }
}
