//! Connect to the user's SSH agent, wherever the platform keeps it: Unix agents
//! listen on the socket named by `SSH_AUTH_SOCK`, the Windows OpenSSH agent on a
//! named pipe (`SSH_AUTH_SOCK` or its default).

use russh::keys::agent::client::{AgentClient, AgentStream};

pub type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

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
