//! Private custody is durable before any predecessor publication is retired.
#[cfg(test)]
#[path = "private_confirmation_tests.rs"]
mod confirmation_tests;
use super::{
    BTreeMap, Deserialize, GovernedRoom, Rooms, Serialize, SigningKey, SystemTime, publication,
};

#[derive(Default, Serialize, Deserialize)]
struct PrivateTaskText {
    instruction: Option<String>,
    agent_request: Option<serde_json::Value>,
    directions: BTreeMap<u64, String>,
}

impl Rooms {
    pub(super) async fn prepare_private_baseline(
        &self,
        room: &mut GovernedRoom,
    ) -> Result<(), String> {
        let committed = crate::workspace_io::get_or_initialize(
            &self.inner.host.workspaces,
            self.inner.host.access.state_root().to_path_buf(),
            self.inner.host.access.shell_payloads(),
            room.workspace_id(),
            true,
            crate::room_access::AdmissionMode::Governed,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await
        .map_err(|e| e.to_string())?
        .lock()
        .await
        .store
        .sanitized_room_baseline()
        .map_err(|e| e.to_string())?;
        if let Some(baseline) = committed {
            room.install_private_task_baseline_for_recovery(&baseline, &self.inner.host.key)?;
        } else if let Some(baseline) = room.committed_private_task_baseline()? {
            if baseline
                .verify_anchored_document(
                    room.room(),
                    room.peer.authority().device_public_key,
                    &baseline.anchor(),
                )
                .is_ok()
            {
                self.inner
                    .host
                    .records
                    .retire_room_channel(room.workspace_id(), room.peer(), &baseline)
                    .await?;
                room.record.published = 0;
                room.persist()?;
            } else {
                // Do not make an old privacy-contaminated checkpoint replayable.
                // Authenticate its exact ancestry, then stage its successor below.
                room.install_private_task_baseline_for_recovery(&baseline, &self.inner.host.key)?;
            }
        }
        if let Some(baseline) =
            room.stage_private_task_baseline(&self.inner.host.key, SystemTime::now())?
        {
            self.inner
                .host
                .records
                .retire_room_channel(room.workspace_id(), room.peer(), &baseline)
                .await?;
            room.install_private_task_baseline(&baseline)?;
        }
        Ok(())
    }
}

impl GovernedRoom {
    pub(super) fn stage_private_task_baseline(
        &self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<tilo_peer_state::SanitizedRoomBaseline>, String> {
        self.validate_confirmation_archives()?;
        let path = self.path.with_extension("private-task-text-v1.json");
        let mut custody: BTreeMap<String, PrivateTaskText> =
            super::private_tasks::read_private(&path)?.unwrap_or_default();
        let references = custody
            .iter()
            .map(|(task, text)| {
                (
                    task.clone(),
                    text.directions
                        .iter()
                        .map(|(revision, text)| {
                            (
                                *revision,
                                tilo_peer_state::workspace_task::private_instruction_reference(
                                    text,
                                ),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        let mut candidate = self.peer.clone();
        let (migrated, baseline) =
            candidate.extract_native_task_private_baseline(key, now, &references)?;
        if !tilo_joined_room::PrivateTaskTextStore::migration_has_private_text(&migrated)
            && self.peer.sanitized_room_generation()? != 0
        {
            self.validate_migrated_task_confirmations(
                self.peer
                    .admitted_room()
                    .ok_or("private task room absent")?,
            )?;
            return Ok(None);
        }
        self.persist_migrated_task_confirmations(&migrated)?;
        self.validate_migrated_task_confirmations(
            candidate
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        let history_path = self.path.with_extension("private-task-history-v1.json");
        let mut historical: BTreeMap<String, Vec<serde_json::Value>> =
            super::private_tasks::read_private(&history_path)?.unwrap_or_default();
        // Retain complete profiles before retiring shared history. Keep this
        // separate from instruction RPCs, which must never return account data.
        let model_path = self
            .path
            .with_extension("private-task-model-authority-v1.json");
        let mut models: BTreeMap<String, tilo_desktop_model::task::WorkspaceTaskModelAuthority> =
            super::private_tasks::read_private(&model_path)?.unwrap_or_default();
        for (task, text) in migrated {
            if let Some(model) = text.model_authority {
                models.insert(task.clone(), model);
            }
            if !text.historical_reports.is_empty() {
                // Migration archives are audit evidence, not authenticated
                // reports: never invent a source actor, invocation or receipt.
                let archive = historical.entry(task.clone()).or_default();
                for event in text.historical_reports {
                    if !archive.contains(&event) {
                        archive.push(event);
                    }
                }
            }
            let entry = custody.entry(task).or_default();
            if text.instruction.is_some() {
                entry.instruction = text.instruction;
            }
            if let Some(request) = text.agent_request {
                entry.agent_request =
                    Some(serde_json::to_value(request).map_err(|e| e.to_string())?);
            }
            entry.directions.extend(text.directions);
        }
        let history_bytes = serde_json::to_vec(&historical).map_err(|e| e.to_string())?;
        if history_bytes.len() > 72 * 1024 * 1024 {
            return Err("private task history exceeds capacity".into());
        }
        let model_bytes = serde_json::to_vec(&models).map_err(|e| e.to_string())?;
        if models.len() > 4096 || model_bytes.len() > 16 * 1024 * 1024 {
            return Err("private model snapshot capacity exhausted".into());
        }
        tilo_files::native::atomic::write_private(&model_path, &model_bytes)
            .map_err(|_| "private model snapshot persistence failed")?;
        tilo_files::native::atomic::write_private(&history_path, &history_bytes)
            .map_err(|_| "private task history persistence failed")?;
        let bytes = serde_json::to_vec(&custody).map_err(|e| e.to_string())?;
        if bytes.len() > 72 * 1024 * 1024 {
            return Err("private task custody exceeds capacity".into());
        }
        tilo_files::native::atomic::write_private(&path, &bytes).map_err(|e| e.to_string())?;
        Ok(Some(baseline))
    }

    fn committed_private_task_baseline(
        &self,
    ) -> Result<Option<tilo_peer_state::SanitizedRoomBaseline>, String> {
        for entry in &self.record.outgoing {
            let message: neighbour_wire::NeighbourMsg =
                postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
            if let neighbour_wire::NeighbourMsg::SanitizedRoomBaseline { baseline, .. } = message {
                return postcard::from_bytes(&baseline)
                    .map(Some)
                    .map_err(|e| e.to_string());
            }
        }
        Ok(None)
    }

    /// Commit the already-retired chain into the private governor record.
    /// Resource publications and their signed channels remain untouched.
    pub(super) fn install_private_task_baseline(
        &mut self,
        baseline: &tilo_peer_state::SanitizedRoomBaseline,
    ) -> Result<(), String> {
        self.install_private_task_baseline_inner(baseline, None)
    }

    fn install_private_task_baseline_for_recovery(
        &mut self,
        baseline: &tilo_peer_state::SanitizedRoomBaseline,
        key: &SigningKey,
    ) -> Result<(), String> {
        self.install_private_task_baseline_inner(baseline, Some(key))
    }

    fn install_private_task_baseline_inner(
        &mut self,
        baseline: &tilo_peer_state::SanitizedRoomBaseline,
        recovery_key: Option<&SigningKey>,
    ) -> Result<(), String> {
        if self.peer.sanitized_room_generation()? == baseline.generation {
            let mut genesis = if let Some(key) = recovery_key {
                tilo_joined_room::verify_private_recovery_baseline(
                    baseline,
                    self.room(),
                    self.peer.authority().device_public_key,
                    key,
                )?
            } else {
                baseline.verify_anchored_document(
                    self.room(),
                    self.peer.authority().device_public_key,
                    &baseline.anchor(),
                )?
            };
            let mut current = self
                .peer
                .admitted_room()
                .ok_or("room governance missing")?
                .clone();
            tilo_peer_state::validate_sanitized_room_sync(Some(&genesis), &mut current)?;
            if genesis
                .get_changes(&[])
                .iter()
                .any(|change| current.get_change_by_hash(&change.hash()).is_none())
            {
                return Err("committed room baseline equivocates with the governor record".into());
            }
            return Ok(());
        }
        let mut candidate = self.peer.clone();
        candidate.admit_sanitized_room_baseline(baseline, SystemTime::now())?;
        let mut record = self.record.clone();
        let channel = *blake3::hash(b"shell-tree").as_bytes();
        let mut kept = Vec::new();
        for entry in record.outgoing {
            let message: neighbour_wire::NeighbourMsg =
                postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
            if neighbour_wire::retention(&message, entry.checkpoint)
                .is_none_or(|(route, _)| route != channel)
            {
                kept.push(entry);
            }
        }
        let message = neighbour_wire::NeighbourMsg::SanitizedRoomBaseline {
            room: baseline.room.clone(),
            baseline: postcard::to_stdvec(baseline).map_err(|e| e.to_string())?,
        };
        kept.insert(0, publication(&message, true)?);
        record.outgoing = kept;
        record.published = 0;
        record.peer = self.parts.stage(&candidate)?;
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("room generation exhausted")?;
        let prior = std::mem::replace(&mut self.record, record);
        if let Err(error) = self.persist() {
            self.record = prior;
            return Err(error);
        }
        self.peer = candidate;
        Ok(())
    }
}
