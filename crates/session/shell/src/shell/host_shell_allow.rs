//! Terminal's Host target — the device's own login shell — asked once.
//!
//! The shell runtime (`ush`) refuses the Host target until the person has
//! allowed it on this device. A shell-hosted Terminal tab that hits that
//! refusal raises [`ui::widget::AppHostIntent::AllowHostShell`]; the shell
//! puts its ordinary permission prompt in front of the person and hands the
//! answer to the supervisor as a `CapOverride` on `Scope::Device("host-shell")`
//! with the prompt's lifetime: "Always" (`Expiry::Never`) is kept on disk,
//! "This session" (`Expiry::EndOfSession`) and "Allow" (`Expiry::AfterUse`,
//! spent by the shell it opens) live in the supervisor until logout.

use std::cell::RefCell;
use std::rc::Weak;

use tilo_desktop_model::capability::{Capability, GRANT_ALWAYS};
use ui::widget::HostShellAllow;

use crate::portal::PromptOutcome;

/// One prompt the shell is waiting on, and the tab waiting for its answer.
pub struct PendingHostShellAllow {
    answer: tokio::sync::oneshot::Receiver<PromptOutcome>,
    reply: Weak<RefCell<HostShellAllow>>,
}

/// The prompt to queue for a Terminal tab asking for the device's own shell.
pub fn prompt(
    reply: Weak<RefCell<HostShellAllow>>,
) -> (
    crate::shell::chrome_state::PendingPrompt,
    PendingHostShellAllow,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (
        crate::shell::chrome_state::PendingPrompt::with_reply(
            TERMINAL_APP_ID.to_owned(),
            Capability::HostShell {
                scope: GRANT_ALWAYS,
            },
            tx,
        ),
        PendingHostShellAllow { answer: rx, reply },
    )
}

/// The app the Host target belongs to.
pub const TERMINAL_APP_ID: &str = "so.tilo.terminal";

impl PendingHostShellAllow {
    /// Deliver the answer once the person has given it. Returns `false`
    /// while the prompt is still up.
    pub fn poll(&mut self) -> bool {
        let allowed = match self.answer.try_recv() {
            Ok(outcome) => record(outcome),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => false,
        };
        if let Some(reply) = self.reply.upgrade()
            && let Ok(mut reply) = reply.try_borrow_mut()
        {
            reply.answer = Some(allowed);
        }
        true
    }
}

/// The lifetime the person chose, in the shared vocabulary; `None` for a
/// refusal.
pub fn lifetime(outcome: PromptOutcome) -> Option<collab_types::caps::Expiry> {
    match outcome {
        PromptOutcome::Deny => None,
        PromptOutcome::AllowAlways => Some(collab_types::caps::Expiry::Never),
        PromptOutcome::AllowSession => Some(collab_types::caps::Expiry::EndOfSession),
        PromptOutcome::AllowOnce => Some(collab_types::caps::Expiry::AfterUse),
    }
}

/// Hand the person's answer to the shell runtime. Returns whether the Host
/// target is now allowed.
pub fn record(outcome: PromptOutcome) -> bool {
    lifetime(outcome).is_some_and(tell_supervisor)
}

#[cfg(target_os = "linux")]
fn tell_supervisor(expiry: collab_types::caps::Expiry) -> bool {
    tell_supervisor_at(&launcher::services::socket_for("ush"), expiry)
}

/// One `allow-host` request on a fresh connection. The shell connects from
/// outside every app sandbox, which is what lets the supervisor accept it.
#[cfg(target_os = "linux")]
fn tell_supervisor_at(socket: &std::path::Path, expiry: collab_types::caps::Expiry) -> bool {
    use std::io::{BufRead, Write};
    let result = (|| -> std::io::Result<bool> {
        let stream = std::os::unix::net::UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let mut line = serde_json::to_vec(&ush::supervisor::ServeRequest::AllowHost { expiry })
            .map_err(std::io::Error::other)?;
        line.push(b'\n');
        (&stream).write_all(&line)?;
        let mut reply = String::new();
        std::io::BufReader::new(&stream).read_line(&mut reply)?;
        Ok(matches!(
            serde_json::from_str(reply.trim()),
            Ok(ush::supervisor::ServeResponse::Allowed)
        ))
    })();
    result.unwrap_or_else(|error| {
        tracing::warn!(%error, "the allow for the device's own shell could not be recorded");
        false
    })
}

#[cfg(not(target_os = "linux"))]
fn tell_supervisor(_expiry: collab_types::caps::Expiry) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_prompts_answers_are_the_shared_lifetimes_and_reach_the_supervisor() {
        use collab_types::caps::Expiry;
        assert_eq!(lifetime(PromptOutcome::AllowAlways), Some(Expiry::Never));
        assert_eq!(
            lifetime(PromptOutcome::AllowSession),
            Some(Expiry::EndOfSession)
        );
        assert_eq!(lifetime(PromptOutcome::AllowOnce), Some(Expiry::AfterUse));
        assert_eq!(lifetime(PromptOutcome::Deny), None);

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ush.sock");
        let store = ush::Store::at(dir.path().join("store"));
        store.ensure().unwrap();
        let serve_socket = socket.clone();
        let serve_store = store.clone();
        std::thread::spawn(move || {
            let _ = ush::supervisor::serve(&serve_socket, &serve_store, std::path::Path::new("/"));
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(tell_supervisor_at(&socket, Expiry::Never));
        assert!(
            ush::host::HostPolicy::load(&ush::host::HostPolicy::path(&store))
                .allows_host_shell(TERMINAL_APP_ID, None),
            "\"Always\" is kept where the supervisor reads it"
        );
        assert!(!tell_supervisor_at(
            &dir.path().join("absent.sock"),
            Expiry::Never
        ));

        let reply = std::rc::Rc::new(RefCell::new(HostShellAllow::default()));
        let (pending_prompt, mut pending) = prompt(std::rc::Rc::downgrade(&reply));
        assert!(matches!(
            pending_prompt.capability,
            Capability::HostShell { .. }
        ));
        assert!(
            !pending.poll(),
            "nothing is answered before the person answers"
        );
        let mut pending_prompt = pending_prompt;
        pending_prompt.answer(PromptOutcome::Deny);
        assert!(pending.poll());
        assert_eq!(reply.borrow().answer, Some(false));
    }
}
