//! Connect to the user's SSH agent, wherever the platform keeps it.
//!
//! Unix agents listen on the socket named by `SSH_AUTH_SOCK`; the Windows
//! OpenSSH agent listens on a named pipe (`SSH_AUTH_SOCK` or its default).
//! Callers get one agent type either way.

use russh::keys::agent::client::{AgentClient, AgentStream};

/// A connected SSH agent.
pub type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// Connect to the user's SSH agent.
pub async fn connect() -> Result<Agent, russh::keys::Error> {
    #[cfg(unix)]
    {
        Ok(AgentClient::connect_env().await?.dynamic())
    }
    #[cfg(windows)]
    {
        Ok(AgentClient::connect_named_pipe(
            std::env::var_os("SSH_AUTH_SOCK")
                .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into()),
        )
        .await?
        .dynamic())
    }
}
