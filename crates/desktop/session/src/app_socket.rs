//! Where the apps this session starts connect, and how each connection reaches the device's
//! daemon.
//!
//! The launcher connects every app the session starts to the socket it hands it, and the
//! session's own windows connect there too. Only this session can tell those connections apart:
//! it started the processes, they run as the person, and the daemon runs as the device's own
//! account and sees neither the processes nor this socket. So the session attests each
//! connection by the launch it recorded ([`Launches`]) and hands the connection itself to the
//! daemon over its shell link, vouching for the app, workspace and window it belongs to. The
//! daemon serves the app from then on: its documents, its actions, what it asks of the person,
//! and the session's own services through the providers the session serves on the same link.
//! This session decides nothing an app asks.

use std::sync::Arc;

use tilo_lock::MutexRecover as _;

use tilo_app_host::collab_app_broker::seat;

use crate::launches::Launches;

/// The app socket while this session serves it: dropping it stops taking connections and
/// removes the socket. A connection already handed to the daemon is the daemon's.
pub(crate) struct AppSocket {
    _socket: seat::AppSocket,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for AppSocket {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Serve the app socket at `path`: each connection is attested by `launches` and handed to the
/// daemon over `shell`.
///
/// # Errors
/// The socket's directory is not this account's own, another session answers at `path`, or the
/// thread that takes connections could not start.
pub(crate) fn serve(
    path: &std::path::Path,
    launches: Arc<Launches>,
    shell: runtime_collab::ShellClient,
) -> std::io::Result<AppSocket> {
    // The launcher's handoff is read where the connection is taken, on a thread of its own: a
    // slow or stalled app never holds the session's loop.
    taking(path, "tilo-app-socket", move |listener, stopped| {
        accept(listener, launches, shell, stopped)
    })
}

/// Serve the identity socket at `path`: each connection is handed to the daemon's identity
/// service over `shell`. An app presents itself there by what it says, as it did to the identity
/// service the session ran itself, so the session attests nothing of it.
///
/// # Errors
/// As [`serve`].
pub(crate) fn serve_identity(
    path: &std::path::Path,
    shell: runtime_collab::ShellClient,
) -> std::io::Result<AppSocket> {
    taking(path, "tilo-identity-socket", move |listener, stopped| {
        hand_identity(listener, shell, stopped)
    })
}

/// The kind of question the API socket asks the person about a program of theirs.
pub(crate) const PROGRAM_KIND: &str = "session.api-program";

/// What the person is asked about a program: its executable, by its path.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProgramAsked {
    pub(crate) program: String,
}

/// Which programs of this login the person said yes to, and how they are asked about the others.
///
/// A program is the executable the kernel names for the process at the other end of its
/// connection, pinned by the connection's pidfd ([`peer_facts`]). Being a program of this
/// login admits nothing by itself: only the person's yes to that program does, given on the
/// system's own sheet as an asked record ([`crate::program_asks`]) and never by anything the
/// program says. A yes is kept for that program alone, and no longer than this session runs.
pub(crate) struct Programs {
    questions: Option<tilo_service::AskSender>,
    /// Allowed while this session runs.
    session: std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>>,
    /// The connections of each program the person is being asked about, waiting for the answer.
    waiting: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Vec<Told>>>,
}

/// Where one waiting connection of a program is told the person's answer.
type Told = tokio::sync::oneshot::Sender<Result<(), String>>;

impl Programs {
    /// The programs the person allowed, asked about through `questions`.
    pub(crate) fn new(questions: Option<tilo_service::AskSender>) -> Arc<Self> {
        Arc::new(Self {
            questions,
            session: std::sync::Mutex::default(),
            waiting: std::sync::Mutex::default(),
        })
    }

    /// Whether the person lets `program` act as them: at once when they said so before, else
    /// once they answer the question put to them. Connections of one program that come while
    /// it is asked about wait for that one answer.
    ///
    /// # Errors
    /// What the program is told: the person declined it, or nobody here can be asked.
    async fn admit(&self, program: &std::path::Path) -> Result<(), String> {
        if self.session.lock_recover().contains(program) {
            return Ok(());
        }
        let (told, answer) = tokio::sync::oneshot::channel();
        let first = {
            let mut waiting = self.waiting.lock_recover();
            let asked = waiting.entry(program.to_owned()).or_default();
            asked.push(told);
            let first = asked.len() == 1;
            drop(waiting);
            first
        };
        if first {
            let decided = self.ask(program).await;
            let waiting = self
                .waiting
                .lock_recover()
                .remove(program)
                .unwrap_or_default();
            for told in waiting {
                let _ = told.send(decided.clone());
            }
        }
        answer
            .await
            .unwrap_or_else(|_| Err(tilo_sheets::word("api-program-declined")))
    }

    /// Put `program` to the person and keep their answer as long as they chose.
    async fn ask(&self, program: &std::path::Path) -> Result<(), String> {
        let Some(questions) = &self.questions else {
            return Err(tilo_sheets::word("api-program-nobody-asked"));
        };
        let label = program.file_name().map_or_else(
            || program.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let details = serde_json::to_value(ProgramAsked {
            program: program.display().to_string(),
        })
        .unwrap_or_default();
        tracing::info!(program = %program.display(), "API: asking the person about a program");
        let answer = questions
            .ask(tilo_service::Question {
                handle: format!("api-program:{}", uuid::Uuid::new_v4().simple()),
                kind: PROGRAM_KIND.to_owned(),
                app_id: None,
                label,
                details,
            })
            .await;
        let chosen = answer.and_then(|answer| answer.choice).and_then(|choice| {
            tilo_action_types::Lifetime::chosen(choice, tilo_clock::unix_now_ms())
        });
        match chosen {
            None => Err(tilo_sheets::word("api-program-declined")),
            Some(tilo_action_types::Lifetime::Once | tilo_action_types::Lifetime::Until(_)) => {
                Ok(())
            }
            // The question offers nothing kept beyond this session: a yes until it is taken back
            // is the person's grant, which only the grants they can see and revoke keep.
            Some(
                tilo_action_types::Lifetime::Session | tilo_action_types::Lifetime::UntilRevoked,
            ) => {
                self.session.lock_recover().insert(program.to_owned());
                Ok(())
            }
        }
    }
}

/// Take the connections the person's own programs (the `tilo` command, the SDKs, scripts) make
/// at `path`, the API socket in the login's private directory, and hand each the person said yes
/// to to the daemon over `shell`, which serves Tilo's API to it as the person. The kernel's
/// account of the peer says which program it is: one of this login's own, never one a sandbox
/// confined though it is the same account, and `programs` decides whether the person lets that
/// program act as them. The socket is taken for as long as the session runs.
///
/// # Errors
/// The socket's directory is not this account's own, a live session answers at `path`, or the
/// thread that takes connections could not start.
pub(crate) fn serve_api(
    path: &std::path::Path,
    shell: runtime_collab::ShellClient,
    programs: Arc<Programs>,
) -> std::io::Result<()> {
    let listener = crate::mcp_server::bind_private(path)?;
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("tilo-api-socket".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "the API socket takes no connection");
                    return;
                }
            };
            let listener = {
                let _inside = runtime.enter();
                match tokio::net::UnixListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        tracing::error!(%error, "the API socket takes no connection");
                        return;
                    }
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, hand_api(listener, shell, programs));
        })?;
    Ok(())
}

/// Take connections on `listener`, handing each of this login's own programs the person said yes
/// to to the daemon's API.
async fn hand_api(
    listener: tokio::net::UnixListener,
    shell: runtime_collab::ShellClient,
    programs: Arc<Programs>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!(%error, "the API socket did not take a connection");
                continue;
            }
        };
        let facts = peer_facts::facts(&stream);
        let login = facts.as_ref().is_ok_and(|facts| {
            matches!(
                tilo_api::daemon::admission::SessionPeer::of(
                    facts,
                    rustix::process::geteuid().as_raw()
                ),
                tilo_api::daemon::admission::SessionPeer::Login { .. }
            )
        });
        if !login {
            tracing::warn!("API: a connection from another account or a sandbox is refused");
            continue;
        }
        let shell = shell.clone();
        let programs = Arc::clone(&programs);
        tokio::task::spawn_local(async move {
            // The program is its executable, as the kernel names it for the process the
            // connection's pidfd pins: one the kernel does not name is no program the person can
            // be asked about.
            let Some(program) = facts.ok().and_then(|facts| facts.exe) else {
                tracing::warn!("API: a program the kernel does not name is refused");
                refuse(stream, &tilo_sheets::word("api-program-unidentified")).await;
                return;
            };
            if let Err(why) = programs.admit(&program).await {
                tracing::info!(program = %program.display(), %why, "API: a program is not let act as the person");
                refuse(stream, &why).await;
                return;
            }
            let connection = match stream.into_std() {
                Ok(connection) => std::os::fd::OwnedFd::from(connection),
                Err(error) => {
                    tracing::error!(%error, "a program's API connection could not be handed over");
                    return;
                }
            };
            let program = runtime_collab::wire::shell::VouchedProgram { exe: program };
            if let Err(error) = shell.hand_api(connection, program).await {
                tracing::error!(%error, "the device's daemon did not take a program's API connection");
            }
        });
    }
}

/// Tell a program at the API socket why it is not served, as the API answers a refusal, then let
/// it go.
async fn refuse(mut stream: tokio::net::UnixStream, why: &str) {
    use tokio::io::AsyncWriteExt as _;
    let body = serde_json::json!({"error": {"code": "not_allowed", "message": why}}).to_string();
    let reply = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let told = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        stream.write_all(reply.as_bytes()).await?;
        stream.shutdown().await?;
        // What the program sent is read to its end before the connection closes: closing on
        // unread bytes resets it, and the program would lose the answer before reading it.
        tokio::io::copy(&mut stream, &mut tokio::io::sink())
            .await
            .map(drop)
    })
    .await;
    if !matches!(told, Ok(Ok(()))) {
        tracing::debug!("a program refused at the API socket was not told why");
    }
}

/// Bind `path` and take its connections with `accept` on a thread named `name`, until the socket
/// drops.
fn taking<F, Taken>(path: &std::path::Path, name: &str, accept: F) -> std::io::Result<AppSocket>
where
    F: FnOnce(tokio::net::UnixListener, tokio::sync::oneshot::Receiver<()>) -> Taken
        + Send
        + 'static,
    Taken: std::future::Future<Output = ()>,
{
    let socket = seat::AppSocket::bind(path)?;
    let listener = socket.copy()?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "the app socket takes no connection");
                    return;
                }
            };
            let listener = {
                let _inside = runtime.enter();
                match tokio::net::UnixListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        tracing::error!(%error, "the app socket takes no connection");
                        return;
                    }
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, accept(listener, stopped));
        })?;
    Ok(AppSocket {
        _socket: socket,
        stop: Some(stop),
    })
}

/// Take connections on `listener` until `stopped`, handing each to the daemon's identity service.
async fn hand_identity(
    listener: tokio::net::UnixListener,
    shell: runtime_collab::ShellClient,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        let stream = tokio::select! {
            _ = &mut stopped => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    tracing::warn!(%error, "the identity socket did not take a connection");
                    continue;
                }
            },
        };
        let shell = shell.clone();
        tokio::task::spawn_local(async move {
            let connection = match stream.into_std() {
                Ok(connection) => std::os::fd::OwnedFd::from(connection),
                Err(error) => {
                    tracing::error!(%error, "an app's identity connection could not be handed over");
                    return;
                }
            };
            if let Err(error) = shell.hand_identity(connection).await {
                // Error, not warning: the app signs nothing for this, and the default session log
                // keeps errors only.
                tracing::error!(%error, "the device's daemon did not take an app's identity connection");
            }
        });
    }
}

/// Take connections on `listener` until `stopped`, attesting and handing over each.
async fn accept(
    listener: tokio::net::UnixListener,
    launches: Arc<Launches>,
    shell: runtime_collab::ShellClient,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
) {
    let services: Arc<dyn tilo_broker_core::services::LauncherServices> = launches.clone();
    loop {
        let stream = tokio::select! {
            _ = &mut stopped => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    tracing::warn!(%error, "the app socket did not take a connection");
                    continue;
                }
            },
        };
        let (services, shell, launches) =
            (Arc::clone(&services), shell.clone(), Arc::clone(&launches));
        tokio::task::spawn_local(async move {
            let handed = match seat::attest(stream, services).await {
                seat::Attested::App {
                    connection,
                    app,
                    process,
                } => {
                    let name = format!("launch {}", app.ticket);
                    launches.recorded(app.ticket).await;
                    shell
                        .vouch(connection, Some((app, process)))
                        .await
                        .map_err(|error| (name, error))
                }
                seat::Attested::Unnamed(connection) => shell
                    .vouch(connection, None)
                    .await
                    .map_err(|error| (String::new(), error)),
                seat::Attested::Refused => return,
            };
            if let Err((app, error)) = handed {
                // Error, not warning: the app is about to end for this, and the default session
                // log keeps errors only.
                tracing::error!(%app, %error, "the device's daemon did not take an app's connection");
            }
        });
    }
}
