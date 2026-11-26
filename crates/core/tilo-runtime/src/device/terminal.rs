//! This device's workspaces' Terminal shells, run by ush here: a session's command line, its
//! shell opened on a terminal for the device that asked, and what it printed recorded on the
//! session.
//!
//! The workspace service answers a session record's `run` and `open-terminal` with these
//! ([`tilo_workspace_service::workspaces::terminal`]): the shells are ush's, and the workspace
//! service knows the records. A shell its programs run on this device here; one the device
//! takes work for runs them in the workspace's VM, through the hosted-VM service.

use std::sync::Arc;
use tilo_person_services::PersonServices;

use tilo_action_services::environment_commands::{Person, VmSecret};
use tilo_actions::remote::Caller;
use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, Watching, WorkspaceShell};
use tilo_workspace_service::workspaces::Workspaces;
use ush::environment_terminals::{EnvironmentTerminals, Ran, TerminalAsk};
use ush::workspace_shell::{RunWith, WorkspaceShells};

/// The shells of the workspaces this device holds.
pub(crate) struct HostShells {
    terminals: EnvironmentTerminals,
    /// The workspaces' records: the machine secrets document a program starts with.
    copies: Workspaces,
}

impl HostShells {
    /// The shells `shells` runs, reading the workspaces' machine secrets through `copies`.
    #[must_use]
    pub(crate) fn new(shells: WorkspaceShells, copies: Workspaces) -> Self {
        Self {
            terminals: EnvironmentTerminals::new(Arc::new(shells)),
            copies,
        }
    }

    /// Carry the terminal stream this device handed `device` over `from` and `to`: the device
    /// that asked for it presents its one-use id, and nobody else may take it.
    pub(crate) fn serve_terminal(
        &self,
        device: [u8; 32],
        from: impl std::io::Read + Send + 'static,
        to: impl std::io::Write,
    ) -> std::io::Result<ush::environment_terminals::Ended> {
        self.terminals.serve_handed(device, from, to)
    }

    /// The vault items `by`'s program in `workspace`'s machine starts with: what the workspace's
    /// machine secrets document names, each released for them, and nothing else.
    fn secrets(&self, workspace: &str, by: &Caller) -> Result<Vec<VmSecret>, String> {
        let copies = self.copies.clone();
        let (workspace, by) = (workspace.to_owned(), by.clone());
        super::workspaces::block_on_owned(async move {
            PersonServices::of(&copies)
                .machine_secrets(&workspace, &by)
                .await
        })
    }
}

impl WorkspaceShell for HostShells {
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        by: &Caller,
        agent: bool,
        person: Option<&Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let secrets = self.secrets(workspace, by)?;
        let ran = self
            .terminals
            .shells()
            .run_for(
                workspace,
                session,
                command,
                agent,
                RunWith {
                    payer: None,
                    person,
                    secrets: &secrets,
                    stopping: watching.stop,
                    seen: watching.seen,
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(ShellRan {
            command: ran.command,
            cwd: ran.cwd,
            exit: ran.exit,
            output: ran.output,
            truncated: ran.truncated,
            cancelled: ran.cancelled,
            started_ms: ran.started_ms,
            ended_ms: ran.ended_ms,
            refusal: ran.refusal,
        })
    }

    fn open(&self, opening: &Opening, by: &Caller, record: Recorder) -> Result<Opened, String> {
        let secrets = self.secrets(&opening.workspace, by)?;
        let opened = self.terminals.open_for(
            opening.device,
            &TerminalAsk {
                workspace: &opening.workspace,
                session: &opening.session,
                payer: None,
                person: opening.person.as_ref(),
                cols: opening.cols,
                rows: opening.rows,
                ran: Some(Arc::new(move |ran| {
                    record(ran_of(ran));
                })),
                attach: opening.attach.as_deref(),
                secrets: &secrets,
            },
        )?;
        Ok(Opened {
            stream: opened.stream,
            session: opened.session,
        })
    }
}

/// What a command run on a session's shell answered, as the workspace service records it.
fn ran_of(ran: Ran) -> ShellRan {
    ShellRan {
        command: ran.command,
        cwd: ran.cwd,
        exit: ran.exit,
        output: ran.output,
        truncated: ran.truncated,
        cancelled: false,
        started_ms: ran.started_ms,
        ended_ms: ran.ended_ms,
        refusal: None,
    }
}
