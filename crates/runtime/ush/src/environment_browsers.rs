//! A workspace's Browser, opened in the workspace's VM on the member device
//! that runs it (`docs/hosted-browser.md`).
//!
//! The member device answers the relay's ask by asking this host's hosted-VM
//! service (`legacy_vm::hosted`, the one that runs the workspace's VM) over
//! its socket ([`Request::OpenBrowser`]) to start the browser session in that
//! VM, and waits while it says the host is full.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::size::VmSize;
use tilo_actions::remote::browser::{OpenBrowser, OpenedBrowser};

/// How long one answer is waited for: a wake and the session starting.
const ANSWER_PATIENCE: Duration = Duration::from_secs(300);

/// How long a host with no room is waited for before it is reported.
const ROOM_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// How often a host with no room is asked again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// The hosted-VM service on this member, as the place the workspaces'
/// Browser starts, in their VMs of `size`.
#[derive(Clone, Debug)]
pub struct BrowserHost {
    socket: PathBuf,
    size: VmSize,
}

impl BrowserHost {
    #[must_use]
    pub const fn new(socket: PathBuf, size: VmSize) -> Self {
        Self { socket, size }
    }

    /// Open `asked`'s workspace's Browser, or attach to the session already
    /// running, waiting while the host has no room.
    pub fn open(&self, asked: &OpenBrowser) -> Result<OpenedBrowser, String> {
        let request = Request::OpenBrowser {
            browser: control::OpenBrowser {
                workspace: asked.workspace.clone(),
                account: asked.account.clone(),
                size: self.size,
                material: asked.material.clone(),
            },
        };
        let started = Instant::now();
        loop {
            let reply = self.ask(&request)?;
            match reply {
                Reply::Opened {
                    opened_ms,
                    provider,
                } => {
                    return Ok(OpenedBrowser {
                        opened_ms,
                        provider,
                    });
                }
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

    /// End the browser session in `workspace`'s VM now.
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
