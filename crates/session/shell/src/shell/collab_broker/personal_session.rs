//! The actual native broker's signed personal execution and compositor lifetime.
use crate::shell::collab_broker_capnp::{LocalSessionLifecycle, LocalSessionPermit};
use std::sync::Arc;
use tilo_action_broker::{collab_broker::AppActionBroker, services::CertifiedPersonContext};
use tilo_actions::remote::Caller;
use tilo_identity_host::custody::HostIdentity;

type Sources = std::collections::HashMap<std::path::PathBuf, std::sync::Weak<NativePersonalSource>>;
fn sources() -> &'static std::sync::Mutex<Sources> {
    static SOURCES: std::sync::OnceLock<std::sync::Mutex<Sources>> = std::sync::OnceLock::new();
    SOURCES.get_or_init(std::sync::Mutex::default)
}

/// Published only by the already bound native broker, retained by its owner.
/// Socket scope prevents one compositor from selecting another session's source.
pub(crate) struct NativePersonalSource {
    broker: AppActionBroker,
    lifecycle: LocalSessionLifecycle,
    host: HostIdentity,
}
impl NativePersonalSource {
    #[cfg(test)]
    pub(crate) fn install_for_test(
        socket: std::path::PathBuf,
        broker: AppActionBroker,
        lifecycle: LocalSessionLifecycle,
        host: HostIdentity,
    ) -> Option<Arc<Self>> {
        Self::install(socket, broker, lifecycle, host)
    }
    pub(super) fn install(
        socket: std::path::PathBuf,
        broker: AppActionBroker,
        lifecycle: LocalSessionLifecycle,
        host: HostIdentity,
    ) -> Option<Arc<Self>> {
        if !socket.is_absolute() {
            return None;
        }
        let mut sources = sources().lock().ok()?;
        sources.retain(|_, source| source.strong_count() != 0);
        if sources
            .get(&socket)
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|source| source.lifecycle.acquire().is_some())
        {
            return None;
        }
        let source = Arc::new(Self {
            broker,
            lifecycle,
            host,
        });
        sources.insert(socket, Arc::downgrade(&source));
        Some(source)
    }
    pub(crate) fn admit_for_socket(
        socket: &std::path::Path,
        principal: &str,
    ) -> Result<Arc<NativePersonalSession>, String> {
        let source = sources()
            .lock()
            .map_err(|_| "native personal source unavailable")?
            .get(socket)
            .and_then(std::sync::Weak::upgrade)
            .ok_or("native personal source unavailable")?;
        NativePersonalSession::admit(
            &source.broker,
            &source.lifecycle,
            source.host.clone(),
            principal,
            socket,
        )
    }
}

pub(crate) struct NativePersonalSession {
    person: CertifiedPersonContext,
    host: HostIdentity,
    caller: Caller,
    permit: LocalSessionPermit,
    // The lifecycle keeps a weak listener. This execution alone owns its watch.
    _watch: Arc<dyn Fn() + Send + Sync>,
    workspace: u64,
    socket: std::path::PathBuf,
}
impl NativePersonalSession {
    pub(super) fn admit(
        broker: &AppActionBroker,
        lifecycle: &LocalSessionLifecycle,
        host: HostIdentity,
        principal: &str,
        socket: &std::path::Path,
    ) -> Result<Arc<Self>, String> {
        let permit = lifecycle
            .acquire()
            .ok_or("native personal session inactive")?;
        let mut public_key = [0; 32];
        hex::decode_to_slice(principal, &mut public_key)
            .map_err(|_| "native principal unavailable")?;
        let room = collab_types::person_room(&public_key).to_string();
        let workspace =
            tilo_relay_protocol::room_workspace_id(&room).map_err(|error| error.to_string())?;
        let (certificate, _) = host.current_native_material(workspace)?;
        if certificate.user_public_key != public_key || certificate.capabilities.foreign {
            return Err("native principal or device differs from current custody".into());
        }
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&certificate)
                .map_err(|error| error.to_string())?,
            proof: ed25519_dalek::Signer::sign(
                &host.device_signing_key,
                &runtime_identity::collaboration_join_signing_bytes(&room),
            )
            .to_bytes()
            .to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_room(room)
                .in_new_execution();
        let person = match broker.certified_person_context(&caller, &presentation) {
            Ok(person) => person,
            Err(error) => {
                caller.retire_execution();
                return Err(error);
            }
        };
        let execution = caller.clone();
        let generation = permit.clone();
        let watch: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if !generation.is_current() {
                execution.retire_execution();
            }
        });
        lifecycle.watch(&watch);
        let session = Arc::new(Self {
            person,
            host,
            caller,
            permit,
            _watch: watch,
            workspace,
            socket: socket.to_path_buf(),
        });
        session.check()?;
        Ok(session)
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if !self.permit.is_current() {
            self.caller.retire_execution();
            return Err("native personal session ended or changed".into());
        }
        self.person.revalidate()?;
        let (certificate, _) = self.host.current_native_material(self.workspace)?;
        if hex::encode(certificate.user_public_key) != self.person.principal()
            || hex::encode(certificate.device_public_key) != self.person.device()
            || certificate.capabilities.foreign
        {
            return Err("native personal custody changed".into());
        }
        Ok(())
    }
    pub(crate) fn person(&self) -> CertifiedPersonContext {
        self.person.clone()
    }
    pub(crate) fn matches_socket(&self, socket: &std::path::Path) -> bool {
        self.socket == socket
    }
    pub(crate) fn host(&self) -> HostIdentity {
        self.host.clone()
    }
}
impl Drop for NativePersonalSession {
    fn drop(&mut self) {
        self.caller.retire_execution();
    }
}
