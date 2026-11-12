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
use tilo_actions::remote::host_account::Refusal;
use tilo_actions::remote::linux_apps::{OpenApp, UP_PROVIDER};

/// Why a workspace's Linux app did not open.
#[derive(Debug)]
pub enum AppRefused {
    /// The account's plan does not allow it: a Linux app is a Linux action.
    Refused(Refusal),
    /// Anything else, in words.
    Error(String),
}

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
    /// running, waiting while the host has no room. The session keeps its
    /// profile in `store`.
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

    /// Start the Linux app `asked` names in its workspace's VM, starting the
    /// workspace's apps session first when none runs, waiting while the host
    /// has no room. An account the plans do not let run Linux is refused in
    /// the plans' own words.
    pub fn open_app(&self, asked: &OpenApp) -> Result<OpenedBrowser, AppRefused> {
        let request = Request::OpenApp {
            app: control::OpenApp {
                workspace: asked.workspace.clone(),
                account: asked.account.clone(),
                size: self.size,
                material: asked.material.clone(),
                launch: asked.launch.clone(),
            },
        };
        let started = Instant::now();
        loop {
            match self.ask(&request).map_err(AppRefused::Error)? {
                Reply::Opened { opened_ms, .. } => {
                    return Ok(OpenedBrowser {
                        opened_ms,
                        provider: UP_PROVIDER.to_owned(),
                    });
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

    /// End the apps session in `workspace`'s VM now, and every app in it.
    pub fn stop_apps(&self, workspace: &str) -> Result<(), String> {
        match self.ask(&Request::StopApps {
            workspace: workspace.to_owned(),
        })? {
            Reply::Done => Ok(()),
            Reply::Error(message) => Err(message),
            other => Err(format!("the hosted-VM service answered {other:?}")),
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
