//! Coalesced session-local views. The caller supplies accepted ledger winners
//! and a current, already enrolled personal route; raw wire entries never enter.
use super::{AppInstanceId, LaunchIntent, ResumeMetadata, RoomId};
use crate::shell::network::AuthenticatedPersonalRoute;
use crate::shell::personal_continuity::ContinuityStamp;
use collab_types::handoff::{OwnerProjection, OwnerView};
use std::collections::BTreeMap;

type Key = (RoomId, String, AppInstanceId);
#[derive(Clone, Debug)]
struct LiveView {
    stamp: ContinuityStamp,
    projection: Option<OwnerProjection>,
    route: Option<AuthenticatedPersonalRoute>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct OwnerViews {
    live: BTreeMap<Key, LiveView>,
    pending: BTreeMap<Key, (OwnerView, Option<AuthenticatedPersonalRoute>)>,
    generations: BTreeMap<Key, u64>,
    acknowledgements: BTreeMap<Key, (OwnerView, Option<AuthenticatedPersonalRoute>)>,
    pub(super) claims: BTreeMap<Key, u64>,
    claim_tokens: BTreeMap<Key, u64>,
    pub(super) revision: u64,
}

impl OwnerViews {
    pub(super) fn offer(
        &mut self,
        key: Key,
        stamp: ContinuityStamp,
        metadata: &ResumeMetadata,
        route: Option<AuthenticatedPersonalRoute>,
    ) {
        if !self.generations.contains_key(&key) && self.generations.len() >= 128 {
            return;
        }
        let metadata = ui::collaboration::ContinuationMetadata {
            schema_version: metadata.schema_version(),
            bytes: metadata.bytes().to_vec(),
        };
        let projection =
            ui::app::semantic_continuation_projection(&metadata).map(|metadata| OwnerProjection {
                schema_version: metadata.schema_version,
                bytes: metadata.bytes,
            });
        if self.live.get(&key).is_some_and(|last| {
            (last.stamp.counter, last.stamp.device.as_str())
                > (stamp.counter, stamp.device.as_str())
                || (last.stamp == stamp && last.projection == projection && last.route == route)
        }) {
            return;
        }
        let view = LiveView {
            stamp,
            projection,
            route,
        };
        self.enqueue(key.clone(), &view, view.route.is_none());
        self.live.insert(key, view);
    }

    fn enqueue(&mut self, key: Key, view: &LiveView, local_claim: bool) {
        if !self.generations.contains_key(&key) && self.generations.len() >= 128 {
            return;
        }
        let generation = self.generations.entry(key.clone()).or_default();
        *generation = generation.saturating_add(1);
        self.pending.insert(
            key.clone(),
            (
                OwnerView {
                    room: key.0.to_string(),
                    app: key.1,
                    instance: key.2.0,
                    generation: *generation,
                    counter: view.stamp.counter,
                    source: view.stamp.device.as_str().to_owned(),
                    projection: view.projection.clone(),
                    local_claim,
                },
                view.route.clone(),
            ),
        );
    }

    pub(super) fn stop_unless(&mut self, allowed: impl Fn(&AuthenticatedPersonalRoute) -> bool) {
        let stopped: Vec<_> = self
            .live
            .iter()
            .filter_map(|(key, view)| {
                view.route
                    .as_ref()
                    .filter(|route| !allowed(route))
                    .map(|_| key.clone())
            })
            .collect();
        for key in stopped {
            if let Some(mut view) = self.live.remove(&key) {
                view.projection = None;
                view.route = None;
                self.enqueue(key, &view, false);
            }
        }
    }

    pub(super) fn refresh(&mut self, room: RoomId, app: &str, instance: AppInstanceId) {
        let key = (room, app.to_owned(), instance);
        if let Some(view) = self.live.get(&key).cloned() {
            self.enqueue(key, &view, view.route.is_none());
        }
    }

    pub(super) fn retain_candidates(&mut self, keys: &std::collections::BTreeSet<Key>) {
        let stopped: Vec<_> = self
            .live
            .keys()
            .filter(|key| !keys.contains(*key))
            .cloned()
            .collect();
        for key in stopped {
            if let Some(mut view) = self.live.remove(&key) {
                view.projection = None;
                view.route = None;
                self.enqueue(key, &view, false);
            }
        }
    }

    pub(super) fn can_claim(&self, key: &Key, token: u64) -> Result<bool, String> {
        if self
            .claim_tokens
            .get(key)
            .is_some_and(|last| *last >= token)
        {
            return Ok(false);
        }
        if (!self.claim_tokens.contains_key(key) && self.claim_tokens.len() >= 128)
            || (!self.generations.contains_key(key) && self.generations.len() >= 128)
        {
            return Err("owner takeover queue is full".into());
        }
        Ok(true)
    }

    pub(super) fn claim(&mut self, key: Key, token: u64) {
        if self
            .claim_tokens
            .get(&key)
            .is_some_and(|last| *last >= token)
        {
            return;
        }
        // Reserve the acknowledgement slot before another offered view can take it.
        self.generations.entry(key.clone()).or_default();
        self.claim_tokens.insert(key.clone(), token);
        self.revision = self.revision.saturating_add(1);
        self.claims.insert(key, self.revision);
    }

    pub(super) fn acknowledge(
        &mut self,
        revision: u64,
        snapshot: &crate::shell::personal_continuity::DeviceContinuationSnapshot,
    ) {
        for workspace in &snapshot.workspaces {
            for app in &workspace.apps {
                let LaunchIntent::App { app_id } = &app.launch else {
                    continue;
                };
                let key = (
                    workspace.room,
                    app_id.clone(),
                    app.record_instance.unwrap_or(app.id),
                );
                if self.claims.get(&key).is_none_or(|claim| *claim > revision) {
                    continue;
                }
                let Some(resume) = app.resume.as_ref() else {
                    continue;
                };
                self.claims.remove(&key);
                self.live.remove(&key);
                self.offer(key.clone(), app.stamp.clone(), resume, None);
                if let Some(ack) = self.pending.remove(&key) {
                    self.acknowledgements.insert(key, ack);
                }
            }
        }
    }

    pub(super) fn forget(&mut self, room: RoomId, app: &str, instance: AppInstanceId) {
        let key = (room, app.to_owned(), instance);
        self.live.remove(&key);
        self.pending.remove(&key);
        self.acknowledgements.remove(&key);
        self.generations.remove(&key);
        self.claims.remove(&key);
        self.claim_tokens.remove(&key);
    }

    pub(super) fn drain(&mut self) -> Vec<(OwnerView, Option<AuthenticatedPersonalRoute>)> {
        // A concurrent remote winner must follow the local durable ack,
        // even when it has the same counter and a greater device tie-break.
        std::mem::take(&mut self.acknowledgements)
            .into_values()
            .chain(std::mem::take(&mut self.pending).into_values())
            .collect()
    }
}
