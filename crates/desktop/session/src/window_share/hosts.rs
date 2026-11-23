//! How the windows that run on other devices stand on this session while nothing of them is shown.
//!
//! The workspace's layout lists a window that runs only on its device on every shell, shared or
//! not, and a shell that holds none of its pictures says why in the window's tab
//! ([`Elsewhere`]). Only a session can read that: whether the workspace says the window is
//! shared and it is still on its way here, whether its device is heard in the workspace, and
//! whether the workspace still admits that device. The shell draws what is read here and decides
//! none of it, and the shares' reading says the same of each such window in a word a tool
//! branches on (`hosted`).

use std::sync::Arc;
use std::time::Instant;

use iroh::EndpointId;
use serde_json::{Value, json};
use tilo_desktop_model::{TabId, TileContent};
use tilo_session_core::desktop::state::DesktopHandle;

use super::elsewhere::Elsewhere;
use super::peer::PeerViews as _;
use super::{WindowShares, device_key};
use crate::sync::Client;
use crate::workspace_view::WorkspaceView;

/// One window that runs on another device, as this session lists it, and how it stands here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HostedTab {
    /// The tab of this session the window stands in.
    tab: TabId,
    /// The workspace that tab stands in.
    workspace: String,
    /// The key of the device the window runs on.
    host: String,
    state: Elsewhere,
}

/// What this device knows of the device a window runs on, in the workspace the window stands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// It was heard in the workspace within the time a device goes unheard there.
    Heard,
    /// Nothing is heard of it, and the workspace admits it, or cannot be read.
    Unheard,
    /// Nothing is heard of it, and the workspace's governance admits other devices and not it.
    Unadmitted,
}

impl Host {
    /// Where the device `device` stands: among the devices `heard` in the workspace, or among
    /// the ones it `admitted` when last read. A workspace whose admitted devices could not be
    /// read names none, which says nothing against any device.
    fn of(device: EndpointId, heard: &[(EndpointId, String)], admitted: &[[u8; 32]]) -> Self {
        if heard.iter().any(|(known, _)| *known == device) {
            Self::Heard
        } else if admitted.is_empty() || admitted.contains(device.as_bytes()) {
            Self::Unheard
        } else {
            Self::Unadmitted
        }
    }
}

/// The word the reading gives a window's state: what a tool branches on, the same in every language.
const fn word(state: &Elsewhere) -> &'static str {
    match state {
        Elsewhere::Unread => "unread",
        Elsewhere::NotShared => "not-shared",
        Elsewhere::Awaited { .. } => "awaited",
        Elsewhere::Away => "away",
        Elsewhere::Left => "left",
    }
}

impl WindowShares {
    /// Read how every window that runs on another device stands on this session, and tell the shell
    /// where that changed: each tab says it from its next pass on. A session with no live session reads
    /// nothing of its workspaces, and its tabs say only where their windows run.
    pub(super) fn follow_hosted(
        &mut self,
        shell: &DesktopHandle,
        sync: Option<&Client>,
        view: Option<&Arc<WorkspaceView>>,
        now: Instant,
    ) {
        let listed: Vec<(TabId, String, String)> = shell.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|held| {
                    held.tabs().filter_map(move |tab| match &tab.tile {
                        TileContent::HostedWindow { host, .. } => {
                            Some((tab.id, held.policy.workspace.clone(), host.clone()))
                        }
                        _ => None,
                    })
                })
                .collect()
        });
        let read: Vec<HostedTab> = listed
            .into_iter()
            .map(|(tab, workspace, host)| {
                let state = sync.map_or(Elsewhere::Unread, |sync| {
                    self.state_of(sync, view, now, (tab, &workspace, &host))
                });
                HostedTab {
                    tab,
                    workspace,
                    host,
                    state,
                }
            })
            .collect();
        if read != self.elsewhere {
            shell.set_elsewhere(
                read.iter()
                    .map(|hosted| (hosted.tab, hosted.state.clone()))
                    .collect(),
            );
            self.elsewhere = read;
            self.changed();
        }
    }

    /// How the window this session lists in `tab` of `workspace`, which runs on the device `host`,
    /// stands while nothing of it is shown.
    fn state_of(
        &mut self,
        sync: &Client,
        view: Option<&Arc<WorkspaceView>>,
        now: Instant,
        (tab, workspace, host): (TabId, &str, &str),
    ) -> Elsewhere {
        // A window this device asks its host for is one the workspace says is shared, or one
        // watched by hand: its tab waits for it, and says what the share's own reading says it
        // waits for.
        let shared = self.shared_as(tab);
        if self.watching.contains_key(&(workspace.to_owned(), shared)) {
            let waiting_for = self
                .received
                .values()
                .find(|stream| stream.tab == shared)
                .map_or_else(
                    || self.watching_waits_for(workspace),
                    |stream| self.received_waits_for(stream),
                );
            return Elsewhere::Awaited { waiting_for };
        }
        // Nothing is read of a workspace this session takes no part in, of a host that is no
        // device, or of this device itself.
        let device = device_key(host).filter(|device| *device != sync.publisher());
        let (Some(device), Some(_)) = (device, sync.workspace_of(workspace)) else {
            return Elsewhere::Unread;
        };
        let heard = sync.workspace_devices(workspace);
        // The workspace's governance is asked who it admits only for a device nothing is heard
        // of, and no more often than a view is asked for again.
        let admitted = if Host::of(device, &heard, &[]) == Host::Heard {
            Vec::new()
        } else {
            self.admitted_to(view, workspace, now)
        };
        match Host::of(device, &heard, &admitted) {
            Host::Heard => Elsewhere::NotShared,
            Host::Unheard => Elsewhere::Away,
            Host::Unadmitted => Elsewhere::Left,
        }
    }

    /// The windows that run on other devices, as the reading says them: the tab each stands in on
    /// this session, its workspace and device, and how it stands. `shown` while a share of it stands
    /// in its tab; until then why it does not (`not-shared`, `awaited` with what it waits for,
    /// `away`, `left`), or `unread` where this session reads nothing of its workspace.
    pub(super) fn hosted_rows(&self) -> Vec<Value> {
        self.elsewhere
            .iter()
            .map(|hosted| {
                let shared = self.shared_as(hosted.tab);
                let shown = self
                    .received
                    .values()
                    .any(|stream| stream.tab == shared && stream.shown);
                let waiting_for = match &hosted.state {
                    Elsewhere::Awaited { waiting_for } if !shown => waiting_for.as_str(),
                    _ => "",
                };
                json!({
                    "tab": hosted.tab.to_string(),
                    "workspace": hosted.workspace,
                    "host": hosted.host,
                    "state": if shown { "shown" } else { word(&hosted.state) },
                    "waiting_for": waiting_for,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    tilo_test_env::scenario! {
        fn a_hosts_state_is_heard_and_has_a_word_of_its_own;

        phase a_device_is_heard_away_or_no_longer_admitted;
        // A device heard in the workspace is there, whatever the governance was last read to say; a
        // device nothing is heard of is away while the workspace admits it or cannot be read, and
        // has left once the workspace admits other devices and not it.
        {
            let (host, other) = (device(0x21), device(0x22));
            let heard = vec![(host, "Laptop".to_owned())];
            let admits = |device: EndpointId| vec![*device.as_bytes()];
            assert_eq!(Host::of(host, &heard, &[]), Host::Heard);
            assert_eq!(Host::of(host, &heard, &admits(other)), Host::Heard);
            assert_eq!(Host::of(host, &[], &[]), Host::Unheard);
            assert_eq!(Host::of(host, &[], &admits(host)), Host::Unheard);
            assert_eq!(Host::of(host, &[], &admits(other)), Host::Unadmitted);
        }

        phase every_state_has_a_word_of_its_own;
        // Every state goes by a word of its own in the reading.
        {
            let words = [
                Elsewhere::Unread,
                Elsewhere::NotShared,
                Elsewhere::Awaited {
                    waiting_for: String::new(),
                },
                Elsewhere::Away,
                Elsewhere::Left,
            ]
            .map(|state| word(&state));
            assert_eq!(words, ["unread", "not-shared", "awaited", "away", "left"]);
        }
    }
}
