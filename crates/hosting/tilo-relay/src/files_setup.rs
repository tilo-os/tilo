//! Host first-run preparation. Identity remains in its existing custody;
//! creation is a separate, explicit choice from unavailable recovery.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs::workspace_authority::{AuthorityRequest, WorkspaceAuthority, WorkspaceAuthorityEnvelope};
use tilo_user_identity::{IdentityPaths, persist};

#[derive(clap::Args, Debug)]
pub(super) struct InitializeOptions {
    #[arg(long)]
    pub cache_dir: PathBuf,
    #[arg(long)]
    pub identity_root: PathBuf,
    #[arg(long)]
    pub identity_dir: PathBuf,
    #[arg(long)]
    pub acl: PathBuf,
    /// Explicitly create NEW storage; existing resources are not imported or replaced.
    #[arg(long)]
    pub confirm_new_workspace: bool,
    /// Acknowledge that remembered recovery candidates have not supplied a verified copy.
    #[arg(long)]
    pub acknowledge_unavailable_recovery: bool,
}

mod recovery;
pub(super) use recovery::{JoinOptions, join};

fn existing_person(paths: &IdentityPaths) -> Result<[u8; 32]> {
    let key = persist::load_device_key(paths).context("read existing certified device key")?;
    let person = persist::read_public_key(&paths.user_pubkey)?;
    let cert = persist::read_device_cert(&paths.device_cert)?;
    tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns())?;
    anyhow::ensure!(
        cert.user_public_key == person && cert.device_public_key == key.verifying_key().to_bytes(),
        "existing identity projection is inconsistent"
    );
    anyhow::ensure!(
        cert.capabilities.can_own_workspaces,
        "existing device cannot own workspaces"
    );
    let revocations = fs::load_revocation_list(paths)?;
    anyhow::ensure!(
        !revocations.is_revoked_at(&cert.device_public_key, tilo_clock::unix_now_ns()),
        "existing device is revoked"
    );
    Ok(person)
}

fn protected_identity(dir: &Path, person: [u8; 32]) -> Result<runtime_identity::SigningIdentity> {
    let record = runtime_identity::resolve_active_identity(dir)
        .context("existing person identity is unavailable; do not create a replacement")?;
    let identity = runtime_identity::load_from_path(record).map_err(|e| {
        anyhow::anyhow!("open existing identity in its normal signing session: {e}")
    })?;
    anyhow::ensure!(
        identity.group_public_key() == person,
        "active identity belongs to another person"
    );
    anyhow::ensure!(
        identity.is_single_device(),
        "threshold identity requires its existing signing ceremony"
    );
    Ok(identity)
}

fn capabilities(acl: &Path) -> Result<Vec<collab_types::CapOverride>> {
    serde_json::from_slice(&std::fs::read(acl)?).context("read existing Files capability policy")
}

fn require_files_access(person: [u8; 32], policy: &[collab_types::CapOverride]) -> Result<()> {
    let access = fs::WorkspaceAcl::from_capabilities(person, policy, &[], None);
    anyhow::ensure!(
        access.permits(&person, "/", fs::acl::Access::Read)
            && access.permits(&person, "/", fs::acl::Access::Write),
        "existing policy does not authorize this person to read/write the Files workspace; request an exact Files capability through the normal permission surface first"
    );
    Ok(())
}

pub(super) fn check(root: &Path, cache: &Path) -> Result<()> {
    let paths = IdentityPaths::under(root);
    let person = existing_person(&paths)?;
    anyhow::ensure!(
        cache.join("namespace.bin").is_file(),
        "no saved Files namespace; recovery or explicit initialization is required"
    );
    let key = persist::load_device_key(&paths)?;
    let ticket = cache.join("workspace-ticket");
    let workspace = if ticket.try_exists()? {
        fs::Ticket::decode(&std::fs::read_to_string(ticket)?)?.workspace_id()
    } else {
        fs::Ticket::workspace_id_for_topic(key.verifying_key().to_bytes())
    };
    let envelope: WorkspaceAuthorityEnvelope =
        postcard::from_bytes(&std::fs::read(cache.join("workspace-authority.bin"))?)?;
    envelope.verify(workspace, person)?;
    anyhow::ensure!(!envelope.tombstoned, "saved Files authority is revoked");
    require_files_access(person, &envelope.capabilities)?;
    println!("Existing primary Files workspace {workspace} is authorized; no state changed.");
    Ok(())
}

fn new_cache(cache: &Path) -> Result<()> {
    anyhow::ensure!(cache.is_absolute(), "Files cache path must be absolute");
    anyhow::ensure!(
        !cache.try_exists()?,
        "Files destination already exists; preserve it and inspect recovery instead"
    );
    let parent = cache.parent().context("Files cache has no parent")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        std::fs::DirBuilder::new().mode(0o700).create(cache)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(parent)?;
        std::fs::create_dir(cache)?;
    }
    Ok(())
}

fn prepare_new(cache: &Path, device: [u8; 32], envelope: WorkspaceAuthorityEnvelope) -> Result<()> {
    let workspace = fs::Ticket::workspace_id_for_topic(device);
    envelope.verify(workspace, envelope.governor)?;
    let mut namespace = fs::Namespace::new(hex::encode(device))?;
    let bytes = namespace.save();
    new_cache(cache)?;
    let authority = WorkspaceAuthority::open(
        cache.join("workspace-authority.bin"),
        workspace,
        envelope.governor,
        envelope.governor,
        Vec::new(),
    )?;
    authority.apply(envelope)?;
    // Namespace last: the service cannot resume an incomplete preparation.
    tilo_files::native::atomic::write_with_mode(&cache.join("namespace.bin"), &bytes, 0o600)?;
    Ok(())
}

pub(super) fn initialize(options: &InitializeOptions) -> Result<()> {
    anyhow::ensure!(
        options.confirm_new_workspace,
        "new initialization requires --confirm-new-workspace; it is not recovered data"
    );
    anyhow::ensure!(
        !options.cache_dir.try_exists()?,
        "Files destination already exists; preserve it"
    );
    let paths = IdentityPaths::under(&options.identity_root);
    let person = existing_person(&paths)?;
    for registry in [
        tilo_relay::home_peer_registry_path(&options.identity_root),
        options.identity_root.join("home-peers.json"),
    ] {
        if registry.try_exists()? {
            let peers: Vec<tilo_relay::HomePeerRegistration> =
                serde_json::from_slice(&std::fs::read(registry)?)?;
            anyhow::ensure!(
                peers.is_empty() || options.acknowledge_unavailable_recovery,
                "remembered recovery candidates remain; obtain explicit new-workspace confirmation despite unavailable recovery"
            );
        }
    }
    let policy = capabilities(&options.acl)?;
    require_files_access(person, &policy)?;
    let identity = protected_identity(&options.identity_dir, person)?;
    let key = persist::load_device_key(&paths)?;
    let device = key.verifying_key().to_bytes();
    let envelope = WorkspaceAuthorityEnvelope::sign_with(
        fs::Ticket::workspace_id_for_topic(device),
        1,
        policy,
        false,
        person,
        |bytes| {
            identity
                .sign(bytes)
                .map_err(|e| anyhow::anyhow!("sign existing policy: {e}"))
        },
    )?;
    prepare_new(&options.cache_dir, device, envelope)?;
    println!(
        "Prepared NEW empty Files namespace for existing device workspace {}. Existing host files and recovery candidates were not imported or replaced.",
        fs::Ticket::workspace_id_for_topic(device)
    );
    Ok(())
}

pub(super) fn publish(acl: &Path) -> Result<()> {
    let paths = fs::identity_paths();
    let person = existing_person(&paths)?;
    let dir = std::env::var_os("TILO_IDENTITY_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
                .map(|p| p.join("tilo/identity"))
        })
        .context("existing identity directory unavailable")?;
    let identity = protected_identity(&dir, person)?;
    let policy = capabilities(acl)?;
    tilo_rt::block_on(async move {
        let (workspace, socket) = fs::workspace_authority::find_local_authority().await?;
        let current = fs::workspace_authority::local_authority_request(
            &socket,
            AuthorityRequest {
                workspace,
                publish: None,
            },
        )
        .await?
        .authority;
        let revision = current.map_or(1, |e| e.revision.saturating_add(1));
        let envelope = WorkspaceAuthorityEnvelope::sign_with(
            workspace,
            revision,
            policy,
            false,
            person,
            |bytes| {
                identity
                    .sign(bytes)
                    .map_err(|e| anyhow::anyhow!("sign existing policy: {e}"))
            },
        )?;
        let accepted = fs::workspace_authority::local_authority_request(
            &socket,
            AuthorityRequest {
                workspace,
                publish: Some(envelope.clone()),
            },
        )
        .await?
        .authority;
        anyhow::ensure!(
            accepted.as_ref() == Some(&envelope),
            "concurrent authority publication; retry"
        );
        Ok(())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_grant_never_substitutes_for_files_authorization() {
        use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Expiry, Scope, Subject};
        let person = [41; 32];
        let mut grant = CapOverride {
            id: String::default(),
            subject: Subject::User(Ed25519PublicKey(person)),
            scope: Scope::Device("camera".into()),
            access: Access::Write,
            effect: Effect::Grant,
            expiry: Expiry::Never,
            granted_by: None,
        };
        assert!(require_files_access(person, std::slice::from_ref(&grant)).is_err());
        grant.scope = Scope::FsPath("/".into());
        require_files_access(person, std::slice::from_ref(&grant))
            .expect("explicit existing Files grant");
        assert!(require_files_access([42; 32], &[grant]).is_err());
    }

    #[test]
    fn normal_files_scope_authorizes_namespace_without_host_root_grant() {
        use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Expiry, Scope, Subject};
        let person = [41; 32];
        let read = CapOverride::grant(
            Subject::User(Ed25519PublicKey(person)),
            Scope::FsPath("/var/workspace".into()),
            Access::Read,
        );
        assert!(require_files_access(person, std::slice::from_ref(&read)).is_err());
        let mut write = read.clone();
        write.access = Access::Write;
        require_files_access(person, std::slice::from_ref(&write))
            .expect("normal Edit files includes Read without Admin");
        let mut ui_person = write.clone();
        ui_person.subject = Subject::person(hex::encode(person));
        require_files_access(person, &[ui_person])
            .expect("normal Permissions member subject matches certified person");
        let policy = vec![read, write.clone()];
        require_files_access(person, &policy).expect("normal scoped Read/Write choice");
        assert!(require_files_access([42; 32], &policy).is_err());
        assert!(
            policy
                .iter()
                .all(|cap| cap.scope == Scope::FsPath("/var/workspace".into()))
        );
        write.scope = Scope::FsPath("/var/workspace/documents".into());
        assert!(require_files_access(person, std::slice::from_ref(&write)).is_err());
        write.scope = Scope::FsPath("/var/workspace-other".into());
        assert!(require_files_access(person, std::slice::from_ref(&write)).is_err());
        write.scope = Scope::FsPath("/var/workspace".into());
        write.expiry = Expiry::AtUnix(1);
        assert!(require_files_access(person, std::slice::from_ref(&write)).is_err());
        let mut restricted = policy;
        write.expiry = Expiry::Never;
        write.scope = Scope::FsPath("/var".into());
        write.effect = Effect::Restrict;
        write.access = Access::Read;
        restricted.push(write);
        assert!(require_files_access(person, &restricted).is_err());
    }

    #[test]
    fn prepared_namespace_resumes_existing_signed_policy_without_root_export() {
        let temp = tempfile::tempdir().expect("isolated fixture");
        let cache = temp.path().join("files");
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let device = [42; 32];
        let workspace = fs::Ticket::workspace_id_for_topic(device);
        let envelope = WorkspaceAuthorityEnvelope::sign(workspace, 1, Vec::new(), false, &root)
            .expect("signed existing policy");
        prepare_new(&cache, device, envelope.clone()).expect("explicit new fixture");
        let bytes = std::fs::read(cache.join("namespace.bin")).expect("durable namespace");
        let mut restored =
            fs::Namespace::from_bytes("restart", &bytes).expect("real CRDT namespace");
        assert!(!restored.save().is_empty());
        let authority = WorkspaceAuthority::open(
            cache.join("workspace-authority.bin"),
            workspace,
            envelope.governor,
            envelope.governor,
            Vec::new(),
        )
        .expect("verified authority");
        assert_eq!(authority.current(), Some(envelope.clone()));
        assert!(!cache.join("user-identity").exists());
        assert!(prepare_new(&cache, device, envelope).is_err());
        assert_eq!(
            std::fs::read(cache.join("namespace.bin")).expect("retained namespace"),
            bytes
        );
    }

    #[test]
    fn certified_saved_workspace_check_requires_exact_signed_files_access() {
        use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Expiry, Scope, Subject};
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let temp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(temp.path().join("identity"));
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let person = root.verifying_key().to_bytes();
        persist::write_public_key(&paths.user_pubkey, &person).unwrap();
        persist::write_device_key(&paths.device_privkey, &device).unwrap();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "isolated source fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        let grant = CapOverride {
            id: String::default(),
            subject: Subject::User(Ed25519PublicKey(person)),
            scope: Scope::FsPath("/var/workspace".into()),
            access: Access::Write,
            effect: Effect::Grant,
            expiry: Expiry::Never,
            granted_by: None,
        };
        let workspace = fs::Ticket::workspace_id_for_topic(device.verifying_key().to_bytes());
        let envelope =
            WorkspaceAuthorityEnvelope::sign(workspace, 1, vec![grant], false, &root).unwrap();
        let cache = temp.path().join("files");
        prepare_new(&cache, device.verifying_key().to_bytes(), envelope.clone()).unwrap();
        check(&temp.path().join("identity"), &cache).expect("existing certified Files primary");
        let bytes = std::fs::read(cache.join("namespace.bin")).unwrap();
        assert!(!paths.user_privkey.exists(), "no root export is required");
        let mut revoked = envelope.capabilities;
        revoked[0].effect = Effect::Restrict;
        let denied = WorkspaceAuthorityEnvelope::sign(workspace, 2, revoked, false, &root).unwrap();
        let authority = WorkspaceAuthority::open(
            cache.join("workspace-authority.bin"),
            workspace,
            person,
            person,
            Vec::new(),
        )
        .unwrap();
        authority.apply(denied).unwrap();
        assert!(check(&temp.path().join("identity"), &cache).is_err());
        assert_eq!(std::fs::read(cache.join("namespace.bin")).unwrap(), bytes);
        assert!(!cache.join("mount").exists());
        assert!(!cache.join("workspace-ticket").exists());
        cert.signature[0] ^= 1;
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        assert!(existing_person(&paths).is_err());
    }

    #[test]
    fn initialization_requires_explicit_choice_before_any_identity_or_storage_access() {
        let temp = tempfile::tempdir().expect("isolated fixture");
        let options = InitializeOptions {
            cache_dir: temp.path().join("files"),
            identity_root: temp.path().join("identity"),
            identity_dir: temp.path().join("records"),
            acl: temp.path().join("acl"),
            confirm_new_workspace: false,
            acknowledge_unavailable_recovery: false,
        };
        assert!(initialize(&options).is_err());
        assert!(!options.cache_dir.exists());
        assert!(!options.identity_root.exists());
    }
}
