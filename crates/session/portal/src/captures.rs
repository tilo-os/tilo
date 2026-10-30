//! What is captured right now, as the person sees and stops it.
//!
//! Every live capture lease of the session's [`CaptureBroker`] (a portal
//! screen share's streams, a call's camera and microphone) is published
//! with an action that stops it: the privacy indicator reads these, and
//! stopping one revokes the lease, stops its stream and ends the portal
//! session it belonged to, telling the app.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};

use crate::PortalCaller;
use crate::capture_broker::{
    CaptureBroker, CaptureBrokerError, CaptureKind, CaptureLeaseId, CaptureSessionBinding,
    LocalCaptureHandle,
};
use crate::session::Sessions;

const CAPTURE_PREFIX: &str = "portal:capture:";
/// The target a portal session's leases are bound to: this device.
const PORTAL_TARGET: &str = "portal";

/// What a live capture offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.capture")]
pub enum CaptureEffect {
    /// Stop capturing
    #[effect(own_device, idempotent)]
    Stop,
}

/// The leases a portal session holds, and the sources registered for them.
struct Held {
    binding: CaptureSessionBinding,
    sources: Vec<String>,
}

/// The session's capture broker and the portal sessions' leases in it.
#[derive(Clone)]
pub struct Captures {
    broker: Arc<Mutex<CaptureBroker>>,
    held: Arc<Mutex<HashMap<String, Held>>>,
    sessions: Sessions,
}

impl Captures {
    /// Leases in a broker of their own, ending portal sessions in
    /// `sessions` when theirs are revoked.
    #[must_use]
    pub fn new(sessions: &Sessions) -> Self {
        let captures = Self {
            broker: Arc::new(Mutex::new(CaptureBroker::new())),
            held: Arc::default(),
            sessions: sessions.clone(),
        };
        let ended = captures.clone();
        sessions.on_end(move |path| ended.session_ended(path));
        captures
    }

    /// The broker: what realtime media leases its camera and microphone
    /// from, and where the session revokes everything (the screen
    /// locked, a privacy switch flipped).
    #[must_use]
    pub fn broker(&self) -> &Arc<Mutex<CaptureBroker>> {
        &self.broker
    }

    /// Hold a lease on stream `node` of portal session `path` for
    /// `caller`, listed as `label`. Revoking it runs `stop` and ends the
    /// session.
    ///
    /// # Errors
    /// The broker refused the source or the lease (capacity, a privacy
    /// switch).
    pub(crate) fn hold_stream(
        &self,
        path: &str,
        caller: &PortalCaller,
        label: &str,
        node: u32,
        stop: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<CaptureLeaseId, CaptureBrokerError> {
        let binding = CaptureSessionBinding::new(
            caller.app_id.clone(),
            caller.workspace.clone().unwrap_or_default(),
            path,
            PORTAL_TARGET,
        )?;
        let source = format!("{path}#{node}");
        let now = Instant::now();
        let mut broker = self.broker.lock_recover();
        broker.register_source(
            source.clone(),
            label,
            CaptureKind::Screen,
            LocalCaptureHandle::PipeWireNode(node),
        )?;
        let lease = broker
            .authorize(&binding, &source, CaptureKind::Screen, now)
            .and_then(|authorization| {
                broker.acquire_held(authorization, &binding, &source, CaptureKind::Screen, now)
            });
        let lease = match lease {
            Ok(lease) => lease,
            Err(error) => {
                broker.remove_source(&source);
                return Err(error);
            }
        };
        let sessions = self.sessions.clone();
        let ended = path.to_owned();
        broker.stop_on_revoke(
            lease,
            &binding,
            now,
            Arc::new(move || {
                stop();
                // Not from here: revocation holds the broker.
                let sessions_later = sessions.clone();
                let path = ended.clone();
                sessions.spawn(async move {
                    sessions_later.end(&path);
                });
            }),
        )?;
        drop(broker);
        self.held
            .lock_recover()
            .entry(path.to_owned())
            .or_insert_with(|| Held {
                binding,
                sources: Vec::new(),
            })
            .sources
            .push(source);
        self.sessions.changed();
        Ok(lease)
    }

    /// Release what portal session `path` held: it ended.
    fn session_ended(&self, path: &str) {
        let Some(held) = self.held.lock_recover().remove(path) else {
            return;
        };
        let mut broker = self.broker.lock_recover();
        broker.session_ended(&held.binding);
        for source in &held.sources {
            broker.remove_source(source);
        }
    }
}

impl crate::Published for Captures {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.broker
            .lock_recover()
            .live()
            .into_iter()
            .map(|capture| {
                offer::<CaptureEffect>(ResourceSpec::new(
                    format!("{CAPTURE_PREFIX}{}", capture.lease_id.to_opaque()),
                    "portal.capture",
                    capture.app_id,
                ))
                .with_presentation(ResourcePresentation {
                    detail: Some(capture.source),
                    badge: Some(kind_name(capture.kind).to_owned()),
                    preview: None,
                })
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let opaque = resource.strip_prefix(CAPTURE_PREFIX)?;
        self.broker
            .lock_recover()
            .live()
            .iter()
            .find(|capture| capture.lease_id.to_opaque() == opaque)
            .map(reading_of)
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let opaque = resource.strip_prefix(CAPTURE_PREFIX)?;
        let unknown = || DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        Some(
            decode::<CaptureEffect>(published, resource, action, arguments).and_then(|_| {
                let mut broker = self.broker.lock_recover();
                let stopped = broker
                    .lease_of(opaque)
                    .is_some_and(|lease| broker.stop(lease));
                drop(broker);
                if stopped {
                    self.sessions.changed();
                    Ok(Started::Done(ActionOutput::default()))
                } else {
                    Err(unknown())
                }
            }),
        )
    }
}

const fn kind_name(kind: CaptureKind) -> &'static str {
    match kind {
        CaptureKind::Camera => "camera",
        CaptureKind::Microphone => "microphone",
        CaptureKind::Screen => "screen",
        CaptureKind::SystemAudio => "system-audio",
    }
}

fn reading_of(capture: &crate::capture_broker::LiveCapture) -> Value {
    serde_json::json!({
        "app_id": capture.app_id,
        "kind": kind_name(capture.kind),
        "source": capture.source,
        "session": capture.session_id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::Published as _;

    #[test]
    fn a_held_stream_is_published_until_the_person_stops_it() {
        let sessions = Sessions::default();
        let captures = Captures::new(&sessions);
        let stopped = Arc::new(AtomicBool::new(false));
        let hook = Arc::clone(&stopped);
        let path = "/org/freedesktop/portal/desktop/session/1_2/s";
        captures
            .hold_stream(
                path,
                &PortalCaller::new("org.example.Share"),
                "Desk",
                7,
                Arc::new(move || hook.store(true, Ordering::SeqCst)),
            )
            .expect("held");
        let published = captures.resources();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].label, "org.example.Share");
        let resource = published[0].id.as_str().to_owned();
        assert_eq!(
            captures.read(&resource).expect("read")["source"],
            Value::from("Desk")
        );
        let stopped_now = captures.invoke(&published, &resource, "stop", &Map::new());
        assert!(matches!(stopped_now, Some(Ok(_))), "{stopped_now:?}");
        assert!(stopped.load(Ordering::SeqCst), "its stream stopped");
        assert!(captures.resources().is_empty());
    }

    #[test]
    fn an_ended_session_releases_its_streams() {
        let sessions = Sessions::default();
        let captures = Captures::new(&sessions);
        let path = "/org/freedesktop/portal/desktop/session/1_2/s";
        captures
            .hold_stream(
                path,
                &PortalCaller::new("org.example.Share"),
                "Desk",
                7,
                Arc::new(|| {}),
            )
            .expect("held");
        captures.session_ended(path);
        assert!(captures.resources().is_empty());
        assert!(
            captures
                .broker()
                .lock_recover()
                .enumerate(CaptureKind::Screen)
                .is_empty(),
            "its sources are gone too"
        );
    }
}
