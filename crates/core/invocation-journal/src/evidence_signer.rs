//! Host-only signing for semantic admission receipts. Never exposed as a
//! signing service: callers supply the receipt's domain-separated bytes.
//!
//! What signs is the host's own device: the certificate its device was issued and the key it
//! took. The host names both as it opens the workspaces it serves ([`serve_as`]); nothing here
//! reads an identity store, so a daemon that runs as one account and serves another person signs
//! as the device it is, wherever its person's store is kept. A certificate expired at the clock,
//! one whose root took it back, or one confined to another workspace signs nothing.

#[cfg(target_family = "wasm")]
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_family = "wasm"))]
use std::sync::LazyLock;

use collab_types::signer::DeviceSigner;
#[cfg(not(target_family = "wasm"))]
use parking_lot::RwLock;
use tilo_user_identity::DeviceCert;

/// What this process signs as, held where the platform can share it: threads share an `Arc`, a
/// page's one thread an `Rc`.
#[cfg(not(target_family = "wasm"))]
type SharedHost = Arc<HostIdentity>;
#[cfg(target_family = "wasm")]
type SharedHost = Rc<HostIdentity>;

/// What this process signs as: the certificate its device holds and the key it took, named by the
/// host that serves the calls.
#[cfg(not(target_family = "wasm"))]
static HOST: LazyLock<RwLock<Option<SharedHost>>> = LazyLock::new(RwLock::default);

// A page has one thread, and its signer is a JavaScript object bound to it: what it signs as is
// that thread's.
#[cfg(target_family = "wasm")]
thread_local! {
    static HOST: std::cell::RefCell<Option<SharedHost>> = const { std::cell::RefCell::new(None) };
}

/// Name what this process signs as, or nobody.
fn name_host(host: Option<SharedHost>) {
    #[cfg(not(target_family = "wasm"))]
    {
        *HOST.write() = host;
    }
    #[cfg(target_family = "wasm")]
    HOST.with(|named| *named.borrow_mut() = host);
}

/// What this process signs as, when a host named it.
fn named_host() -> Option<SharedHost> {
    #[cfg(not(target_family = "wasm"))]
    {
        HOST.read().clone()
    }
    #[cfg(target_family = "wasm")]
    {
        HOST.with(|named| named.borrow().clone())
    }
}

/// The workspaces a host opened, held as its platform can: threads share them behind a lock, a
/// page's one thread keeps them in a cell.
#[cfg(not(target_family = "wasm"))]
type OpenedSet = RwLock<std::collections::BTreeSet<String>>;
#[cfg(target_family = "wasm")]
type OpenedSet = std::cell::RefCell<std::collections::BTreeSet<String>>;

struct HostIdentity {
    certificate: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    /// The workspaces this host opened: the ones its receipts may be signed for. A workspace no
    /// host here opened has no signer of its own and fails closed.
    opened: OpenedSet,
}

impl HostIdentity {
    /// This host opened `workspace`.
    fn open(&self, workspace: &str) {
        #[cfg(not(target_family = "wasm"))]
        self.opened.write().insert(workspace.to_owned());
        #[cfg(target_family = "wasm")]
        self.opened.borrow_mut().insert(workspace.to_owned());
    }

    /// This host serves `workspace` no more.
    fn close(&self, workspace: &str) {
        #[cfg(not(target_family = "wasm"))]
        self.opened.write().remove(workspace);
        #[cfg(target_family = "wasm")]
        self.opened.borrow_mut().remove(workspace);
    }

    /// Whether this host opened `workspace`.
    fn has_opened(&self, workspace: &str) -> bool {
        #[cfg(not(target_family = "wasm"))]
        {
            self.opened.read().contains(workspace)
        }
        #[cfg(target_family = "wasm")]
        {
            self.opened.borrow().contains(workspace)
        }
    }
}

/// Name what this process signs its receipts as: the host's device certificate and the key that
/// certificate names. A host calls this as it opens the workspaces it serves; the last host to
/// name one is the one that signs.
pub fn serve_as(certificate: DeviceCert, signer: Arc<dyn DeviceSigner>) {
    name_host(Some(SharedHost::new(HostIdentity {
        certificate,
        signer,
        opened: OpenedSet::default(),
    })));
}

/// Name `workspace` as one this host opened: the receipts of its calls are signed from now on. A
/// host calls it as it opens each workspace it serves.
pub fn opened(workspace: &str) {
    if let Some(host) = named_host() {
        host.open(workspace);
    }
}

/// `workspace` is one this host serves no more: it closed it, left it or was removed from it, and
/// the receipts of its calls are signed no longer.
pub fn closed(workspace: &str) {
    if let Some(host) = named_host() {
        host.close(workspace);
    }
}

/// Whether a host named what signs this process's receipts yet: what a fixture asks before it
/// waits on evidence, so a missing signer fails at once naming it.
#[must_use]
pub fn serves_a_signer() -> bool {
    named_host().is_some()
}

/// Sign already domain-separated semantic receipt bytes with this host's device. The returned
/// certificate identifies the device signing the bytes.
///
/// # Errors
/// No host named what signs here, the certificate is invalid at the clock, or its person's
/// devices record took the device back or confined it to another workspace.
pub fn sign(workspace: &str, bytes: &[u8]) -> Result<(DeviceCert, [u8; 64]), String> {
    let now = tilo_clock::try_unix_now_ns().ok_or("receipt signing clock is unavailable")?;
    let host = named_host().ok_or("no host names what signs this process's receipts")?;
    if !host.has_opened(workspace) {
        return Err("no host opened the workspace this receipt is for".into());
    }
    tilo_user_identity::verify_device_cert_at(&host.certificate, now)
        .map_err(|error| error.to_string())?;
    if !tilo_identity::certificates::admitted_in(
        workspace,
        &host.certificate.user_public_key,
        &host.certificate,
    ) {
        return Err("this device reaches no such workspace as the identity it signs as".into());
    }
    let signature = collab_types::signer::sign_now(host.signer.as_ref(), bytes.to_vec())
        .map_err(|error| error.to_string())?;
    Ok((host.certificate.clone(), signature))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::*;
    use tilo_user_identity::DeviceCapabilities;

    /// A host's device: its certificate, its key, and the person whose root certified it. Each
    /// fixture is a person of its own, so one phase's revocation touches no other's.
    struct Fixture {
        certificate: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
    }

    impl Fixture {
        fn new(seeds: (u8, u8), capabilities: DeviceCapabilities) -> Self {
            let root = ed25519_dalek::SigningKey::from_bytes(&[seeds.0; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[seeds.1; 32]);
            tilo_identity::sources::test_trust::install();
            let mut certificate = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "A host's device".into(),
                capabilities,
                issued_at_ns: tilo_clock::unix_now_ns(),
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
            let signer: Arc<dyn DeviceSigner> = Arc::new(device);
            serve_as(certificate.clone(), Arc::clone(&signer));
            Self {
                certificate,
                signer,
            }
        }
    }

    tilo_test_env::scenario! {
        fn a_host_signs_with_the_device_it_is;

        phase the_hosts_device_signs_the_bytes_it_is_given;
        {
            let fixture = Fixture::new((241, 242), DeviceCapabilities::member());
            let workspace = uuid::Uuid::new_v4().to_string();
            opened(&workspace);
            let bytes = b"semantic receipt";
            let (cert, signature) = sign(&workspace, bytes).unwrap();
            assert_eq!(cert, fixture.certificate);
            ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key)
                .unwrap()
                .verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&signature))
                .expect("the signature binds the actual receipt bytes");
            let _ = fixture.signer;
        }

        phase a_device_its_person_took_back_signs_nothing;
        {
            let fixture = Fixture::new((243, 244), DeviceCapabilities::member());
            let after = uuid::Uuid::new_v4().to_string();
            opened(&after);
            tilo_identity::sources::test_trust::revoke(
                fixture.certificate.user_public_key,
                fixture.certificate.device_public_key,
            );
            assert!(sign(&after, b"after").is_err());
        }

        phase a_device_confined_to_one_workspace_signs_nothing_elsewhere;
        {
            let own = uuid::Uuid::new_v4();
            Fixture::new(
                (245, 246),
                DeviceCapabilities::workspace_device(*own.as_bytes()),
            );
            opened(&own.to_string());
            assert!(sign(&own.to_string(), b"its own workspace").is_ok());
            assert!(sign(&uuid::Uuid::new_v4().to_string(), b"elsewhere").is_err());
        }

        phase a_workspace_the_host_closed_signs_nothing;
        {
            let fixture = Fixture::new((247, 248), DeviceCapabilities::member());
            let workspace = uuid::Uuid::new_v4().to_string();
            opened(&workspace);
            assert!(sign(&workspace, b"before it closed").is_ok());
            closed(&workspace);
            assert!(sign(&workspace, b"after it closed").is_err());
            let _ = fixture.signer;
        }

        phase a_process_no_host_named_signs_nothing;
        {
            name_host(None);
            assert!(sign(&uuid::Uuid::new_v4().to_string(), b"nobody").is_err());
        }
    }
}
