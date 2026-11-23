//! The `messaging` session socket.
//!
//! Only the person's own processes reach it (the peer's uid must be the
//! service's), and each connection first carries the handoff the launcher
//! writes for the app it is handing it to, before the app holds it. Apps in
//! their sandbox cannot reach the socket path; the launcher is what names
//! them, as for the model gateway. Only Messages is served.

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::Duration;

use super::{AppHandler, Service};
use crate::wire::{AppLink, HANDOFF_FRAMING, Handoff, PROTOCOL};

/// How long a connection may take to name its app.
const HANDOFF: Duration = Duration::from_secs(5);

/// Listen on the session's `messaging` socket and serve every app that
/// connects, on a thread of its own.
///
/// # Errors
/// The socket cannot be bound.
pub fn listen(service: &Arc<Service>) -> std::io::Result<()> {
    let path = launcher::services::socket_for(crate::SERVICE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = tilo_files::native::private::bind_socket(&path)?;
    let service = Arc::downgrade(service);
    std::thread::Builder::new()
        .name("tilo-messaging-accept".into())
        .spawn(move || accept(&listener, &service))?;
    Ok(())
}

fn accept(listener: &UnixListener, service: &std::sync::Weak<Service>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let Some(current) = service.upgrade() else {
            return;
        };
        // A connection that never names its app holds no other one up.
        let serving = Arc::downgrade(&current);
        let _ = std::thread::Builder::new()
            .name("tilo-messaging-handoff".into())
            .spawn(move || {
                if admit(&stream)
                    && let Some(service) = serving.upgrade()
                {
                    serve(&service, stream);
                }
            });
    }
}

/// Whether a connection is the person's own and names Messages, the one app
/// that holds messaging accounts.
fn admit(stream: &UnixStream) -> bool {
    let admitted = || {
        let peer = rustix::net::sockopt::socket_peercred(stream).ok()?;
        if peer.uid != rustix::process::geteuid() {
            return None;
        }
        stream.set_read_timeout(Some(HANDOFF)).ok()?;
        let mut reader = stream;
        let body = HANDOFF_FRAMING.read(&mut reader).ok()??;
        let handoff: Handoff = serde_json::from_slice(&body).ok()?;
        stream.set_read_timeout(None).ok()?;
        Some(handoff.protocol == PROTOCOL && handoff.app_id == crate::MESSAGES_APP)
    };
    admitted() == Some(true)
}

fn serve(service: &Arc<Service>, stream: UnixStream) {
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let handler = Arc::new(AppHandler {
        service: Arc::downgrade(service),
    });
    if let Ok(link) = AppLink::start("tilo-messaging-app", reading, stream, handler) {
        service.serve(link);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_names_messages_or_is_closed() {
        for (app, admitted) in [
            (crate::MESSAGES_APP, true),
            ("so.tilo.passwords", false),
            ("so.tilo.notes", false),
        ] {
            let (mut app_end, service_end) = UnixStream::pair().unwrap();
            crate::client::write_handoff(&mut app_end, app).unwrap();
            assert_eq!(admit(&service_end), admitted, "{app}");
        }
        let (app_end, service_end) = UnixStream::pair().unwrap();
        drop(app_end);
        assert!(!admit(&service_end));
    }
}
