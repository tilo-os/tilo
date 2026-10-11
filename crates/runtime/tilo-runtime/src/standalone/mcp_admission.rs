//! Exact selected-room material for a single runtime MCP execution.
//! Uses existing custody and enrollment; never enrolls, provisions or grants.
use super::{Arc, Connect, Joined, RoomId, room_topic};
use ed25519_dalek::Signer as _;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

pub(in crate::standalone) struct SessionAdmission {
    caller: Caller,
    room: RoomId,
    own_room: bool,
    certificate: DeviceCert,
    enrollment: Option<(tilo_peer_state::join::TransportHint, u64)>,
    presentation: Vec<u8>,
    _registration: super::super::execution::ExecutionRegistration,
}

impl SessionAdmission {
    pub(in crate::standalone) fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }
    pub(in crate::standalone) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::standalone) fn presentation(&self) -> &[u8] {
        &self.presentation
    }

    pub(in crate::standalone) fn check(&self, joined: &Joined) -> Result<(), String> {
        if !self.caller.execution_is_live() {
            return Err("execution session ended".into());
        }
        self.check_original_custody(joined)
    }

    fn check_original_custody(&self, joined: &Joined) -> Result<(), String> {
        let result = (|| {
            let (certificate, enrollment) = joined.mcp_material(self.room, self.own_room)?;
            if certificate != self.certificate || enrollment != self.enrollment {
                return Err("original room or device custody changed".into());
            }
            Ok(())
        })();
        if result.is_err() {
            self.caller.retire_execution();
        }
        result
    }
}

impl Joined {
    pub(in crate::standalone) fn recheck_mcp_executions(&self, room: RoomId) -> Result<(), String> {
        // Baselines come only from host-admitted sessions, never a presented
        // certificate. Release the registry lock before material reads.
        let admissions = self.executions.admissions(room)?;
        #[cfg(test)]
        tests::after_snapshot();
        let mut result = Ok(());
        for admission in admissions {
            // A normal EOF may retire this execution after the snapshot above.
            // Its lifetime cannot invalidate another caller's room custody.
            // Still verify every original material pin; never reopen its lease.
            if let Err(error) = admission.check_original_custody(self) {
                result = Err(error);
            }
        }
        result
    }

    fn mcp_material(
        &self,
        room: RoomId,
        own_room: bool,
    ) -> Result<
        (
            DeviceCert,
            Option<(tilo_peer_state::join::TransportHint, u64)>,
        ),
        String,
    > {
        if own_room {
            let (certificate, _) = self.custody.current_native_material(room_topic(room)?)?;
            return Ok((certificate, None));
        }
        let (hint, certificate, _, continuity) =
            Connect::new(self.owner.clone(), room)?.material(&self.custody)?;
        Ok((certificate, Some((hint, continuity))))
    }

    pub(in crate::standalone) fn admit_mcp_execution(
        &self,
        room: &str,
        own_room: &str,
        actor: &str,
    ) -> Result<Arc<SessionAdmission>, String> {
        let selected = RoomId::parse(room).ok_or("invalid exact MCP room")?;
        if selected.is_nil() || selected.to_string() != room {
            return Err("noncanonical MCP room".into());
        }
        let own_room = room == own_room;
        let (certificate, enrollment) = self.mcp_material(selected, own_room)?;
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_room(room)
                // Preserve the standalone host's existing foreign-device restriction,
                // including when a signed certificate permits a stronger device.
                .from_foreign_device(true)
                .in_new_execution();
        if caller.principal() != &tilo_actions::remote::CallerPrincipal::Person(actor.into()) {
            caller.retire_execution();
            return Err("selected room identity differs from the runtime profile actor".into());
        }
        // The one owner observer is already installed. Register this exact
        // lease, then re-read the pinned material: a concurrent native fence
        // either closes the lease or is detected by the changed continuity.
        let registration = self.executions.register_checked(selected, &caller, || {
            let current = self.mcp_material(selected, own_room)?;
            if current != (certificate.clone(), enrollment.clone()) {
                return Err("room changed while admitting execution".into());
            }
            Ok(())
        })?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.into(),
            device_certificate: serde_json::to_vec(&certificate)
                .map_err(|error| error.to_string())?,
            proof: self
                .custody
                .device_signing_key
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let admission = Arc::new(SessionAdmission {
            caller,
            room: selected,
            own_room,
            certificate,
            enrollment,
            presentation,
            _registration: registration,
        });
        self.executions.attach(&admission)?;
        admission.check(self)?;
        Ok(admission)
    }
}

#[cfg(test)]
#[path = "mcp_admission_tests.rs"]
mod tests;
