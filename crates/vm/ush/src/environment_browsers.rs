//! A workspace's Browser, opened in the workspace's VM on the member device
//! that runs it (`docs/hosted-browser.md`).
//!
//! The member device answers the server's ask by asking this host's hosted-VM
//! service (`linux_vm::hosted`, the one that runs the workspace's VM) over
//! its socket ([`Request::OpenBrowser`]) to start the device daemon in that
//! VM, and waits while it says the host is full. What runs in the VM is then
//! the daemon's: the machine asks it to open its windows.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use linux_vm::hosted::control::{self, Reply, Request};
use tilo_actions::remote::browser::{OpenBrowser, OpenedBrowser};
use tilo_actions::remote::linux_apps::OpenApp;
use tilo_actions::remote::server_account::Refusal;
use tilo_server_protocol::hosted_vm::VmSize;

/// Why a workspace's Linux app did not open.
#[derive(Debug)]
pub enum AppRefused {
    /// The account's plan does not allow it: a Linux app is a Linux action.
    Refused(Refusal),
    Error(String),
}

const ANSWER_PATIENCE: Duration = Duration::from_secs(300);

const ROOM_PATIENCE: Duration = Duration::from_secs(10 * 60);

const ASK_AGAIN: Duration = Duration::from_secs(2);

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

    /// Start `asked`'s workspace's VM daemon for its Browser, or find it
    /// running, waiting while the host has no room. Browser keeps its profile
    /// in `store`.
    pub fn open(&self, asked: &OpenBrowser, store: PathBuf) -> Result<OpenedBrowser, String> {
        let request = Request::OpenBrowser {
            browser: control::OpenBrowser {
                workspace: asked.workspace.clone(),
                account: asked.account.clone(),
                size: self.size,
                store: Some(store),
                material: asked.material.clone(),
            },
        };
        let started = Instant::now();
        loop {
            let reply = self.ask(&request)?;
            match reply {
                Reply::Opened { opened_ms, device } => {
                    return Ok(OpenedBrowser { opened_ms, device });
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

    /// Start `asked`'s workspace's VM daemon for its Linux apps, or find it
    /// running, waiting while the host has no room. An account the plans do not let run Linux is refused in
    /// the plans' own words.
    pub fn open_app(&self, asked: &OpenApp) -> Result<OpenedBrowser, AppRefused> {
        let request = Request::OpenApp {
            app: control::OpenApp {
                workspace: asked.workspace.clone(),
                account: asked.account.clone(),
                size: self.size,
                material: asked.material.clone(),
            },
        };
        let started = Instant::now();
        loop {
            match self.ask(&request).map_err(AppRefused::Error)? {
                Reply::Opened { opened_ms, device } => {
                    return Ok(OpenedBrowser { opened_ms, device });
                }
                Reply::Refused(refusal) => return Err(AppRefused::Refused(refusal)),
                Reply::Busy(words) => {
                    if started.elapsed() >= ROOM_PATIENCE {
                        return Err(AppRefused::Error(words));
                    }
                    std::thread::sleep(ASK_AGAIN);
                }
                Reply::Error(message) => return Err(AppRefused::Error(message)),
                other => {
                    return Err(AppRefused::Error(format!(
                        "the hosted-VM service answered {other:?}"
                    )));
                }
            }
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
