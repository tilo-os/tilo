//! Separately consented website sessions in canonical sealed account custody.
//! Captured session material is not provider authentication and never joins the public accounts projection.
#[cfg(target_os = "linux")]
pub mod source;
use super::account_transfer::{Connector, PasswordSealedAccounts};
#[cfg(target_os = "linux")]
use accounts::Account;
use accounts::{AccountId, AccountStore, SecretVault};
use browser_continuity::sessions::Service;
#[cfg(target_os = "linux")]
use browser_continuity::sessions::{OriginStorage, SessionCookies};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::{io, path::Path};
#[cfg(target_os = "linux")]
use zeroize::Zeroize;

/// Public ciphertext-only declaration, authenticated by the prepared setup manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub schema: u32,
    #[serde(
        default = "legacy_protocol",
        skip_serializing_if = "is_legacy_protocol"
    )]
    pub consumer_protocol: u32,
    pub sessions: Vec<SelectedSession>,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: [u8; 32],
}
fn legacy_protocol() -> u32 {
    1
}
fn is_legacy_protocol(value: impl std::borrow::Borrow<u32>) -> bool {
    *value.borrow() == 1
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedSession {
    pub id: String,
    pub service: Service,
}
impl Declaration {
    /// # Errors
    /// Refuses unsupported declarations, duplicate IDs/services or oversized ciphertext.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || !matches!(self.consumer_protocol, 1 | 2)
            || self.sessions.is_empty()
            || self.sessions.len() > 2
            || self.sealed_bytes == 0
            || self.sealed_bytes > 8 * 1024 * 1024 + 1024
        {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut services = std::collections::BTreeSet::new();
        for session in &self.sessions {
            if !browser_continuity::sessions::valid_id(&session.id)
                || !ids.insert(&session.id)
                || !services.insert(session.service.as_str())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// # Errors
    /// Refuses changed ciphertext before authentication/import.
    pub fn verify_ciphertext(&self, artifact: &PasswordSealedAccounts) -> io::Result<()> {
        self.validate()?;
        if self.sealed_bytes != u64::try_from(artifact.as_bytes().len()).map_err(|_| invalid())?
            || self.ciphertext_sha256 != <[u8; 32]>::from(Sha256::digest(artifact.as_bytes()))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Retains only an owned private staging vault. Original browser stores remain read-only.
pub struct PreparedSessions {
    stage: tempfile::TempDir,
    identity: [u8; 32],
    binding: String,
    selected: Vec<SelectedSession>,
}
/// Each variant requires separate native inclusion consent before capture.
#[cfg(target_os = "linux")]
pub enum SelectedSource {
    Cookies(source::SourceGrant, Service),
    #[cfg(target_os = "linux")]
    DiscordOrigin(source::discord_origin::DiscordOriginGrant),
}
#[cfg(target_os = "linux")]
impl SelectedSource {
    fn kind(&self) -> (Service, bool) {
        match self {
            Self::Cookies(_, service) => (*service, false),
            #[cfg(target_os = "linux")]
            Self::DiscordOrigin(_) => (Service::Discord, true),
        }
    }
}
impl PreparedSessions {
    /// Explicit inclusion consent is required before this credential-reading operation.
    /// # Errors
    /// Refuses unavailable/changed sources, unsupported cookies or duplicate selections.
    #[cfg(target_os = "linux")]
    pub fn capture(
        identity: [u8; 32],
        binding: String,
        sources: Vec<(source::SourceGrant, Service)>,
    ) -> io::Result<Self> {
        Self::capture_selected(
            identity,
            binding,
            sources
                .into_iter()
                .map(|(grant, service)| SelectedSource::Cookies(grant, service))
                .collect(),
        )
    }
    /// Capture only explicitly selected material, independently of cookie availability.
    /// A selected source refusal fails the whole operation; another source never hides it.
    /// # Errors
    /// Refuses invalid bindings, duplicate material, changed sources or unsupported storage.
    #[cfg(target_os = "linux")]
    pub fn capture_selected(
        identity: [u8; 32],
        binding: String,
        sources: Vec<SelectedSource>,
    ) -> io::Result<Self> {
        if sources.is_empty()
            || sources.len() > 3
            || identity == [0; 32]
            || binding.is_empty()
            || binding.len() > 256
            || binding.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let mut kinds = std::collections::BTreeSet::new();
        for source in &sources {
            let (service, origin) = source.kind();
            if !kinds.insert((service.as_str(), origin)) {
                return Err(invalid());
            }
        }
        let mut material = std::collections::BTreeMap::new();
        for selected_source in sources {
            let (service, _) = selected_source.kind();
            let (session, cookie_digest) = material.entry(service.as_str()).or_insert_with(|| {
                (
                    SessionCookies {
                        schema: 1,
                        session_id: uuid::Uuid::new_v4().to_string(),
                        service,
                        identity_public_key: identity,
                        setup_binding: binding.clone(),
                        source_snapshot_digest: [0; 32],
                        cookies: Vec::new(),
                        origin_storage: Vec::new(),
                    },
                    None,
                )
            });
            match selected_source {
                SelectedSource::Cookies(grant, _) => {
                    let captured = source::read_selected(&grant, service)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    *cookie_digest = Some(captured.source_snapshot_digest);
                    session.cookies = captured.cookies;
                }
                #[cfg(target_os = "linux")]
                SelectedSource::DiscordOrigin(grant) => {
                    let captured = source::discord_origin::read_selected(&grant)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    session.schema = 2;
                    session.origin_storage.push(OriginStorage {
                        origin: source::discord_origin::ORIGIN.into(),
                        key: source::discord_origin::KEY.into(),
                        value: (*captured.value).clone(),
                        source_snapshot_digest: captured.source_snapshot_digest,
                    });
                }
            }
        }
        let stage = private_stage()?;
        let mut store = AccountStore::new();
        let mut vault = SecretVault::open_in(stage.path());
        let mut selected = Vec::new();
        for (_, (mut session, cookie_digest)) in material {
            session.source_snapshot_digest = if session.origin_storage.is_empty() {
                cookie_digest.ok_or_else(invalid)?
            } else {
                let mut digest = Sha256::new();
                digest.update(b"tilo-selected-service-session-sources/v2\0");
                digest.update(session.service.as_str().as_bytes());
                if let Some(cookie_digest) = cookie_digest {
                    digest.update(b"\0cookies\0");
                    digest.update(cookie_digest);
                }
                for origin in &session.origin_storage {
                    digest.update(b"\0discord-origin\0");
                    digest.update(origin.source_snapshot_digest);
                }
                digest.finalize().into()
            };
            let id = session.session_id.clone();
            let service = session.service;
            let mut secret = account_custody::service_sessions::service_session_secret(&session)
                .map_err(|_| invalid())?;
            let mut account = Account::new(
                id.clone(),
                account_custody::service_sessions::SERVICE_SESSION_TYPE,
            );
            account.config =
                serde_json::json!({"service":service.as_str(), "setup_binding":binding});
            vault.put(&account.id, &secret);
            secret.password.zeroize();
            if let Some(token) = secret.token.as_mut() {
                token.zeroize();
            }
            store.upsert(account);
            selected.push(SelectedSession { id, service });
        }
        vault.try_save().map_err(|_| invalid())?;
        store.save_in(stage.path())?;
        Ok(Self {
            stage,
            identity,
            binding,
            selected,
        })
    }
    /// # Errors
    /// Refuses invalid passwords/bindings or changed staging custody.
    pub fn seal(&self, password: &[u8]) -> io::Result<(PasswordSealedAccounts, Declaration)> {
        let ids: Vec<_> = self
            .selected
            .iter()
            .map(|v| AccountId::from(v.id.clone()))
            .collect();
        let custodian = account_custody::Custodian::for_root(Some(self.stage.path().to_owned()));
        let mut consumer_protocol = 1;
        for id in &ids {
            let session = custodian
                .service_session(id, self.identity, &self.binding)
                .map_err(|_| invalid())?;
            consumer_protocol = consumer_protocol.max(session.schema);
        }
        let artifact = Connector::new(self.stage.path().to_owned())
            .seal_selected_accounts_with_password(&ids, password, &transport_binding(&self.binding))
            .map_err(|_| invalid())?;
        let declaration = Declaration {
            schema: 1,
            consumer_protocol,
            sessions: self.selected.clone(),
            sealed_bytes: u64::try_from(artifact.as_bytes().len()).map_err(|_| invalid())?,
            ciphertext_sha256: Sha256::digest(artifact.as_bytes()).into(),
        };
        declaration.verify_ciphertext(&artifact)?;
        Ok((artifact, declaration))
    }
}
pub fn transport_binding(binding: &str) -> Vec<u8> {
    let mut value = b"tilo-selected-service-sessions/v1\0".to_vec();
    value.extend_from_slice(binding.as_bytes());
    value
}
/// Authenticate into fresh private quarantine, then validate all secret bindings before target writes.
/// # Errors
/// Refuses changed declarations, passwords, identity, setup or secret shapes.
pub fn validate_artifact(
    artifact: &PasswordSealedAccounts,
    declaration: &Declaration,
    password: &[u8],
    identity: [u8; 32],
    binding: &str,
) -> io::Result<()> {
    declaration.verify_ciphertext(artifact)?;
    let stage = private_stage()?;
    import_checked_stage(
        artifact,
        declaration,
        password,
        identity,
        binding,
        stage.path(),
    )?;
    Ok(())
}
/// Import a previously validated artifact into an explicitly granted private target config root.
/// # Errors
/// Refuses ciphertext, selection or custody conflicts; never overwrites unrelated accounts.
pub fn import(
    artifact: &PasswordSealedAccounts,
    declaration: &Declaration,
    password: &[u8],
    identity: [u8; 32],
    binding: &str,
    target: &Path,
) -> io::Result<()> {
    validate_artifact(artifact, declaration, password, identity, binding)?;
    let pin = match std::fs::symlink_metadata(target) {
        Ok(_) => super::migration::PinnedDirectory::open_absolute(target)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = super::migration::PinnedDirectory::open_absolute(
                target.parent().ok_or_else(invalid)?,
            )?;
            parent.validate_destination()?;
            super::destination_security::check_private_file(&parent.file)?;
            parent.create_child(target.file_name().ok_or_else(invalid)?)?
        }
        Err(error) => return Err(error),
    };
    pin.validate_destination()?;
    super::destination_security::check_private_file(&pin.file)?;
    import_checked_stage(artifact, declaration, password, identity, binding, target)?;
    pin.validate_destination()
}
fn import_checked_stage(
    artifact: &PasswordSealedAccounts,
    declaration: &Declaration,
    password: &[u8],
    identity: [u8; 32],
    binding: &str,
    target: &Path,
) -> io::Result<()> {
    declaration.verify_ciphertext(artifact)?;
    let ids: Vec<_> = declaration
        .sessions
        .iter()
        .map(|v| AccountId::from(v.id.clone()))
        .collect();
    Connector::new(target.to_owned())
        .import_password_sealed_accounts_matching(
            artifact,
            password,
            &transport_binding(binding),
            &ids,
        )
        .map_err(|_| invalid())?;
    let custodian = account_custody::Custodian::for_root(Some(target.to_owned()));
    let mut consumer_protocol = 1;
    for selected in &declaration.sessions {
        let session = custodian
            .service_session(&AccountId::from(selected.id.clone()), identity, binding)
            .map_err(|_| invalid())?;
        if session.service != selected.service {
            return Err(invalid());
        }
        consumer_protocol = consumer_protocol.max(session.schema);
    }
    if consumer_protocol != declaration.consumer_protocol {
        return Err(invalid());
    }
    Ok(())
}
fn private_stage() -> io::Result<tempfile::TempDir> {
    #[cfg(target_os = "linux")]
    {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Website session custody is unavailable on this platform",
        ))
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Selected website session is unavailable",
    )
}

#[cfg(test)]
mod declaration_tests {
    use super::*;

    #[test]
    fn declaration_validation_is_portable_and_rejects_duplicate_services() {
        let mut declaration = Declaration {
            schema: 1,
            consumer_protocol: 2,
            sessions: vec![SelectedSession {
                id: uuid::Uuid::new_v4().to_string(),
                service: Service::Discord,
            }],
            sealed_bytes: 128,
            ciphertext_sha256: [1; 32],
        };
        declaration.validate().unwrap();
        declaration.sessions.push(SelectedSession {
            id: uuid::Uuid::new_v4().to_string(),
            service: Service::Discord,
        });
        assert!(declaration.validate().is_err());
        declaration.sessions.pop();
        declaration.consumer_protocol = 3;
        assert!(declaration.validate().is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn selected_firefox_session_seals_reopens_and_refuses_foreign_identity_before_writes() {
        let source = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(source.path().join("cookies.sqlite")).unwrap();
        db.execute_batch("PRAGMA user_version=17; CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER); INSERT INTO moz_cookies VALUES(1,'.slack.com','d','selected-secret-marker','/',2000000123000,1,1,256,'',0,0);").unwrap();
        drop(db);
        let original = std::fs::read(source.path().join("cookies.sqlite")).unwrap();
        let grant = source::SourceGrant::new(
            std::fs::File::open(source.path()).unwrap(),
            browser_continuity::Provider::Firefox,
        )
        .unwrap();
        let prepared = PreparedSessions::capture(
            [7; 32],
            "exact-operation".into(),
            vec![(grant, Service::Slack)],
        )
        .unwrap();
        let (artifact, declaration) = prepared
            .seal(b"a durable session transfer password")
            .unwrap();
        assert!(
            !artifact
                .as_bytes()
                .windows(b"selected-secret-marker".len())
                .any(|v| v == b"selected-secret-marker")
        );
        assert!(
            !serde_json::to_string(&declaration)
                .unwrap()
                .contains("selected-secret-marker")
        );
        drop(prepared);
        let public_target = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o755))
            .tempdir()
            .unwrap();
        let mode_before = std::fs::metadata(public_target.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            import(
                &artifact,
                &declaration,
                b"a durable session transfer password",
                [7; 32],
                "exact-operation",
                public_target.path()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_dir(public_target.path()).unwrap().count(), 0);
        assert_eq!(
            std::fs::metadata(public_target.path())
                .unwrap()
                .permissions()
                .mode(),
            mode_before
        );
        let target = private_stage().unwrap();
        assert!(
            import(
                &artifact,
                &declaration,
                b"a durable session transfer password",
                [8; 32],
                "exact-operation",
                target.path()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
        assert!(
            import(
                &artifact,
                &declaration,
                b"a different session transfer password",
                [7; 32],
                "exact-operation",
                target.path()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
        import(
            &artifact,
            &declaration,
            b"a durable session transfer password",
            [7; 32],
            "exact-operation",
            target.path(),
        )
        .unwrap();
        import(
            &artifact,
            &declaration,
            b"a durable session transfer password",
            [7; 32],
            "exact-operation",
            target.path(),
        )
        .unwrap();
        let fresh_child = target.path().join("fresh-config");
        import(
            &artifact,
            &declaration,
            b"a durable session transfer password",
            [7; 32],
            "exact-operation",
            &fresh_child,
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&fresh_child)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let session = account_custody::Custodian::for_root(Some(target.path().to_owned()))
            .service_session(
                &AccountId::from(declaration.sessions[0].id.clone()),
                [7; 32],
                "exact-operation",
            )
            .unwrap();
        assert_eq!(session.cookies[0].value, "selected-secret-marker");
        assert_eq!(session.cookies[0].expires_unix_seconds, Some(2_000_000_123));
        assert_eq!(
            std::fs::read(source.path().join("cookies.sqlite")).unwrap(),
            original
        );
        let mut changed = declaration;
        changed.sessions[0].service = Service::Discord;
        let fresh = tempfile::tempdir().unwrap();
        assert!(
            import(
                &artifact,
                &changed,
                b"a durable session transfer password",
                [7; 32],
                "exact-operation",
                fresh.path()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(fresh.path()).unwrap().count(), 0);
    }
}

#[cfg(all(
    any(test, feature = "test-support"),
    target_os = "linux",
    feature = "native-capture-qualification"
))]
pub mod official_format_native;
