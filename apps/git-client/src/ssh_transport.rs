//! Stateful Git SSH transport without libssh2 or OpenSSL.
//!
//! Only an already trusted host and identities held by the local SSH agent are
//! accepted. No private key, trust prompt, or shell command is synthesized.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use russh::keys::agent::AgentIdentity;
use russh::{ChannelMsg, client};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_secs(60);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Called only by the shared transport registration gate, before network use.
pub(super) fn register() -> Result<(), git2::Error> {
    for scheme in ["ssh", "git+ssh", "ssh+git"] {
        // SAFETY: transport::with_context serializes registration before it
        // permits any of this application's remote operations.
        unsafe {
            git2::transport::register(scheme, |remote| {
                let config = super::transport::current_config()?;
                if config.get_string("core.sshCommand").is_ok()
                    || std::env::var_os("GIT_SSH_COMMAND").is_some()
                    || std::env::var_os("GIT_SSH").is_some()
                {
                    return Err(git2::Error::from_str(
                        "custom SSH commands are unsupported by the built-in SSH transport",
                    ));
                }
                Transport::smart(
                    remote,
                    false,
                    SshTransport {
                        username: config.get_string("credential.username").ok(),
                        session: Mutex::new(None),
                    },
                )
            })?;
        }
    }
    Ok(())
}

struct SshTransport {
    username: Option<String>,
    session: Mutex<Option<Arc<Mutex<SshSession>>>>,
}

impl SmartSubtransport for SshTransport {
    fn action(
        &self,
        url: &str,
        service: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let endpoint = Endpoint::parse(url, self.username.as_deref()).map_err(git_error)?;
        let command = endpoint.command(service);
        let session = Arc::new(Mutex::new(
            SshSession::connect(&endpoint, command).map_err(git_error)?,
        ));
        let mut current = self.session.lock().map_err(git_error)?;
        if current.is_some() {
            return Err(git2::Error::from_str("SSH session is already open"));
        }
        *current = Some(Arc::clone(&session));
        drop(current);
        Ok(Box::new(SshStream(session)))
    }

    fn close(&self) -> Result<(), git2::Error> {
        let session = self.session.lock().map_err(git_error)?.take();
        if let Some(session) = session {
            session.lock().map_err(git_error)?.close();
        }
        Ok(())
    }
}

fn git_error(error: impl std::fmt::Display) -> git2::Error {
    git2::Error::from_str(&format!("SSH: {error}"))
}

#[derive(Debug, PartialEq, Eq)]
struct Endpoint {
    host: String,
    port: u16,
    user: String,
    path: String,
}

impl Endpoint {
    fn parse(raw: &str, configured_user: Option<&str>) -> anyhow::Result<Self> {
        let (host, port, user, path) = if raw.contains("://") {
            let url = reqwest::Url::parse(raw).context("invalid SSH URL")?;
            if !matches!(url.scheme(), "ssh" | "git+ssh" | "ssh+git")
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!("unsupported SSH URL components");
            }
            let host = url.host_str().context("SSH URL has no host")?;
            (
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string(),
                url.port().unwrap_or(22),
                decode_url_component(url.username())?,
                decode_url_component(url.path().strip_prefix("/~").map_or_else(
                    || url.path(),
                    |_| url.path().strip_prefix('/').unwrap_or_else(|| url.path()),
                ))?,
            )
        } else {
            // SCP form: [user@]host:path, including bracketed IPv6 hosts.
            let separator = if let Some(bracket) = raw.find('[') {
                raw.get(bracket..)
                    .and_then(|rest| rest.find("]:"))
                    .map(|offset| bracket + offset + 1)
            } else {
                raw.find(':')
            }
            .context("SSH remote must have host:path or ssh:// form")?;
            let authority = raw.get(..separator).context("invalid SSH authority")?;
            let path = raw.get(separator + 1..).context("invalid SSH path")?;
            let (user, host) = authority.rsplit_once('@').unwrap_or(("", authority));
            (
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string(),
                22,
                user.to_string(),
                path.to_string(),
            )
        };
        let user = if user.is_empty() {
            configured_user
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(tilo_paths::session::user_name)
                .context("SSH remote needs a username")?
        } else {
            user
        };
        if host.is_empty()
            || user.is_empty()
            || path.is_empty()
            || path.starts_with('-')
            || [&host, &user, &path]
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            bail!("invalid SSH host, username, or repository path");
        }
        Ok(Self {
            host,
            port,
            user,
            path,
        })
    }

    fn command(&self, service: Service) -> String {
        let program = match service {
            Service::UploadPackLs | Service::UploadPack => "git-upload-pack",
            Service::ReceivePackLs | Service::ReceivePack => "git-receive-pack",
        };
        // A POSIX shell interprets SSH exec strings. Quote the entire path,
        // including embedded apostrophes; never permit it to become options.
        format!("{program} '{}'", self.path.replace('\'', "'\\''"))
    }
}

fn decode_url_component(value: &str) -> anyhow::Result<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut source = value.bytes();
    while let Some(byte) = source.next() {
        if byte == b'%' {
            let high = source.next().and_then(|b| char::from(b).to_digit(16));
            let low = source.next().and_then(|b| char::from(b).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                bail!("invalid percent escape in SSH URL");
            };
            bytes.push(u8::try_from(high * 16 + low)?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).context("SSH URL is not UTF-8")
}

struct HostVerifier {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}

impl client::Handler for HostVerifier {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = key.public_key();
        // russh's checker supports hashed entries but does not process marker
        // records. Honor explicit key revocation before accepting a plain entry.
        let entries = std::fs::read_to_string(&self.known_hosts)
            .context("cannot read ~/.ssh/known_hosts; trust the server explicitly first")?;
        for line in entries.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.first() == Some(&"@revoked") {
                let encoded = fields.get(3).context("malformed revoked SSH host key")?;
                let revoked = russh::keys::parse_public_key_base64(encoded)?;
                if revoked.key_data() == key.key_data() {
                    bail!("SSH host key is explicitly revoked");
                }
            }
        }
        if !russh::keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)? {
            bail!("unknown SSH host key; verify and add it to ~/.ssh/known_hosts first");
        }
        Ok(true)
    }
}

/// One SSH connection bridged to libgit2's synchronous stream API. The
/// caller's thread waits on each step with the app runtime's `block_on`,
/// while the runtime thread drives the socket and russh's session task,
/// including between calls.
struct SshSession {
    handle: client::Handle<HostVerifier>,
    channel: russh::Channel<client::Msg>,
    pending: io::Cursor<Vec<u8>>,
    closed: bool,
}

impl SshSession {
    fn connect(endpoint: &Endpoint, command: String) -> anyhow::Result<Self> {
        let (handle, channel) = ui::rt::app().block_on(async {
            tokio::time::timeout(CONNECT_TIMEOUT, async {
                let home = std::env::home_dir().context("no home directory for SSH host trust")?;
                let mut handle = client::connect(
                    Arc::new(client::Config {
                        inactivity_timeout: Some(IO_TIMEOUT),
                        ..Default::default()
                    }),
                    (endpoint.host.as_str(), endpoint.port),
                    HostVerifier {
                        host: endpoint.host.clone(),
                        port: endpoint.port,
                        known_hosts: home.join(".ssh").join("known_hosts"),
                    },
                )
                .await?;
                let mut agent = ssh_agent_client::connect()
                    .await
                    .context("cannot connect to SSH agent")?;
                let hash = handle.best_supported_rsa_hash().await?.flatten();
                let mut authenticated = false;
                for identity in agent.request_identities().await? {
                    let AgentIdentity::PublicKey { key, .. } = identity else {
                        continue;
                    };
                    if handle
                        .authenticate_publickey_with(endpoint.user.clone(), key, hash, &mut agent)
                        .await?
                        .success()
                    {
                        authenticated = true;
                        break;
                    }
                }
                if !authenticated {
                    bail!("SSH agent has no accepted public-key identity");
                }
                let channel = handle.channel_open_session().await?;
                channel.exec(true, command).await?;
                Ok::<_, anyhow::Error>((handle, channel))
            })
            .await
            .context("SSH connection or authentication timed out")?
        })??;
        Ok(Self {
            handle,
            channel,
            pending: io::Cursor::new(Vec::new()),
            closed: false,
        })
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = ui::rt::app().block_on(async {
            tokio::time::timeout(CLOSE_TIMEOUT, async {
                let _ = self.channel.close().await;
                self.handle
                    .disconnect(
                        russh::Disconnect::ByApplication,
                        "Git operation finished",
                        "en",
                    )
                    .await
            })
            .await
        });
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        self.close();
    }
}

struct SshStream(Arc<Mutex<SshSession>>);

impl Read for SshStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut session = self
            .0
            .lock()
            .map_err(|_| io::Error::other("SSH session lock poisoned"))?;
        let count = session.pending.read(output)?;
        if count != 0 {
            return Ok(count);
        }
        if session.closed {
            return Ok(0);
        }
        let SshSession {
            channel, pending, ..
        } = &mut *session;
        let result = ui::rt::app().block_on(async {
            tokio::time::timeout(IO_TIMEOUT, async {
                loop {
                    match channel.wait().await {
                        Some(ChannelMsg::Data { data }) => {
                            *pending = io::Cursor::new(data.to_vec());
                            let count = pending.read(output)?;
                            if count != 0 {
                                return Ok(count);
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                            return Err(io::Error::other(format!(
                                "remote Git command exited with status {exit_status}"
                            )));
                        }
                        Some(ChannelMsg::Failure | ChannelMsg::ExitSignal { .. }) => {
                            return Err(io::Error::other(
                                "remote rejected or terminated the Git command",
                            ));
                        }
                        Some(ChannelMsg::Close) | None => return Ok(0),
                        // stderr is not Git protocol data. Do not buffer an
                        // unbounded diagnostic stream or expose it as pack data.
                        _ => {}
                    }
                }
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SSH read timed out"))?
        });
        drop(session);
        result.map_err(io::Error::other)?
    }
}

impl Write for SshStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let session = self
            .0
            .lock()
            .map_err(|_| io::Error::other("SSH session lock poisoned"))?;
        if session.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "SSH session is closed",
            ));
        }
        let result = ui::rt::app().block_on(async {
            tokio::time::timeout(IO_TIMEOUT, session.channel.data(input))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SSH write timed out"))?
                .map_err(io::Error::other)
        });
        drop(session);
        result.map_err(io::Error::other)??;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SSH transport: scp/url remotes preserve repository paths, remote shell
    /// arguments cannot escape the path, and host verification requires existing
    /// trust and rejects revocation. Each phase is a former test with its own fresh fixture.
    #[test]
    fn ssh_remotes_keep_repository_paths_and_require_existing_trust() {
        scp_and_url_remotes_preserve_repository_paths();
        remote_shell_arguments_cannot_escape_the_repository_path();
        host_verifier_requires_existing_trust_and_rejects_revocation();
    }

    fn scp_and_url_remotes_preserve_repository_paths() {
        let endpoint = Endpoint::parse("git@example.org:team/repo.git", None).unwrap();
        assert_eq!(endpoint.host, "example.org");
        assert_eq!(endpoint.user, "git");
        assert_eq!(endpoint.path, "team/repo.git");
        let endpoint = Endpoint::parse("ssh://git@[::1]:2222/a%20repo.git", None).unwrap();
        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.port, 2222);
        assert_eq!(endpoint.path, "/a repo.git");
    }

    fn remote_shell_arguments_cannot_escape_the_repository_path() {
        let endpoint =
            Endpoint::parse("git@example.org:repo'; touch /tmp/nope; echo '", None).unwrap();
        assert_eq!(
            endpoint.command(Service::UploadPackLs),
            "git-upload-pack 'repo'\\''; touch /tmp/nope; echo '\\'''"
        );
        for url in [
            "git@example.org:-option",
            "ssh://git@example.org/a%00b",
            "ssh://git:secret@example.org/repo",
        ] {
            assert!(Endpoint::parse(url, None).is_err(), "{url}");
        }
    }

    fn host_verifier_requires_existing_trust_and_rejects_revocation() {
        use russh::client::Handler as _;
        const ENCODED: &str =
            "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let key = russh::keys::PublicKeyOrCertificate::from(
            russh::keys::parse_public_key_base64(ENCODED).unwrap(),
        );
        let mut verifier = HostVerifier {
            host: "localhost".into(),
            port: 2222,
            known_hosts: known_hosts.clone(),
        };
        let runtime = ui::rt::app();
        assert!(
            runtime
                .block_on(verifier.check_server_key(&key))
                .unwrap()
                .is_err()
        );
        std::fs::write(&known_hosts, "").unwrap();
        assert!(
            runtime
                .block_on(verifier.check_server_key(&key))
                .unwrap()
                .is_err()
        );
        let trusted = format!("[localhost]:2222 ssh-ed25519 {ENCODED}\n");
        std::fs::write(&known_hosts, &trusted).unwrap();
        assert!(
            runtime
                .block_on(verifier.check_server_key(&key))
                .unwrap()
                .unwrap()
        );
        std::fs::write(
            &known_hosts,
            format!("{trusted}@revoked [localhost]:2222 ssh-ed25519 {ENCODED}\n"),
        )
        .unwrap();
        assert!(
            runtime
                .block_on(verifier.check_server_key(&key))
                .unwrap()
                .is_err()
        );
    }
}
