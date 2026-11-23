//! A question this computer's own agent puts to its person, answered on this
//! computer when it holds the person's root (`tilo login --new-identity`).
//!
//! A workspace command acting as the computer's device acts as its agent, and
//! what the person's grants leave to them ("ask first": making a workspace,
//! inviting someone) is put to the devices that hold their identity. Anything
//! on a computer that holds the root can read it, so asking a tab adds nothing
//! there: while a command is under way, the root answers its agent's
//! questions as any device holding the root does (the server's question list,
//! signed by the root), and the command goes on in one go. `tilo keys create`
//! approves its key here the same way. A computer without the root leaves the
//! question to the person, and the command says it waits for them.
//!
//! The root is read in a process of its own (`tilo approve-here`), started
//! beside the command: a process that reads it makes itself undumpable, and
//! the process holding the device's session admits only callers whose
//! namespaces it can read.

use std::process::ExitCode;
use std::time::Duration;

use tilo_server_client::questions::{MemberQuestions, QuestionServer as _};
use tilo_server_protocol::certify::ConfirmationAnswer;

/// How often the server's question list is read while a command is under way.
const LOOKING: Duration = Duration::from_millis(250);

/// Answering this computer's agent's questions until dropped.
pub(crate) struct Approving(std::process::Child);

impl Drop for Approving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Answer what this computer's agent asks its person while the answer is
/// kept, when this computer holds the person's root; `None` otherwise.
pub(crate) fn start() -> Option<Approving> {
    if !crate::identity::holds_root() {
        return None;
    }
    std::process::Command::new(std::env::current_exe().ok()?)
        .arg("approve-here")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .ok()
        .map(Approving)
}

/// `tilo approve-here`: answer every question this computer's agent puts to
/// the person whose root this computer holds, allowing each, until stopped.
///
/// # Errors
/// The runtime did not start.
pub(crate) fn serve() -> anyhow::Result<ExitCode> {
    let Some(person) = crate::identity::held()? else {
        return Ok(ExitCode::SUCCESS);
    };
    let Some(device) = crate::login::home() else {
        return Ok(ExitCode::SUCCESS);
    };
    if device.cert.user_public_key != person.public_key() {
        return Ok(ExitCode::SUCCESS);
    }
    let agent = tilo_actions::remote::device_agent_id(&person.public_key(), &device.agent);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let server = crate::login::server_of(&device).await?;
            let member = tilo_server_client::MemberApprover::new(server);
            let mut questions = MemberQuestions::of_root(&member, &person.root);
            loop {
                for question in questions.confirmations().await.unwrap_or_default() {
                    if question.agent_id == agent
                        && questions
                            .answer(question.id, ConfirmationAnswer::AllowOnce)
                            .await
                            .is_ok()
                    {
                        eprintln!("Approved here, as {}.", person.who());
                    }
                }
                tokio::time::sleep(LOOKING).await;
            }
        })
}
