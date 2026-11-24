//! The device's own place in one workspace's live session, held by its daemon.
//!
//! The daemon owns the device's endpoint, and the live session a device takes part in is one
//! `moq-lite` client of the workspace's relay. Holding it here, beside the relay of the
//! workspaces this device serves, is what lets a device with no desktop session — a server —
//! take part in its workspaces' calls and presence: an app's call lane joins through this
//! session, and the person's desktop drives its capture and playout through the same one over
//! its link. There is one place a device is in a live session, wherever it runs.
//!
//! The session is sealed under the workspace's media channel keys, which the daemon's key
//! service holds, and it reads what each workspace's calls allow from the workspace's own
//! governance. A call's own media is sealed under the keys of the call's rule generation, which
//! only the devices the call admits are given: the session asks for them as its presence moves
//! into a call, and seals under them once they are here.

use std::collections::BTreeMap;
use std::sync::Arc;

use collab_types::presence::CallPresence;
use collab_types::scope::Scope;
use tilo_desktop_model::WorkspaceId;
use tilo_live::calls::CallRule;
use tilo_live::frame::LiveKeys;
use tilo_live::grant;
use tilo_live::live_session::{Join, LiveError, LiveSession};
use tilo_live::native::Iroh;
use tilo_live::presence::{Call, Own};
use tilo_live::relay::Relay;
use tilo_live::roster::Roster;
use tilo_live::speaker::Speaker;
use tilo_session_client::workspace_keys::Device;
use tilo_workspace_service::workspaces::Workspaces;

/// One workspace's live session as this device's daemon holds it: the one place this device is
/// in the workspace's call and presence.
pub(crate) struct CallSession {
    workspace: WorkspaceId,
    device: [u8; 32],
    /// The device that relays the workspace's live session.
    relay: [u8; 32],
    /// The epoch the session seals under, as its keys moved.
    epoch: std::sync::atomic::AtomicU64,
    session: Arc<LiveSession>,
    /// The device's one word in the workspace: what the person's shell, the device's apps, a call
    /// it joined and a bridge say, said on `session`.
    speaker: Arc<Speaker>,
}

impl CallSession {
    /// Join `workspace`'s live session on `relay`, the one this device serves or the one its
    /// keeping runs, as this device, sealed under `keys` and reading `roster`.
    pub(crate) async fn join(
        local: Option<&Relay>,
        away: Option<iroh::EndpointAddr>,
        endpoint: &iroh::Endpoint,
        certificate: &tilo_user_identity::DeviceCert,
        relay: [u8; 32],
        device: [u8; 32],
        workspace: WorkspaceId,
        keys: LiveKeys,
        roster: Roster,
        base: Own,
    ) -> Result<Self, LiveError> {
        let scope = Scope::workspace(workspace.to_string());
        let epoch = keys.epoch();
        let join = Join {
            scope: scope.clone(),
            device,
            keys,
            roster,
            publish: true,
            presence: true,
        };
        let session = match (local, away) {
            (Some(local), _) => LiveSession::local(local, &join)?,
            (None, Some(addr)) => {
                let certificate =
                    serde_json::to_vec(certificate).map_err(|_| LiveError::Invalid)?;
                let grant = grant::Native::new(
                    *workspace.as_uuid().as_bytes(),
                    &scope.id(),
                    epoch,
                    certificate,
                    Vec::new(),
                )
                .map_err(|_| LiveError::Invalid)?;
                let dial = Arc::new(Iroh::new(endpoint.clone(), addr));
                LiveSession::join(dial, &grant.encode(), join).await?
            }
            (None, None) => return Err(LiveError::Unavailable),
        };
        let session = Arc::new(session);
        let speaker = Speaker::start(&session, workspace.to_string(), device, base);
        Ok(Self {
            workspace,
            device,
            relay,
            epoch: std::sync::atomic::AtomicU64::new(epoch),
            session,
            speaker,
        })
    }

    /// The device that relays this workspace's live session.
    pub(crate) fn relay(&self) -> [u8; 32] {
        self.relay
    }

    /// The epoch the session seals under now.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The live session itself: what media and presence are published on and heard from.
    pub(crate) fn session(&self) -> &LiveSession {
        &self.session
    }

    /// A handle to the live session, for what carries a desktop's link into it.
    pub(crate) fn shared(&self) -> Arc<LiveSession> {
        Arc::clone(&self.session)
    }

    /// The device's one word in the workspace.
    pub(crate) fn speaker(&self) -> &Arc<Speaker> {
        &self.speaker
    }

    /// Join `call`, or leave the one the device is in (`None`), as `gate` lets this device: said
    /// at once, and answered with the call's channel and its rule's generation once this device
    /// holds that generation's media keys.
    ///
    /// # Errors
    /// The call lets this device do nothing, or its keys did not arrive.
    pub(crate) async fn enter_call(
        &self,
        workspaces: &Workspaces,
        call: Option<CallPresence>,
        gate: &CallGate,
    ) -> Result<Option<(String, u64)>, String> {
        let Some(call) = call else {
            self.speaker.set_joined(None);
            return Ok(None);
        };
        let (rules, persons) = gate;
        let may = tilo_live::calls::may(&call.channel, rules, persons, self.device);
        if !may.publish && !may.subscribe {
            return Err(format!(
                "the call in {} takes this device in no more",
                call.channel
            ));
        }
        let channel = call.channel.clone();
        self.speaker.set_joined(Some(Call {
            call,
            camera: false,
            screen: false,
            muted: !may.publish,
            deafened: !may.subscribe,
        }));
        self.hand_call_keys(workspaces).await;
        match self.session.call_keys_wanted() {
            Some((wanted, generation)) if wanted == channel => Ok(Some((wanted, generation))),
            _ => Err(format!("the call in {channel} is not this device's")),
        }
    }

    /// Whether the session ended, as a carrier that went and did not come back leaves it.
    pub(crate) fn ended(&self) -> bool {
        self.session.ended()
    }

    /// What this device seals and opens with now, and who may send in it: the workspace's keys
    /// and devices may have moved under a session that is still carried.
    pub(crate) fn refresh(&self, keys: LiveKeys, devices: &[Device], rules: CallGate) {
        self.epoch
            .store(keys.epoch(), std::sync::atomic::Ordering::Relaxed);
        self.session.set_keys(keys);
        self.session
            .set_roster(Roster::new(devices.iter().map(|device| device.device)));
        let (rules, persons) = rules;
        self.session.set_calls(rules, persons);
    }

    /// Hand the session the keys of the call it says this device is in when this device holds
    /// them: the key group of the call's rule generation, which only the devices the call admits
    /// are given. Asking takes this device into the group; a generation without keys here asks
    /// the workspace's key service, and the answer arrives as any key answer does.
    pub(crate) async fn hand_call_keys(&self, workspaces: &Workspaces) {
        let Some((channel, generation)) = self.session.call_keys_wanted() else {
            return;
        };
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: collab_types::scope::MEDIA_APP.to_owned(),
            cluster_id: self.workspace.to_string(),
            document: collab_types::calls::media_document(&channel, generation),
        };
        if let Some(Ok(keys)) = workspaces.own_route_keys(&route, true).await {
            self.session.set_call_keys(&channel, generation, keys);
        }
    }
}

impl std::fmt::Debug for CallSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallSession")
            .field("workspace", &self.workspace)
            .field("device", &hex::encode(self.device))
            .field("ended", &self.ended())
            .finish_non_exhaustive()
    }
}

/// What a workspace's calls allow, by channel, and the person each of its member devices is, as
/// the workspace's governance says them: what a live session holds its call media to.
pub(crate) type CallGate = (BTreeMap<String, CallRule>, BTreeMap<[u8; 32], String>);

/// What the person's shell says of them, as the device's word carries it.
pub(crate) fn own_of(word: runtime_collab::wire::shell::ShellWord) -> Own {
    Own {
        label: word.label,
        device: word.device,
        user: word.user,
        profile_ms: word.profile_ms,
        status: word
            .status
            .map(|(text, automatic)| tilo_live::presence::Status { text, automatic }),
        availability: word.availability,
        call: word.call.map(call_of),
        following: word.following,
        wave: word.wave,
        pointer: word
            .pointer
            .map(|(tab, x, y)| tilo_live::presence::Point { tab, x, y }),
        shell: word.shell,
        bridged: Vec::new(),
        apps: word.apps,
    }
}

/// A call the person's shell says they are in, as the device's word carries it.
pub(crate) fn call_of(call: runtime_collab::wire::shell::ShellInCall) -> Call {
    Call {
        call: call.call,
        camera: call.camera,
        screen: call.screen,
        muted: call.muted,
        deafened: call.deafened,
    }
}
