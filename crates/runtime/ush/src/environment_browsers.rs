//! A workspace's browser environment, opened on the member device that runs
//! the workspace's VMs (`docs/hosted-browser.md`, "Where it runs").
//!
//! A hosted workspace's Browser runs in a microVM of the workspace's own,
//! which this host's hosted-VM service (`legacy_vm::hosted`, the one that runs
//! the workspace's Linux VM) boots and keeps. So the member device answers the
//! relay's ask by asking that service over its socket
//! ([`Request::OpenBrowser`]), and waits while it says the host is full: an
//! idle environment says the same thing as a machine starting (a few
//! minutes), and a browser opens sooner than it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use legacy_vm::hosted::control::{self, Reply, Request};
use tilo_actions::remote::browser::{OpenBrowser, OpenedBrowser};

/// How long one answer is waited for: a boot and its session, when the host
/// has no spare to hand over.
const ANSWER_PATIENCE: Duration = Duration::from_secs(300);

/// How long a host with no room is waited for before it is reported.
const ROOM_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// How often a host with no room is asked again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// The hosted-VM service on this member, as the place browser environments
/// start.
#[derive(Clone, Debug)]
pub struct BrowserHost {
    socket: PathBuf,
}

impl BrowserHost {
    #[must_use]
    pub const fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Open `asked`'s workspace's browser environment, or attach to the one
    /// already serving it, waiting while the host has no room.
    pub fn open(&self, asked: &OpenBrowser) -> Result<OpenedBrowser, String> {
        let request = Request::OpenBrowser {
            browser: control::OpenBrowser {
                workspace: asked.workspace.clone(),
                account: asked.account.clone(),
                material: asked.material.clone(),
            },
        };
        let started = Instant::now();
        loop {
            let reply = self.ask(&request)?;
            match reply {
                Reply::Opened { opened_ms } => return Ok(OpenedBrowser { opened_ms }),
                // Every machine is full: a VM going idle, or another machine
                // starting, makes room. A browser is never refused for it.
                Reply::Busy(words) => {
                    if started.elapsed() >= ROOM_PATIENCE {
                        return Err(words);
                    }
                    std::thread::sleep(ASK_AGAIN);
                }
                Reply::Error(message) => return Err(message),
                other => return Err(format!("the hosted-VM service answered {other:?}")),
            }
        }
    }

    /// End `workspace`'s browser environment now.
    pub fn stop(&self, workspace: &str) -> Result<(), String> {
        match self.ask(&Request::StopBrowser {
            workspace: workspace.to_owned(),
        })? {
            Reply::Done => Ok(()),
            Reply::Error(message) => Err(message),
            other => Err(format!("the hosted-VM service answered {other:?}")),
        }
    }

    fn ask(&self, request: &Request) -> Result<Reply, String> {
        control::ask(&self.socket, request, ANSWER_PATIENCE)
            .map_err(|error| format!("the hosted-VM service: {error}"))?
            .ok_or_else(|| {
                format!(
                    "this host's Linux VM service is not running ({} is not there)",
                    self.socket.display()
                )
            })
    }
}
