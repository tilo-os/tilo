//! Joining the workspace a host set this session up for, with nobody sitting at it: a provider's
//! guest redeems one invitation at startup through the same join the person's own pasted code
//! takes, and waits for the authority the host's operator grants it before it serves agents.
//! Nothing happens when the host set nothing: the variables are only what an image that runs a
//! guest session sets.

use std::path::Path;
use std::time::{Duration, Instant};

use tilo_desktop_surface::DesktopHandle;

use crate::directory;

/// The variable naming a file that holds one invitation text to redeem at startup.
pub const INVITATION_ENV: &str = "TILO_SHELL_JOIN_INVITATION_FILE";
/// The variable a host sets when its operator makes this session an administrator of the workspace
/// once it has joined: the session then waits for that authority before it serves agents, whose
/// first acts may need it.
pub const ADMINISTRATOR_ENV: &str = "TILO_SHELL_JOIN_ADMINISTRATOR";
/// How long the host may take to make this session an administrator.
const ADMIN_DEADLINE: Duration = Duration::from_secs(60);

/// The join a session starts with, and the authority it waits for.
pub(crate) struct BootJoin {
    /// The workspace the invitation joins; `None` when there was no invitation, or it was not one
    /// for this computer.
    workspace: Option<String>,
    grant: Grant,
}

/// Whether this session may serve agents yet.
enum Grant {
    /// The host asked for no authority here: agents are served from the first pass.
    NotAsked,
    /// Waiting for the authority, at most until then.
    Waiting { until: Instant },
    /// Granted, or never granted: agents are served.
    Served,
}

impl crate::Shared {
    /// A session the host started for one workspace joins it on its own, and withholds its agents
    /// until the host's operator makes it an administrator of that workspace.
    pub(crate) fn follow_boot_join(&self) {
        self.boot_join
            .borrow_mut()
            .follow_session(&self.handle, &self.person);
    }
}

impl BootJoin {
    /// What this session was started to join, as [`Self::start`] takes it from the desk the login
    /// opens on.
    pub(crate) fn at_boot(directory: &directory::Store, handle: &DesktopHandle) -> Self {
        Self::start(directory, Instant::now(), |workspace| {
            handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .any(|held| held.id().to_string() == workspace)
            })
        })
    }

    /// [`Self::follow`] against this session's desk: whether it may administer the workspace it
    /// joined.
    pub(crate) fn follow_session(&mut self, handle: &DesktopHandle, person: &str) {
        let now = tilo_clock::unix_now();
        self.follow(Instant::now(), |workspace| {
            handle.read(|tree| {
                tree.workspaces.iter().any(|held| {
                    held.id().to_string() == workspace
                        && held.may_govern_at(
                            person,
                            tilo_action_policy::roles::governance::GRANT,
                            now,
                        )
                })
            })
        });
    }

    /// What this session was started to join: the invitation in [`INVITATION_ENV`], handed to
    /// `directory` to redeem as the person's own pasted code is, and whether [`ADMINISTRATOR_ENV`]
    /// has it wait. `held` answers whether the desk already holds the workspace — a desktop that
    /// restarts keeps its membership, so its invitation is not redeemed again.
    pub(crate) fn start(
        directory: &directory::Store,
        now: Instant,
        held: impl FnOnce(&str) -> bool,
    ) -> Self {
        let Some(path) = std::env::var_os(INVITATION_ENV) else {
            return Self::none();
        };
        let path = Path::new(&path);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "boot join: the invitation was not read");
                return Self::none();
            }
        };
        // A short link carries where its invitation waits, not what it grants: it is redeemed like
        // a pasted one, and the workspace it joins is learned from what the site answers with.
        let sealed = tilo_workspace_join::sealed::parse_link(&text).is_some();
        let workspace = if sealed {
            None
        } else {
            match crate::platform::directory::invited_workspace(&text) {
                Ok(workspace) => Some(workspace),
                Err(error) => {
                    tracing::warn!(%error, "boot join: the invitation is not for this computer");
                    None
                }
            }
        };
        match workspace.as_deref() {
            Some(workspace) if held(workspace) => {
                tracing::info!(%workspace, "boot join: this computer is already a member");
            }
            Some(workspace) => match crate::platform::directory::take(directory, &text) {
                Ok(()) => tracing::info!(%workspace, "boot join: redeeming the invitation"),
                Err(error) => {
                    tracing::warn!(%workspace, %error, "boot join: the invitation was not redeemed");
                }
            },
            None if sealed => match crate::platform::directory::take(directory, &text) {
                Ok(()) => tracing::info!("boot join: fetching the invitation its link names"),
                Err(error) => {
                    tracing::warn!(%error, "boot join: the link's invitation was not fetched");
                }
            },
            None => {}
        }
        // A short link's workspace is known only once its site answers, but the host asked all the
        // same: the wait starts, and asks about the workspace as soon as the join names it
        // ([`Self::joined`]).
        let administrator =
            std::env::var_os(ADMINISTRATOR_ENV).is_some() && (workspace.is_some() || sealed);
        Self {
            workspace,
            grant: if administrator {
                Grant::Waiting {
                    until: now.checked_add(ADMIN_DEADLINE).unwrap_or_else(Instant::now),
                }
            } else {
                Grant::NotAsked
            },
        }
    }

    /// No invitation: the session serves its agents as any other.
    const fn none() -> Self {
        Self {
            workspace: None,
            grant: Grant::NotAsked,
        }
    }

    /// Whether the session withholds agents while the authority it waits for is on its way.
    pub(crate) fn waiting(&self) -> bool {
        matches!(self.grant, Grant::Waiting { .. })
    }

    /// The workspace the invitation joined, once the join names it: a short link's invitation is
    /// fetched at startup, so what it grants arrives after the session has started waiting for the
    /// host's authority.
    pub(crate) fn joined(&mut self, workspace: &str) {
        if self.workspace.is_none() {
            self.workspace = Some(workspace.to_owned());
        }
    }

    /// Whether this session may administer its workspace now, and with that serve its agents:
    /// `may_administer` answers whether it may.
    pub(crate) fn follow(&mut self, now: Instant, may_administer: impl FnOnce(&str) -> bool) {
        let Grant::Waiting { until } = self.grant else {
            return;
        };
        // Nothing to ask about until the join names its workspace: a host's authority is waited
        // for, no longer than it is asked of any join.
        let Some(workspace) = self.workspace.clone() else {
            if now >= until {
                tracing::warn!("boot join: the join named no workspace to administer");
                self.grant = Grant::Served;
            }
            return;
        };
        let workspace = workspace.as_str();
        if may_administer(workspace) {
            tracing::info!(%workspace, "boot join: administering the workspace");
            self.grant = Grant::Served;
        } else if now >= until {
            tracing::warn!(
                %workspace,
                "boot join: the host did not make this session an administrator"
            );
            self.grant = Grant::Served;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(now: Instant) -> BootJoin {
        BootJoin {
            workspace: Some("ws-1".into()),
            grant: Grant::Waiting {
                until: now.checked_add(ADMIN_DEADLINE).unwrap_or(now),
            },
        }
    }

    #[test]
    fn a_session_the_host_administers_waits_for_that_before_it_serves_agents() {
        let now = Instant::now();
        let mut join = waiting(now);
        assert!(join.waiting(), "agents wait while it is on its way");
        join.follow(now + Duration::from_secs(1), |workspace| {
            assert_eq!(
                workspace, "ws-1",
                "the workspace it joined is the one asked about"
            );
            false
        });
        assert!(join.waiting());
        join.follow(now + Duration::from_secs(2), |_| true);
        assert!(
            !join.waiting(),
            "served once it may administer the workspace"
        );
    }

    #[test]
    fn the_host_that_never_grants_it_does_not_withhold_agents_for_good() {
        let now = Instant::now();
        let mut join = waiting(now);
        join.follow(now + ADMIN_DEADLINE, |_| false);
        assert!(!join.waiting(), "the wait ends on its own");
    }

    #[test]
    fn a_short_links_workspace_is_asked_about_once_the_join_names_it() {
        let now = Instant::now();
        // The link carries where its invitation waits, not what it grants: the session waits
        // without a workspace to ask about.
        let mut join = BootJoin {
            workspace: None,
            grant: Grant::Waiting {
                until: now.checked_add(ADMIN_DEADLINE).unwrap_or(now),
            },
        };
        let mut asked = 0;
        join.follow(now + Duration::from_secs(1), |_| {
            asked += 1;
            true
        });
        assert_eq!(asked, 0, "no workspace is asked about before the join");
        assert!(
            join.waiting(),
            "and the host's authority is still waited for"
        );
        join.joined("ws-2");
        join.follow(now + Duration::from_secs(2), |workspace| {
            assert_eq!(workspace, "ws-2", "the workspace the join landed in");
            true
        });
        assert!(!join.waiting(), "served once the host granted it");
    }

    #[test]
    fn a_session_the_host_set_nothing_up_for_serves_its_agents_at_once() {
        let mut join = BootJoin::none();
        assert!(!join.waiting());
        join.follow(Instant::now(), |_| false);
        assert!(!join.waiting());
    }
}
