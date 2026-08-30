//! Small app-side peer used by the agent's integration tests.
//!
//! The assertions in these tests predate the native collaboration service,
//! so the adapter deliberately retains their request/event-shaped driving
//! code while putting every byte on the production Cap'n Proto endpoint.

#![allow(
    dead_code,
    reason = "each integration binary uses a different adapter subset"
)]

use std::io;
use std::path::Path;
use std::time::Duration;

use qos_agent::wire::Link;
use ui::collaboration::{CollabEvent, CollabRequest};

pub struct TypedPeer {
    link: Option<Link>,
    path: std::path::PathBuf,
    timeout: Duration,
}

impl TypedPeer {
    pub fn connect(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            link: None,
            path: path.to_path_buf(),
            timeout: Duration::from_millis(200),
        })
    }

    pub fn announce(&mut self, path: &Path, request: &CollabRequest) -> anyhow::Result<()> {
        let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            handle,
        } = request
        else {
            anyhow::bail!("the first request must announce the peer route")
        };
        self.link = Some(Link::connect_endpoint(
            path,
            app_id,
            *cluster_id,
            record.clone(),
            handle.clone(),
        )?);
        Ok(())
    }

    pub fn send(&mut self, request: &CollabRequest) -> anyhow::Result<()> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .send(request)
    }

    pub fn recv(&mut self) -> anyhow::Result<Option<CollabEvent>> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .recv(self.timeout)
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.timeout = timeout.unwrap_or(Duration::from_millis(200));
        Ok(())
    }
}

pub fn send(peer: &mut TypedPeer, request: &CollabRequest) -> io::Result<()> {
    let result = if matches!(request, CollabRequest::Hello { .. }) {
        let path = peer.path.clone();
        peer.announce(&path, request)
    } else {
        peer.send(request)
    };
    result.map_err(io::Error::other)
}

pub fn recv(peer: &mut TypedPeer) -> Option<CollabEvent> {
    peer.recv()
        .expect("receive from typed collaboration service")
}

pub fn spawn(path: &Path) -> shell::shell::collab_broker::BrokerHandle {
    shell::shell::collab_broker::spawn_native(path.to_path_buf(), None)
        .expect("bind the typed collaboration service")
}
