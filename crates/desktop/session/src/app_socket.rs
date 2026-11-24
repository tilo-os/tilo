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

use tilo_workspace_service::apps::collab_app_broker::seat;

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
    let launcher: Arc<dyn tilo_broker_core::services::LauncherServices> = launches;
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
        let (launcher, shell) = (Arc::clone(&launcher), shell.clone());
        tokio::task::spawn_local(async move {
            let handed = match seat::attest(stream, launcher).await {
                seat::Attested::App {
                    connection,
                    app,
                    process,
                    launcher,
                } => {
                    let name = app.app.clone();
                    shell
                        .vouch(connection, Some((app, process, launcher)))
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
