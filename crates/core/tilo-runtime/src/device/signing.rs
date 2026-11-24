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

use super::services::device_root_record;
use super::workspaces::{ServiceWorkspaces, block_on_owned};

/// How long an agent's grant this daemon certifies for every workspace lasts, and how much of a
/// key's certificate may be left when it is certified again.
const AGENT_GRANT_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;
const AGENT_GRANT_RENEW_WITHIN_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// A person whose root this computer keeps, and the identity it signs for: the one root a
/// caller's person is, never another account's.
pub(crate) struct Rooted {
    pub person: [u8; 32],
    pub identity: String,
    pub root: tilo_identity::store::SigningIdentity,
}

impl Rooted {
    /// The person whose root the record at `record` is, with the identity it signs for.
    ///
    /// # Errors
    /// The root cannot be read, or its record is in no registry.
    pub(crate) fn at(record: &std::path::Path) -> io::Result<Self> {
        let dir = record
            .parent()
            .ok_or_else(|| io::Error::other("the root's record has no directory"))?;
        let root = tilo_identity::store::load_from_path(record.to_path_buf())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let person = root.group_public_key();
        let identity = tilo_identity::store::IdentityRegistryStore::at(dir)
            .load_reconciled()?
            .identities
            .into_values()
            .find(|profile| profile.record_path == record)
            .map(|profile| profile.id)
            .ok_or_else(|| io::Error::other("the root's record is in no registry"))?;
        Ok(Self {
            person,
            identity,
            root,
        })
    }
}

/// The person's root as this daemon keeps it, and the identity it signs for.
fn person() -> io::Result<Rooted> {
    Rooted::at(&device_root_record()?)
}

/// The certificate the person's root signs for `tab`, a key a sign-in ticket waits with, as it
/// signs one for any device of theirs: a browser's, or, for a key asked for something narrower
/// ([`tilo_device_identity::login::Tab::key_for`]), what [`certify_key_in`] certifies, listed
/// among the person's devices before it is answered.
///
/// # Errors
/// This daemon keeps no root, the tab is not one a certificate is signed for, or the person
/// revoked the key.
pub(crate) async fn sign_tab(
    workspaces: Option<&Arc<ServiceWorkspaces>>,
    tab: &tilo_device_identity::login::Tab,
) -> io::Result<DeviceCert> {
    // A browser approved as another account's is signed with that account's own root where this
    // computer keeps it; the person this device serves signs with the device's.
    let rooted = match super::services::root_caller() {
        Some(person) => Rooted::at(&super::services::root_record_for_person(person)?)?,
        None => person()?,
    };
    if let Some(key_for) = &tab.key_for {
        let key = tab
            .key()
            .map_err(|error| io::Error::other(format!("the tab's certificate: {error}")))?;
        let copies = workspaces
            .ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?
            .copies()
            .clone();
        let certified = certify_key_in(
            &copies,
            key,
            Some((key_for.clone(), tab.fitted_label())),
            rooted,
        )
        .await?;
        if certified.revoked {
            return Err(io::Error::other(
                "the person removed this key from their devices",
            ));
        }
        return certified
            .cert
            .ok_or_else(|| io::Error::other("the person's root gave no certificate for the key"));
    }
    let mut cert = tab
        .certificate(rooted.person, tilo_clock::unix_now_ns())
        .map_err(|error| io::Error::other(format!("the tab's certificate: {error}")))?;
    tilo_user_identity::sign_device_cert(&mut cert, &rooted.root)
        .map_err(|error| io::Error::other(format!("signing the tab's certificate: {error}")))?;
    Ok(cert)
}

/// A key this daemon certified with the person's root: its certificate, the person it belongs to,
/// and the time the certificate ends (`None`: until revoked).
#[derive(Debug)]
pub(crate) struct KeyCertified {
    pub identity: String,
    pub expires_at_ns: Option<u64>,
    pub cert: Option<DeviceCert>,
    pub revoked: bool,
}

/// Certify `key` with the person's root for what `asked` names, listed by its name
/// ([`tilo_device_identity::login::KeyFor`]: one agent in one workspace, or a device that reaches
/// one workspace alone), or, asked for nothing, as an agent's grant in every workspace of theirs.
/// A key the person revoked is answered as revoked; one certified for the same thing for long
/// enough already is answered as it stands. The certificate is kept among the person's devices
/// before it is answered: the one record that lists it, says what it reaches, and revokes it.
///
/// This is the one place the person's root certifies a key that is neither a browser's nor a
/// computer's they log in: every caller asks it, whatever the key is for.
pub(crate) async fn certify_key_in(
    copies: &Workspaces,
    key: [u8; 32],
    asked: Option<(tilo_device_identity::login::KeyFor, String)>,
    rooted: Rooted,
) -> io::Result<KeyCertified> {
    let Rooted {
        person,
        identity,
        root,
    } = rooted;
    let (capabilities, lifetime_ns, label) = match asked {
        Some((key_for, name)) => {
            let (capabilities, lifetime_ns) = key_for
                .capabilities()
                .map_err(|error| io::Error::other(error.to_string()))?;
            (capabilities, lifetime_ns, name)
        }
        None => (
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            Some(AGENT_GRANT_LIFETIME_NS),
            "Agent key".to_owned(),
        ),
    };
    let devices = person_workspace::devices::of(copies, &person).await;
    let listed = devices.listed(&person);
    if listed
        .iter()
        .any(|listed| listed.cert.device_public_key == key && listed.device.revoked(&listed.cert))
    {
        return Ok(KeyCertified {
            identity,
            expires_at_ns: None,
            cert: None,
            revoked: true,
        });
    }
    let now = tilo_clock::unix_now_ns();
    let lasting = listed
        .iter()
        .map(|listed| &listed.cert)
        .filter(|cert| cert.device_public_key == key && cert.capabilities == capabilities)
        .max_by_key(|cert| cert.expires_at_ns.unwrap_or(u64::MAX))
        .filter(|cert| {
            cert.expires_at_ns
                .is_none_or(|expires| expires.saturating_sub(now) > AGENT_GRANT_RENEW_WITHIN_NS)
        });
    if let Some(cert) = lasting {
        return Ok(KeyCertified {
            identity,
            expires_at_ns: cert.expires_at_ns,
            cert: Some(cert.clone()),
            revoked: false,
        });
    }
    let expires_at_ns = lifetime_ns.map(|lifetime| now.saturating_add(lifetime));
    let mut cert = DeviceCert {
        user_public_key: person,
        device_public_key: key,
        device_label: label,
        capabilities,
        issued_at_ns: now,
        expires_at_ns,
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
    Ok(KeyCertified {
        identity,
        expires_at_ns,
        cert: Some(cert),
        revoked: false,
    })
}

/// The daemon's `devices#certifyKey` answer for `args`
/// ([`tilo_device_identity::login::CERTIFY_KEY`]).
///
/// # Errors
/// This daemon keeps no root, `args` name no key or nothing it can be for, or it did not certify
/// the key.
pub fn certify_key(
    workspaces: Option<&Arc<ServiceWorkspaces>>,
    args: &Value,
    rooted: Rooted,
) -> io::Result<Value> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .and_then(|key| hex::decode(key).ok())
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .ok_or_else(|| io::Error::other("the key is named by its public key, as hex"))?;
    if args.get("for").and_then(Value::as_str) == Some("boot") {
        return certify_boot_key(workspaces, args, key, rooted);
    }
    let asked = args
        .get("for")
        .cloned()
        .map(serde_json::from_value::<tilo_device_identity::login::KeyFor>)
        .transpose()
        .map_err(|error| io::Error::other(format!("what the key is for: {error}")))?
        .map(|key_for| {
            let name = args.get("name").and_then(Value::as_str).map_or_else(
                || match &key_for {
                    tilo_device_identity::login::KeyFor::Agent(agent) => agent.agent.clone(),
                    tilo_device_identity::login::KeyFor::Workspace(_) => {
                        "Workspace computer".to_owned()
                    }
                },
                str::to_owned,
            );
            (key_for, name)
        });
    let workspaces =
        workspaces.ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?;
    let copies = workspaces.copies().clone();
    let certified =
        block_on_owned(async move { certify_key_in(&copies, key, asked, rooted).await })?;
    if certified.revoked {
        return Ok(json!({ "revoked": true }));
    }
    let cert = certified
        .cert
        .as_ref()
        .ok_or_else(|| io::Error::other("the person's root gave no certificate for the key"))?;
    let cert = tilo_user_identity::canonical::canonical_bytes(cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(json!({
        "identity": certified.identity,
        "expires_at_ns": certified.expires_at_ns,
        "cert": hex::encode(cert),
    }))
}

/// The `for: "boot"` case of [`certify_key`]: certify the boot key `key` that `request` proves,
/// seal its sibling half to the person's secret-sync key, name the person's other devices that
/// hold the half, and sign the acceptance of an unattended boot chain when one is asked for.
///
/// # Errors
/// This daemon keeps no root, the request does not read, the key is not the one it proves, or it
/// could not sign.
#[expect(
    clippy::too_many_lines,
    reason = "one boot key's certificate, its sealed half and the devices that carry it"
)]
fn certify_boot_key(
    workspaces: Option<&Arc<ServiceWorkspaces>>,
    args: &Value,
    key: [u8; 32],
    rooted: Rooted,
) -> io::Result<Value> {
    let Rooted {
        person,
        identity,
        root,
    } = rooted;
    let request: EnrolRequest = args
        .get("request")
        .and_then(Value::as_str)
        .and_then(|bytes| hex::decode(bytes).ok())
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .ok_or_else(|| io::Error::other("the request is not one a boot sent"))?;
    request
        .verify()
        .map_err(|error| io::Error::other(format!("the boot's request: {error}")))?;
    if request.boot_public_key != key {
        return Err(io::Error::other(
            "the key is not the one the boot's request proves",
        ));
    }
    let workspaces =
        workspaces.ok_or_else(|| io::Error::other("this daemon holds no workspaces"))?;
    let copies = workspaces.copies().clone();
    let devices =
        block_on_owned(async move { person_workspace::devices::of(&copies, &person).await });
    let enrollee = args
        .get("device")
        .and_then(Value::as_str)
        .and_then(|device| hex::decode(device).ok())
        .and_then(|device| <[u8; 32]>::try_from(device).ok());
    let now = tilo_clock::unix_now_ns();
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
        .listed(&person)
        .into_iter()
        .filter(|listed| {
            listed.cert.capabilities.is_personal_device()
                && tilo_user_identity::verify_device_cert_at(&listed.cert, now).is_ok()
                && !listed.device.revoked(&listed.cert)
        })
        .filter(|listed| Some(listed.cert.device_public_key) != enrollee)
        .map(|listed| {
            json!({
                "device": hex::encode(listed.cert.device_public_key),
                "label": listed.device.called(&listed.cert),
            })
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
    let cert = tilo_user_identity::canonical::canonical_bytes(&cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(json!({
        "identity": identity,
        "expires_at_ns": Value::Null,
        "cert": hex::encode(cert),
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
pub fn release_half(args: &Value, rooted: Rooted) -> io::Result<Value> {
    let entry = |field: &str| -> io::Result<&str> {
        args.get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other(format!("{field} is needed")))
    };
    let cert: DeviceCert =
        postcard::from_bytes(&hex::decode(entry("cert")?).unwrap_or_default())
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
    let secret_sync = rooted
        .root
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
pub fn answer_sign_in(
    keys: &tilo_device_identity::IdentityPaths,
    args: &Value,
    rooted: Rooted,
) -> io::Result<Value> {
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
        .map(|grant| -> io::Result<GrantAsk> {
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
                .ok_or_else(|| io::Error::other("the grant names no scope"))?,
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
    let root = rooted.root;
    let device = tilo_device_identity::persist::load_device_key(keys)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let cert = tilo_device_identity::persist::read_device_cert(&keys.device_cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let signed = block_on_owned(async move {
        let ask = SignInAsk {
            audience: &audience,
            challenge,
            client: &client,
            grant: grant_ask,
        };
        answer_sign_in(&root, &device, &cert.device_label, &ask).await
    })
    .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(json!({ "proof": signed.proof, "grant": signed.grant }))
}

/// The person's devices as this daemon holds them for `tilo-identity`
/// ([`tilo_identity::sources::DeviceTrust`]): the record's entries, each revocation verified against
/// the root that signed it, and the keys an entry confines to one workspace alone, as the workspace
/// service read them.
///
/// The daemon's own device is one of the person's by the certificate their root signed for the key
/// this device serves as, whether or not the record has reached it yet: a server signed in by a
/// ticket keeps the person's workspaces before a copy of their own one comes, and a host that
/// could not answer for itself would refuse the apps it serves. A revocation of the device in the
/// record still takes it back, at the next call, as it takes back any other device.
pub(crate) fn devices_seen(workspaces: &ServiceWorkspaces) -> tilo_identity::sources::DevicesSeen {
    let copies = workspaces.copies();
    let own = copies.device_cert();
    let keys = copies.blocked_keys();
    tilo_identity::sources::DevicesSeen::of_host(
        own.user_public_key,
        keys.certified_keys()
            .copied()
            .chain(std::iter::once(own.device_public_key)),
        keys.revocations().iter().cloned(),
    )
    .confining(keys.confined_keys())
}

/// What certifies this device's agents' keys for the workspace service
/// ([`tilo_workspace_service::agents::agent_keys::AgentGrants`]): this daemon's root, and the
/// workspaces whose record it keeps the certificates in.
pub(crate) struct GrantsOf {
    pub(crate) workspaces: Weak<ServiceWorkspaces>,
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
        // The person the workspace asks for is the one whose root signs, where this computer
        // keeps it: no other account's root signs an agent's key.
        let rooted = Rooted::at(
            &super::services::root_record_for_person(person).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let copies = workspaces.copies().clone();
        let asked =
            tilo_device_identity::login::KeyFor::Agent(tilo_device_identity::login::AgentAsked {
                workspace: workspace.to_string(),
                agent: agent.to_owned(),
            });
        let name = name.to_owned();
        let certified = block_on_owned(async move {
            certify_key_in(&copies, grant, Some((asked, name)), rooted).await
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
        let keys = workspaces.copies().blocked_keys();
        Ok(keys
            .revocations()
            .iter()
            .find(|entry| {
                entry.user_public_key == *person && entry.revoked_device_public_key == *key
            })
            .cloned())
    }
}
