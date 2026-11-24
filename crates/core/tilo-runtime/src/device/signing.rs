//! What the daemon signs with the person's root beside a device's ticket: the keys a disk unlock
//! needs, and a browser's sign-in. The root stays here: nothing outside asks for it, and what
//! leaves is a certificate, a signature, or a release sealed to the key that asked for it.

use std::io;
use std::sync::{Arc, Weak};

use serde_json::{Value, json};
use tilo_server_protocol::WorkspaceId;
use tilo_unlock_protocol::{
    EnrolRequest, Half, Release, SealedSiblingHalf, open_sibling_half, seal_release,
    seal_sibling_half,
};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, StatementSigner as _,
    device_cert_signing_digest,
};
use tilo_workspace_service::workspaces::{Workspaces, person_workspace};
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use super::workspaces::{ServiceWorkspaces, block_on_owned};
use super::services::root_record;

/// How long an agent's grant this daemon certifies lasts, and how much of it may be left when it
/// is certified again.
const AGENT_GRANT_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;
const AGENT_GRANT_RENEW_WITHIN_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// The person's root as this daemon keeps it, and the identity it signs for.
fn person() -> io::Result<([u8; 32], String, tilo_identity::store::SigningIdentity)> {
    let record = root_record()?;
    let dir = record
        .parent()
        .ok_or_else(|| io::Error::other("the root's record has no directory"))?;
    let root = tilo_identity::store::load_from_path(record.clone())
        .map_err(|error| io::Error::other(error.to_string()))?;
    let person = root.group_public_key();
    let id = tilo_identity::store::IdentityRegistryStore::at(dir)
        .load_reconciled()?
        .identities
        .into_values()
        .find(|profile| profile.record_path == record)
        .map(|profile| profile.id)
        .ok_or_else(|| io::Error::other("the root's record is in no registry"))?;
    Ok((person, id, root))
}

/// The daemon's `devices#personalDevices` answer: the person's devices as their record lists them,
/// the revocations their root signed, and whether this daemon keeps that root.
///
/// # Errors
/// This daemon holds no workspace service, or its root cannot be read.
pub fn personal_devices(workspaces: Option<&Arc<ServiceWorkspaces>>) -> io::Result<Value> {
    let workspaces =
        workspaces.ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?;
    let copies = workspaces.copies().clone();
    let person = copies.serving_certificate().user_public_key;
    let devices = block_on_owned(async move { person_workspace::devices::of(&copies, &person).await });
    let listed = devices.listed(&person);
    let revocations = workspaces
        .copies()
        .revocations()
        .iter()
        .filter(|entry| entry.user_public_key == person)
        .map(|entry| hex::encode(postcard::to_allocvec(entry).unwrap_or_default()))
        .collect::<Vec<_>>();
    Ok(json!({
        "person": hex::encode(person),
        "key_holder": root_record().is_ok(),
        "devices": listed
            .iter()
            .map(|listed| json!({
                "certificate": hex::encode(postcard::to_allocvec(&listed.cert).unwrap_or_default()),
                "revoked": listed.device.revoked,
            }))
            .collect::<Vec<_>>(),
        "revocations": revocations,
    }))
}

/// A key this daemon certified with the person's root: its certificate, the person it belongs to,
/// and the time the certificate ends.
#[derive(Debug)]
pub(crate) struct AgentCertified {
    pub identity: String,
    pub expires_at_ns: u64,
    pub cert: Option<DeviceCert>,
    pub revoked: bool,
}

/// Certify `grant`, an agent's grant key, with the person's root: for `ask`'s agent in its
/// workspace where one is named, else for every workspace. A key the person revoked is answered as
/// revoked; one certified for long enough already is answered as it stands. The certificate is
/// kept among the person's devices.
pub(crate) async fn certify_agent_in(
    copies: &Workspaces,
    grant: [u8; 32],
    ask: Option<(WorkspaceId, String, String)>,
) -> io::Result<AgentCertified> {
    let (person, identity, root) = person()?;
    let devices = person_workspace::devices::of(copies, &person).await;
    let listed = devices.listed(&person);
    if listed
        .iter()
        .any(|listed| listed.cert.device_public_key == grant && listed.device.revoked)
        || copies.revocations().is_revoked(&person, &grant)
    {
        return Ok(AgentCertified {
            identity,
            expires_at_ns: 0,
            cert: None,
            revoked: true,
        });
    }
    let now = tilo_clock::unix_now_ns();
    let lasting = listed
        .iter()
        .map(|listed| &listed.cert)
        .filter(|cert| cert.device_public_key == grant)
        .filter_map(|cert| cert.expires_at_ns.map(|expires| (expires, cert)))
        .max_by_key(|(expires, _)| *expires)
        .filter(|(expires, _)| expires.saturating_sub(now) > AGENT_GRANT_RENEW_WITHIN_NS);
    if let Some((expires_at_ns, cert)) = lasting {
        return Ok(AgentCertified {
            identity,
            expires_at_ns,
            cert: Some(cert.clone()),
            revoked: false,
        });
    }
    let (capabilities, label) = match ask {
        Some((workspace, agent, name)) => (
            DeviceCapabilities::agent_grant(AgentScope::agent_in(
                *workspace.as_uuid().as_bytes(),
                &agent,
            )),
            name,
        ),
        None => (
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            "Agent key".to_owned(),
        ),
    };
    let expires_at_ns = now.saturating_add(AGENT_GRANT_LIFETIME_NS);
    let mut cert = DeviceCert {
        user_public_key: person,
        device_public_key: grant,
        device_label: label,
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(expires_at_ns),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root)
        .map_err(|error| io::Error::other(error.to_string()))?;
    tilo_user_identity::verify_device_cert_at(&cert, now)
        .map_err(|error| io::Error::other(error.to_string()))?;
    person_workspace::devices::keep(copies, &cert)
        .await
        .map_err(io::Error::other)?;
    Ok(AgentCertified {
        identity,
        expires_at_ns,
        cert: Some(cert),
        revoked: false,
    })
}

/// The daemon's `devices#certifyAgent` answer for `args`.
///
/// # Errors
/// This daemon keeps no root, or it did not certify the key.
pub fn certify_agent(
    workspaces: Option<&Arc<ServiceWorkspaces>>,
    args: &Value,
) -> io::Result<Value> {
    let grant = args
        .get("grant")
        .and_then(Value::as_str)
        .and_then(|grant| hex::decode(grant).ok())
        .and_then(|grant| <[u8; 32]>::try_from(grant).ok())
        .ok_or_else(|| io::Error::other("an agent's grant is named by its key, as hex"))?;
    let ask = match (
        args.get("workspace").and_then(Value::as_str),
        args.get("agent").and_then(Value::as_str),
    ) {
        (Some(workspace), Some(agent)) => Some((
            WorkspaceId::parse(workspace)
                .ok_or_else(|| io::Error::other("the workspace is not one"))?,
            agent.to_owned(),
            args.get("name")
                .and_then(Value::as_str)
                .unwrap_or(agent)
                .to_owned(),
        )),
        _ => None,
    };
    let workspaces =
        workspaces.ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?;
    let copies = workspaces.copies().clone();
    let certified = block_on_owned(async move { certify_agent_in(&copies, grant, ask).await })?;
    if certified.revoked {
        return Ok(json!({ "revoked": true }));
    }
    let cert = certified.cert.as_ref().ok_or_else(|| {
        io::Error::other("the person's root gave no certificate for the key")
    })?;
    let cert = tilo_user_identity::canonical::canonical_bytes(cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(json!({
        "identity": certified.identity,
        "expires_at_ns": certified.expires_at_ns,
        "cert": hex::encode(cert),
    }))
}

/// The daemon's `devices#enrolBootKey` answer for `args`: certify the boot key a device asked
/// about, seal its sibling half to the person's secret-sync key, name the person's other devices
/// that hold the half, and sign the acceptance of an unattended boot chain when one is asked for.
///
/// # Errors
/// This daemon keeps no root, the request does not read, or it could not sign.
pub fn enrol_boot_key(
    workspaces: Option<&Arc<ServiceWorkspaces>>,
    args: &Value,
) -> io::Result<Value> {
    let (person, _identity, _root) = person()?;
    let request: EnrolRequest = args
        .get("request")
        .and_then(Value::as_str)
        .and_then(|bytes| hex::decode(bytes).ok())
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .ok_or_else(|| io::Error::other("the request is not an enrolment"))?;
    request
        .verify()
        .map_err(|error| io::Error::other(format!("the enrolment request: {error}")))?;
    let workspaces =
        workspaces.ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?;
    let copies = workspaces.copies().clone();
    let devices = block_on_owned(async move {
        person_workspace::devices::of(&copies, &person).await
    });
    let enrollee = args
        .get("device")
        .and_then(Value::as_str)
        .and_then(|device| hex::decode(device).ok())
        .and_then(|device| <[u8; 32]>::try_from(device).ok());
    let now = tilo_clock::unix_now_ns();
    let root = tilo_identity::store::load_from_path(root_record()?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    // The boot key is certified as an ordinary device certificate that grants nothing: it can ask
    // for the half, never act as the device.
    let label = enrollee
        .and_then(|enrollee| {
            devices
                .listed(&person)
                .into_iter()
                .find(|listed| listed.cert.device_public_key == enrollee)
                .map(|listed| listed.device.called(&listed.cert))
        })
        .unwrap_or_else(|| "Boot key".to_owned());
    let mut cert = DeviceCert {
        user_public_key: person,
        device_public_key: request.boot_public_key,
        device_label: label,
        capabilities: DeviceCapabilities::nothing(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    let digest = device_cert_signing_digest(&cert)
        .map_err(|error| io::Error::other(format!("the boot key's certificate: {error}")))?;
    cert.signature = root
        .sign_digest(&digest)
        .map_err(|error| io::Error::other(error.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let secret_sync = root
        .secret_sync_key()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let secret_sync = secret_sync
        .public_key()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let sealed_half = seal_sibling_half(&secret_sync, &cert, &request.sibling_half)
        .map_err(|error| io::Error::other(error.to_string()))?;
    // Every one of the person's devices carries the half, named as the record names them.
    let siblings = devices
        .personal_devices(&person, now)
        .into_iter()
        .filter(|key| Some(*key) != enrollee)
        .map(|key| {
            let label = devices
                .listed(&person)
                .into_iter()
                .find(|listed| listed.cert.device_public_key == key)
                .map(|listed| listed.device.called(&listed.cert))
                .unwrap_or_default();
            json!({ "device": hex::encode(key), "label": label })
        })
        .collect::<Vec<_>>();
    let approval_signature = match args
        .get("approval_digest")
        .and_then(Value::as_str)
        .and_then(|digest| hex::decode(digest).ok())
        .and_then(|digest| <[u8; 32]>::try_from(digest).ok())
    {
        Some(digest) => Some(
            root.sign_digest(&digest)
                .map_err(|error| io::Error::other(error.to_string()))?,
        ),
        None => None,
    };
    Ok(json!({
        "cert": hex::encode(postcard::to_allocvec(&cert).unwrap_or_default()),
        "sealed_half": hex::encode(postcard::to_allocvec(&sealed_half).unwrap_or_default()),
        "siblings": siblings,
        "approval_signature": approval_signature.map(hex::encode),
    }))
}

/// The daemon's `devices#releaseHalf` answer for `args`: the boot's sibling half, opened with the
/// person's secret-sync key here and sealed to the boot's ephemeral key. The half appears here
/// and in the boot's hands alone.
///
/// # Errors
/// This daemon keeps no root, or the half is not one it sealed.
pub fn release_half(args: &Value) -> io::Result<Value> {
    let entry = |field: &str| -> io::Result<&str> {
        args.get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other(format!("{field} is needed")))
    };
    let cert: DeviceCert = postcard::from_bytes(&hex::decode(entry("cert")?).unwrap_or_default())
        .map_err(|error| io::Error::other(format!("the boot key's certificate: {error}")))?;
    let sealed: SealedSiblingHalf =
        postcard::from_bytes(&hex::decode(entry("sealed_half")?).unwrap_or_default())
            .map_err(|error| io::Error::other(format!("the sealed half: {error}")))?;
    let digest: [u8; 32] = hex::decode(entry("request_digest")?)
        .ok()
        .and_then(|digest| digest.try_into().ok())
        .ok_or_else(|| io::Error::other("the request's digest is 32 bytes in hex"))?;
    let ephemeral: [u8; 32] = hex::decode(entry("ephemeral")?)
        .ok()
        .and_then(|ephemeral| ephemeral.try_into().ok())
        .ok_or_else(|| io::Error::other("the boot's ephemeral key is 32 bytes in hex"))?;
    let root = tilo_identity::store::load_from_path(root_record()?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let secret_sync = root
        .secret_sync_key()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let half: Half = open_sibling_half(&secret_sync, &cert, &sealed)
        .map_err(|error| io::Error::other(format!("the half does not open: {error}")))?;
    let release: Release = seal_release(&ephemeral, &digest, &half)
        .map_err(|error| io::Error::other(format!("the release does not seal: {error}")))?;
    Ok(json!({
        "release": hex::encode(postcard::to_allocvec(&release).unwrap_or_default()),
    }))
}

/// The daemon's `devices#answerSignIn` answer for `args`: the person's proof for a site's
/// challenge, and the certificate their root gives the key the site asked to sign in with.
///
/// # Errors
/// This daemon keeps no root, the ask does not read, or it could not sign.
pub fn answer_sign_in(keys: &tilo_device_identity::IdentityPaths, args: &Value) -> io::Result<Value> {
    use tilo_server_client::certification::{GrantAsk, SignInAsk, answer_sign_in};
    let text = |field: &str| -> io::Result<String> {
        args.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other(format!("{field} is needed")))
    };
    let challenge: [u8; 32] = hex::decode(text("challenge")?)
        .ok()
        .and_then(|challenge| challenge.try_into().ok())
        .ok_or_else(|| io::Error::other("the challenge is 32 bytes in hex"))?;
    let grant = args.get("grant").and_then(Value::as_object);
    let grant_ask = grant
        .map(|grant| -> io::Result<GrantAsk<'_>> {
            let key: [u8; 32] = grant
                .get("key")
                .and_then(Value::as_str)
                .and_then(|key| hex::decode(key).ok())
                .and_then(|key| key.try_into().ok())
                .ok_or_else(|| io::Error::other("the grant's key is 32 bytes in hex"))?;
            Ok(GrantAsk {
                key,
                scope: AgentScope::parse(
                    grant.get("scope").and_then(Value::as_str).unwrap_or("all"),
                )
                .map_err(|error| io::Error::other(error.to_string()))?,
                expires_at_ns: grant.get("expires_at_ns").and_then(Value::as_u64),
                server_key: grant
                    .get("server_key")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                browser_pages: grant
                    .get("browser_pages")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .transpose()?;
    let audience = text("audience")?;
    let client = args
        .get("client")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let ask = SignInAsk {
        audience: &audience,
        challenge,
        client: &client,
        grant: grant_ask,
    };
    let root = tilo_identity::store::load_from_path(root_record()?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let device = tilo_device_identity::persist::load_device_key(keys)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let cert = tilo_device_identity::persist::read_device_cert(&keys.device_cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let signed = block_on_owned(async move {
        answer_sign_in(&root, &device, &cert.device_label, &ask).await
    })
    .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(json!({ "proof": signed.proof, "grant": signed.grant }))
}

/// What this daemon knows of the person's devices for `tilo-identity`
/// ([`tilo_identity::sources::DeviceTrust`]): the record's marks and the revocations its hosts
/// keep.
pub(crate) struct TrustOf(pub(crate) Weak<ServiceWorkspaces>);

impl tilo_identity::sources::DeviceTrust for TrustOf {
    fn revoked(&self, person: &[u8; 32], device: &[u8; 32]) -> Option<bool> {
        let workspaces = self.0.upgrade()?;
        (workspaces.copies().serving_certificate().user_public_key == *person)
            .then(|| workspaces.copies().blocked_keys().revokes_key(device))
    }

    fn enrolled(&self, cert: &DeviceCert) -> Option<bool> {
        let workspaces = self.0.upgrade()?;
        (workspaces.copies().serving_certificate().user_public_key == cert.user_public_key)
            .then(|| {
                workspaces
                    .copies()
                    .blocked_keys()
                    .certified(&cert.device_public_key)
            })
    }

    fn revocations(&self, person: &[u8; 32]) -> Vec<RevocationEntry> {
        let Some(workspaces) = self.0.upgrade() else {
            return Vec::new();
        };
        workspaces
            .copies()
            .revocations()
            .iter()
            .filter(|entry| entry.user_public_key == *person)
            .cloned()
            .collect()
    }

    fn generation(&self) -> u64 {
        let Some(workspaces) = self.0.upgrade() else {
            return 0;
        };
        workspaces.copies().blocked_keys().generation()
            ^ u64::try_from(workspaces.copies().revocations().len())
                .unwrap_or_default()
                .rotate_left(32)
    }
}

/// What certifies this device's agents' keys for the workspace service
/// ([`tilo_workspace_service::agents::agent_keys::AgentGrants`]): this daemon's root, and the
/// workspaces whose record it keeps the certificates in.
pub(crate) struct GrantsOf {
    pub(crate) workspaces: Weak<ServiceWorkspaces>,
    pub(crate) keys: tilo_device_identity::IdentityPaths,
}

impl tilo_workspace_service::agents::agent_keys::AgentGrants for GrantsOf {
    fn certify(
        &self,
        person: [u8; 32],
        grant: [u8; 32],
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
    ) -> Result<Option<DeviceCert>, String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this computer's daemon holds no workspaces")?;
        let copies = workspaces.copies().clone();
        let (agent, name) = (agent.to_owned(), name.to_owned());
        let certified = block_on_owned(async move {
            certify_agent_in(&copies, grant, Some((workspace, agent, name))).await
        })
        .map_err(|error| error.to_string())?;
        if certified.revoked {
            return Ok(None);
        }
        certified
            .cert
            .filter(|cert| cert.user_public_key == person)
            .map(Some)
            .ok_or_else(|| "the person's root gave no certificate for the key".to_owned())
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this computer's daemon holds no workspaces")?;
        Ok(workspaces
            .copies()
            .revocations()
            .iter()
            .filter(|entry| {
                entry.user_public_key == *person && entry.revoked_device_public_key == *key
            })
            .cloned()
            .next())
    }
}

/// The device actions this daemon answers that read or sign with the person's root: their names,
/// as the session, the CLI and the login tools call them.
pub use tilo_device_identity::login::{
    ANSWER_SIGN_IN, ENROL_BOOT_KEY, PERSONAL_DEVICES, RELEASE_HALF,
};

