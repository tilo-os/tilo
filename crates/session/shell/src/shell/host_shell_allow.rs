//! Terminal's Host target — the device's own login shell — asked once.
//!
//! The shell runtime (`ush`) refuses the Host target until the person has
//! allowed it on this device. A shell-hosted Terminal tab that hits that
//! refusal raises [`ui::widget::AppHostIntent::AllowHostShell`]; the shell
//! puts its ordinary permission prompt in front of the person and records
//! the answer as a `CapOverride` on `Scope::Device("host-shell")` in the
//! runtime's own store, where the supervisor reads it on the next request.
//!
//! "Always" records it for good. "Allow" and "This session" let the shell
//! being opened right now through: the grant they write expires within a
//! minute, long before anyone could reuse it.

use std::cell::RefCell;
use std::rc::Weak;

use tilo_desktop_model::capability::{Capability, GRANT_ALWAYS};
use ui::widget::HostShellAllow;

use crate::portal::PromptOutcome;

/// How long a one-time allow stays valid: enough for the retry that follows
/// the answer, not enough to matter later.
const ONE_TIME_SECONDS: u64 = 60;

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

/// Record the person's answer where the shell runtime reads it. Returns
/// whether the Host target is now allowed.
pub fn record(outcome: PromptOutcome) -> bool {
    let expiry = match outcome {
        PromptOutcome::Deny => return false,
        PromptOutcome::AllowAlways => collab_types::caps::Expiry::Never,
        PromptOutcome::AllowOnce | PromptOutcome::AllowSession => {
            collab_types::caps::Expiry::AtUnix(
                tilo_clock::unix_now().saturating_add(ONE_TIME_SECONDS),
            )
        }
    };
    write_allow(expiry)
}

#[cfg(target_os = "linux")]
fn write_allow(expiry: collab_types::caps::Expiry) -> bool {
    write_allow_at(
        &ush::host::HostPolicy::path(&ush::Store::discover()),
        expiry,
    )
}

#[cfg(target_os = "linux")]
fn write_allow_at(path: &std::path::Path, expiry: collab_types::caps::Expiry) -> bool {
    let mut policy = ush::host::HostPolicy::load(path);
    let id = policy.allow_host_shell(TERMINAL_APP_ID);
    if let Some(cap) = policy.capabilities.iter_mut().find(|cap| cap.id == id) {
        cap.expiry = expiry;
    }
    match policy.save(path) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "the allow for the device's own shell could not be recorded");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn write_allow(_expiry: collab_types::caps::Expiry) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn an_answer_reaches_the_tab_and_only_an_allow_reaches_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        let now = tilo_clock::unix_now();

        assert!(write_allow_at(&path, collab_types::caps::Expiry::Never));
        let policy = ush::host::HostPolicy::load(&path);
        assert!(policy.allows_host_shell(TERMINAL_APP_ID, Some(now)));
        assert!(
            !policy.allows_host_shell("so.tilo.notes", Some(now)),
            "the allow names Terminal and nobody else"
        );

        let once = dir.path().join("once.json");
        assert!(write_allow_at(
            &once,
            collab_types::caps::Expiry::AtUnix(now + ONE_TIME_SECONDS)
        ));
        let policy = ush::host::HostPolicy::load(&once);
        assert!(policy.allows_host_shell(TERMINAL_APP_ID, Some(now)));
        assert!(
            !policy.allows_host_shell(TERMINAL_APP_ID, Some(now + ONE_TIME_SECONDS + 1)),
            "a one-time allow is gone after the shell it was for"
        );

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
