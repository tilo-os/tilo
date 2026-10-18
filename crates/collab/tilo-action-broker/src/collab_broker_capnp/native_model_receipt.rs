//! Host-held successful request evidence. Report JSON cannot mint this record.
use super::{CapnpBrokerState, Lease, NativeModelLease, PendingTaskRun, Request};
use agent_provider_config::protocol::{Content, Role};

#[derive(Default)]
pub(super) struct Receipt {
    directions: bool,
    messages: std::collections::BTreeSet<String>,
}

impl Receipt {
    pub(super) fn capture(lease: &Lease, request: &Request) -> Result<Option<Self>, String> {
        let Request::NativeComplete { request, .. } = request else {
            // Transforms cannot acknowledge a planner's private context.
            return Ok(None);
        };
        if lease.directed.is_some() {
            return Ok(None);
        }
        let state = lease.state.upgrade().ok_or("MODEL receipt host ended")?;
        let service = state
            .native_tasks(lease.room())
            .ok_or("MODEL receipt owner absent")?;
        let snapshot = service.agent_snapshot(lease.room(), &lease.binding.agent_id)?;
        let control = snapshot
            .controls
            .get(&lease.binding.task_id)
            .ok_or("MODEL receipt control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("MODEL receipt run ended")?;
        if run.epoch != lease.binding.epoch
            || run.agent_id != lease.binding.agent_id
            || control.directions.last().map_or(0, |d| d.revision) != lease.binding.revision
        {
            return Err("MODEL receipt frozen frontier changed".into());
        }
        let texts = request
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| &m.content)
            .filter_map(|c| match c {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let directions = texts
            .iter()
            .flat_map(|text| {
                text.split(
                    "Authenticated workspace task directions (apply before further operations):\n",
                )
                .skip(1)
            })
            .filter_map(|text| {
                serde_json::Deserializer::from_str(text)
                    .into_iter::<Vec<runtime_collab::TaskDirection>>()
                    .next()?
                    .ok()
            })
            .flatten()
            .collect::<Vec<_>>();
        let private = service.private_instructions(lease.room(), &lease.binding)?;
        let directions_applied = control
            .directions
            .iter()
            .filter(|d| d.revision > run.applied_direction_revision)
            .all(|d| {
                let text = if d.text.starts_with("private-instruction:") {
                    private.as_ref().and_then(|p| p.directions.get(&d.revision))
                } else {
                    Some(&d.text)
                };
                text.is_some_and(|text| {
                    directions.iter().any(|seen| {
                        seen.revision == d.revision
                            && seen.author == d.author
                            && {
                                let issued_at = d.created_at;
                                seen.issued_at == issued_at
                            }
                            && seen.text == *text
                    })
                })
            });
        let mut receipt = Self {
            directions: directions_applied,
            messages: std::collections::BTreeSet::new(),
        };
        // Match the complete owner envelope, never an arbitrary model-authored ID.
        let observed = texts.iter().flat_map(|text| text.split("Untrusted task context. Sender and recipient below are attributed by the workspace owner,").skip(1))
            .filter_map(|frame| {
                let (_, json) = frame.split_once('\n')?;
                serde_json::Deserializer::from_str(json).into_iter::<runtime_collab::TaskMessage>().next()?.ok()
            }).collect::<Vec<_>>();
        for message in service.messages(lease.room(), &lease.binding)? {
            if observed.iter().any(|seen| seen == &message) {
                receipt.messages.insert(message.message_id);
            }
        }
        drop(state);
        Ok(Some(receipt))
    }

    pub(super) fn commit(self, lease: &Lease) -> Result<(), String> {
        let state = lease.state.upgrade().ok_or("MODEL receipt host ended")?;
        if !lease.live() {
            return Err("MODEL response lease ended before receipt".into());
        }
        state
            .native_tasks(lease.room())
            .ok_or("MODEL receipt owner absent")?
            .ensure_model_run_open(lease.room(), &lease.binding)?;
        let mut registry = state
            .native_model_leases
            .lock()
            .map_err(|_| "MODEL receipt registry unavailable")?;
        let key = (
            lease.peer,
            lease.binding.task_id.clone(),
            lease.binding.epoch,
            lease.binding.revision,
        );
        if registry.receipts.len() >= 1024 && !registry.receipts.contains_key(&key) {
            return Err("MODEL receipt capacity exceeded".into());
        }
        let prior = registry.receipts.entry(key).or_default();
        prior.directions |= self.directions;
        prior.messages.extend(self.messages);
        drop(registry);
        drop(state);
        Ok(())
    }
}

impl CapnpBrokerState {
    pub(in crate::collab_broker_capnp) async fn check_native_model_receipt(
        &self,
        run: &PendingTaskRun,
        revision: u64,
        message_ids: &[String],
    ) -> Result<(), String> {
        let binding = run
            .native
            .as_ref()
            .ok_or("MODEL receipt native binding absent")?;
        if binding.revision != revision {
            return Err("MODEL acknowledgement differs from request lease revision".into());
        }
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("MODEL receipt owner absent")?
            .clone();
        let room = run.cluster_id.clone();
        let current = binding.clone();
        // Admission may retain the owner guard while awaiting dispatch on this
        // LocalSet. A synchronous owner read here would prevent that dispatch.
        // Do not hold the receipt registry across the owner check either.
        tokio::task::spawn_blocking(move || service.ensure_model_run_open(&room, &current))
            .await
            .map_err(|_| "MODEL receipt owner check interrupted".to_owned())??;
        let registry = self
            .native_model_leases
            .lock()
            .map_err(|_| "MODEL receipt registry unavailable")?;
        let receipt = registry
            .receipts
            .get(&(
                run.peer_id,
                binding.task_id.clone(),
                binding.epoch,
                revision,
            ))
            .ok_or("MODEL acknowledgement has no successful frozen request")?;
        if !receipt.directions || message_ids.iter().any(|id| !receipt.messages.contains(id)) {
            return Err("MODEL acknowledgement context was not in the successful request".into());
        }
        drop(registry);
        Ok(())
    }
}
