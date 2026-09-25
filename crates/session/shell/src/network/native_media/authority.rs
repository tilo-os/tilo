use super::*;
use qos_moq::{client::ClientAuthority, relay::Direction};

impl Ticket {
    pub(super) fn inspect_current<T>(
        &self,
        read: impl FnOnce(&State, &protocol::GrantView) -> Result<T, String>,
    ) -> Result<T, String> {
        self.live()?;
        if self.leaving.load(Ordering::SeqCst) {
            return Err("native participation leaving".into());
        }
        self.owner.with_current(self.room, |view| {
            let instant = Instant::now();
            let now = now_ns()?;
            let current = registry::current_view(view, now)?;
            let mut state = self.state.lock();
            if now < state.wall_high_water {
                return Err("native media clock moved backwards".into());
            }
            state.wall_high_water = now;
            let grant = state.grant.as_ref().ok_or("native grant missing")?;
            let carrier = state.carrier.as_ref().ok_or("native carrier missing")?;
            if instant >= grant.expires || instant >= grant.presence || instant >= carrier.expires {
                return Err("native media lease expired".into());
            }
            let Phase::Control(control) = &state.phase else {
                return Err("native control unavailable".into());
            };
            let actual =
                control.current_carrier(&current, grant.view.handle, carrier.protocol_id)?;
            if actual.namespace != grant.view.namespace
                || actual.inventory != grant.view.inventory
                || actual.directions != grant.view.directions
            {
                return Err("native retained carrier scope changed".into());
            }
            let mut end = grant
                .expires
                .min(grant.presence)
                .min(carrier.expires)
                .min(state.descriptor_expires);
            if let Some(custody_expires) = state.custody_expires {
                end = end.min(custody_expires);
            }
            if instant >= end {
                return Err("native capture authority expired".into());
            }
            self.capture_until.store(
                end.saturating_duration_since(self.base)
                    .as_nanos()
                    .min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            read(&state, &actual)
        })
    }
    pub(super) fn inventory(&self) -> runtime_collab::native_media::Inventory {
        self.inspect_current(|_, grant| {
            Ok(runtime_collab::native_media::Inventory {
                revision: Some(grant.inventory.revision),
                publication: grant.directions.publish.then(|| namespace(grant.namespace)),
                publishers: if grant.directions.subscribe {
                    grant
                        .inventory
                        .namespaces
                        .iter()
                        .copied()
                        .map(namespace)
                        .collect()
                } else {
                    Vec::new()
                },
            })
        })
        .unwrap_or_default()
    }
}
impl ClientAuthority for Ticket {
    fn current(&self, scope: &ConnectionScope, direction: Direction, name: &str) -> bool {
        self.inspect_current(|state, grant| {
            let carrier = state.carrier.as_ref().ok_or("native carrier absent")?;
            if &carrier.scope != scope {
                return Err("native carrier generation changed".into());
            }
            let publication = grant.directions.publish.then(|| namespace(grant.namespace));
            let subscriptions: Vec<_> = if grant.directions.subscribe {
                grant
                    .inventory
                    .namespaces
                    .iter()
                    .copied()
                    .map(namespace)
                    .collect()
            } else {
                Vec::new()
            };
            if publication != scope.publication || subscriptions != scope.subscriptions {
                return Err("native current inventory changed".into());
            }
            match direction {
                Direction::Publish if publication.as_deref() == Some(name) => Ok(()),
                Direction::Subscribe if subscriptions.iter().any(|allowed| allowed == name) => {
                    Ok(())
                }
                _ => Err("native exact namespace direction denied".into()),
            }
        })
        .is_ok()
    }
}
