//! A person whose root this computer holds (`tilo login --new-identity`),
//! and what that root lets this computer do without a browser: approve the
//! key `tilo keys create` asks for, as the person's Tilo on the web does.
//!
//! The person is this account's one Tilo identity, kept where its session
//! keeps it: their identity in `$XDG_CONFIG_HOME/tilo/identity` with its secret
//! in the platform's secret store (or its private-file fallback), and this
//! computer's device key and the certificate the root signed for it under the
//! device's identity root
//! ([`collaboration_identity_root`](tilo_identity::provisioning::collaboration_identity_root)).
//! A Tilo session started on this account is that same person.
//! Approving a key asks the site only to take the person's proof and the key's
//! certificate: nothing is read from the site's own entry, and no site's host
//! is dialled.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::{load_device_key, read_device_cert};
use tilo_identity::provisioning::IdentityStorage;
use tilo_identity::store::SigningIdentity;
use tilo_user_identity::StatementSigner as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, SignInProof};

use crate::login;

/// The person this computer holds the root of.
pub struct Person {
    /// Their root, from the identity store it is kept in.
    pub root: SigningIdentity,
    pub device: SigningKey,
    /// The certificate the root signed for [`Self::device`].
    pub cert: DeviceCert,
    /// How the person is named: empty while they gave no name.
    pub name: String,
}

impl Person {
    /// Who the person is in a sentence said to them: their name, or "you" while they gave none.
    #[must_use]
    pub fn who(&self) -> &str {
        if self.name.is_empty() {
            "you"
        } else {
            &self.name
        }
    }

    /// Whose a thing of theirs is, in a sentence said to them: "Ada's", or "your".
    #[must_use]
    pub fn whose(&self) -> String {
        if self.name.is_empty() {
            "your".to_owned()
        } else {
            format!("{}'s", self.name)
        }
    }

    /// Their user public key, hex.
    pub fn user(&self) -> String {
        hex::encode(self.public_key())
    }

    /// Their user public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.root.public_key()
    }
}

/// This account's identity directory, the session's own.
pub fn dir() -> Result<PathBuf> {
    tilo_identity::store::identity_dir_from_env().context("this account has no identity directory")
}

/// This computer's device key and certificates, the session's own.
pub fn paths() -> IdentityPaths {
    tilo_identity::provisioning::collaboration_identity_paths()
}

/// The active identity's file, when this computer holds its root.
fn root_identity() -> Option<PathBuf> {
    let active = tilo_identity::store::resolve_active_identity(&dir().ok()?)?;
    tilo_identity::store::load_metadata_from_path(&active)
        .is_ok_and(|metadata| metadata.single_device)
        .then_some(active)
}

/// Whether this computer keeps a person's root, without reading it: a process
/// that reads it makes itself undumpable ([`held`]).
pub fn holds_root() -> bool {
    root_identity().is_some()
}

/// The person kept here, if this computer holds their root.
pub fn held() -> Result<Option<Person>> {
    let Some(active) = root_identity() else {
        return Ok(None);
    };
    let identity = tilo_identity::store::load_from_path(active.clone())
        .map_err(|error| anyhow!("{} cannot be opened: {error}", active.display()))?;
    let name = identity.display_name().trim();
    let name = if tilo_identity::store::is_chosen_display_name(name) {
        name.to_owned()
    } else {
        String::new()
    };
    let root = identity;
    let user = root.public_key();
    let paths = paths();
    let cert = read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| read_device_cert(&paths.device_cert))
        .ok()
        .filter(|cert| cert.user_public_key == user)
        .with_context(|| {
            format!(
                "this computer holds no certificate of {} for its device: start Tilo here once                  so it certifies this device",
                hex::encode(user)
            )
        })?;
    Ok(Some(Person {
        root,
        device: load_device_key(&paths)?,
        cert,
        name,
    }))
}

/// The login an earlier tilo kept as `device.json`, with its key in a file of its own beside it.
#[derive(serde::Deserialize)]
struct EarlierLogin {
    site: String,
    host_origin: String,
    workspace: String,
    #[serde(default)]
    workspace_name: Option<String>,
    agent: String,
    /// Hex of the postcard certificate the root signed for the earlier key.
    cert: String,
    /// Hex of the host's postcard certificate.
    server_cert: String,
    #[serde(default)]
    server_address: Option<tilo_device_identity::login::ServerAddress>,
}

/// Convert, once, what an earlier tilo kept as plain files in this account's state folder: the
/// person's root (`tilo/identity/user-identity/privkey.bin`) becomes the identity store's record
/// under the same key, certifying this computer's one device key, and its login
/// (`tilo/device/device.json`) becomes this computer's login with that certificate. Each file is
/// removed once converted, so the root has one home and nothing reads the old files again.
///
/// Where the store already keeps another person, the earlier root is the one this account acts
/// as from now on only when the earlier login is that person's: the workspaces this computer
/// works in are theirs. The other person's record stays in the store as it was.
///
/// # Errors
/// The files are damaged, the computer holds another person's identity and no earlier login of
/// this one, or the login is of a person whose root is not here.
pub fn convert_earlier() -> Result<()> {
    let place = login::Place::here(paths())?;
    if let Some(state) = place.dir().parent() {
        convert_earlier_root(&state.join("identity/user-identity"), &place)?;
    }
    convert_earlier_login(&place)
}

/// Remove `files`, those that are there.
fn remove(files: &[&std::path::Path]) -> Result<()> {
    for file in files {
        match std::fs::remove_file(file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(anyhow!(error).context(format!("remove {}", file.display())));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The login an earlier tilo kept in `place`, if it kept one.
fn earlier_login(place: &login::Place) -> Result<Option<(EarlierLogin, DeviceCert, DeviceCert)>> {
    let file = place.dir().join("device.json");
    if !file.exists() {
        return Ok(None);
    }
    let earlier: EarlierLogin = serde_json::from_slice(
        &std::fs::read(&file).with_context(|| format!("read {}", file.display()))?,
    )
    .with_context(|| format!("{} is damaged", file.display()))?;
    let cert = |hexed: &str| -> Result<DeviceCert> {
        hex::decode(hexed)
            .ok()
            .and_then(|bytes| postcard::from_bytes(&bytes).ok())
            .with_context(|| format!("{} holds a damaged certificate", file.display()))
    };
    let (own, host) = (cert(&earlier.cert)?, cert(&earlier.server_cert)?);
    Ok(Some((earlier, own, host)))
}

/// The person's root an earlier tilo kept in `earlier`, kept in the identity store: see
/// [`convert_earlier`].
fn convert_earlier_root(earlier: &std::path::Path, place: &login::Place) -> Result<()> {
    let seed = earlier.join("privkey.bin");
    if !seed.exists() {
        return Ok(());
    }
    let root = tilo_device_identity::persist::read_signing_key(&seed)
        .map_err(|error| anyhow!("{} cannot be read: {error}", seed.display()))?;
    let user = root.verifying_key().to_bytes();
    let public = earlier.join("pubkey.bin");
    if public.exists() && tilo_device_identity::persist::read_public_key(&public).ok() != Some(user)
    {
        bail!(
            "{} is not the key of {}: nothing was changed.",
            public.display(),
            seed.display()
        );
    }
    let dir = dir()?;
    let storage = IdentityStorage::for_session();
    // Who the store acts as now, read from its public record: its device may hold no
    // certificate of it.
    let active = tilo_identity::store::resolve_active_identity(&dir)
        .map(|record| tilo_identity::store::load_metadata_from_path(&record))
        .transpose()
        .map_err(|error| anyhow!("{} cannot be read: {error}", dir.display()))?
        .map(|record| record.group_public_key);
    let record = match active {
        Some(kept) if kept == user => tilo_identity::store::resolve_active_identity(&dir)
            .context("the identity kept is not there")?,
        Some(kept) => {
            let login_is_theirs =
                earlier_login(place)?.is_some_and(|(_, cert, _)| cert.user_public_key == user);
            if !login_is_theirs {
                bail!(
                    "This computer account holds the Tilo identity {} and an earlier tilo kept \
                     another one ({}) in {}, with no login of it: nothing was changed.",
                    hex::encode(kept),
                    hex::encode(user),
                    earlier.display()
                );
            }
            let id = format!("person-{}", hex::encode(&user[..8]));
            tilo_identity::provisioning::import_single_device_root_as(&dir, &id, &root, "", storage)
                .with_context(|| format!("keep the identity in {}", dir.display()))?
        }
        None => tilo_identity::provisioning::import_single_device_root(&dir, &root, "", storage)
            .with_context(|| format!("keep the identity in {}", dir.display()))?,
    };
    tilo_identity::provisioning::ensure_device_certificate_for_record(
        &record,
        &paths(),
        &login::label("tilo", &login::hostname()),
    )
    .context("certify this computer's device key")?;
    let kept = held()?.context("the identity just kept is not there")?;
    if kept.public_key() != user {
        bail!("the identity kept is not the earlier one");
    }
    remove(&[&seed, &public])?;
    let _ = std::fs::remove_dir(earlier);
    Ok(())
}

/// The login an earlier tilo kept in `place`, kept as this computer's: see [`convert_earlier`].
fn convert_earlier_login(place: &login::Place) -> Result<()> {
    if place.logged_in() {
        return Ok(());
    }
    let Some((earlier, cert, server_cert)) = earlier_login(place)? else {
        return Ok(());
    };
    let person = held()?.context(
        "This computer's earlier login is of a person whose identity it does not hold: log in \
         with a ticket from `tilo device add` on one of their devices.",
    )?;
    if cert.user_public_key != person.public_key() {
        bail!(
            "This computer's earlier login is of another person than the one whose identity it \
             holds: log in with a ticket from `tilo device add` on one of their devices."
        );
    }
    login::store(
        place,
        &login::Device {
            site: earlier.site,
            host_origin: earlier.host_origin,
            workspace: earlier.workspace,
            workspace_name: earlier.workspace_name,
            agent: earlier.agent,
            cert: person.cert,
            server_cert,
            server_address: earlier.server_address,
        },
    )?;
    remove(&[&place.dir().join("device.json"), &place.dir().join("key")])
}

/// The person kept here, made first when there is none: whether it was made
/// now. `name` names a new one; one already kept stays as it is.
pub fn ensure(name: Option<&str>) -> Result<(Person, bool)> {
    convert_earlier()?;
    let made = !holds_root();
    if made {
        let dir = dir()?;
        // A person goes by the name they give, else by none until they give one: never their
        // computer's account name or a made-up one.
        tilo_identity::provisioning::create_person(
            &dir,
            &paths(),
            name.map(str::trim).unwrap_or_default(),
            &login::label("tilo", &login::hostname()),
            IdentityStorage::for_session(),
        )
        .with_context(|| format!("make the identity in {}", dir.display()))?;
    }
    let person = held()?.context("the identity just made is not there")?;
    Ok((person, made))
}

/// Become again the person whose 24-word recovery key `words` is: their root
/// kept here as `tilo login --new-identity` keeps a new one, and this computer
/// certified by it as a device of its own. A computer that holds them already
/// keeps them; one that holds someone else refuses.
///
/// # Errors
/// The words are not a recovery key, they are someone else's than the person
/// kept here, or the identity could not be written or certified.
pub fn restore(words: &str) -> Result<Person> {
    convert_earlier()?;
    let dir = dir()?;
    tilo_identity::provisioning::restore_from_recovery_words(
        &dir,
        &paths(),
        words,
        Some(&login::label("tilo", &login::hostname())),
        IdentityStorage::for_session(),
    )
    .map_err(
        |error| match tilo_identity::store::recovery_words_error(&error) {
            Some(tilo_identity::store::RecoveryWordsError::DifferentIdentity {
                expected, ..
            }) => {
                anyhow!(
                    "This computer holds another Tilo identity ({}), not the one these words are: \
                 log in as that person, or use a computer account of its own for this one.",
                    hex::encode(expected)
                )
            }
            Some(problem) => anyhow!("{problem}"),
            None => anyhow!(error).context(format!("restore the identity in {}", dir.display())),
        },
    )?;
    held()?.context("the identity just restored is not there")
}

/// Name the person kept here `name`, when they go by none yet: how a person who joins a
/// workspace from a computer that holds their identity already names themselves there.
///
/// # Errors
/// The name could not be written.
pub fn keep_name(person: &mut Person, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || !person.name.is_empty() {
        return Ok(());
    }
    tilo_identity::store::rename_active_identity(&dir()?, name)?;
    name.clone_into(&mut person.name);
    Ok(())
}

/// `tilo login --new-identity`: make the person, or say there is one, and
/// print their user public key — or, as one line of JSON, the key and the
/// name they go by, for a script that declares them somewhere.
pub fn new_identity(name: Option<&str>, json: bool) -> Result<std::process::ExitCode> {
    let (person, made) = ensure(name)?;
    let dir = dir()?;
    if json {
        println!(
            "{}",
            serde_json::json!({"user_public_key": person.user(), "name": person.name})
        );
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if made {
        eprintln!(
            "Made {} Tilo identity on this computer ({}). Once a host's operator declares \
             this key an owner, `tilo keys create` approves its key here.",
            person.whose(),
            dir.display()
        );
    } else {
        eprintln!(
            "This computer already holds {} Tilo identity ({}).",
            person.whose(),
            dir.display()
        );
    }
    println!("{}", person.user());
    Ok(std::process::ExitCode::SUCCESS)
}

/// What the person's Tilo is asked to sign at `<site>/sign-in?…`.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    pub audience: String,
    pub challenge: [u8; 32],
    pub return_to: String,
    pub pending: String,
    pub client: String,
    pub grant_key: [u8; 32],
    pub server_key: bool,
    /// What the key reaches: `all` of the person's workspaces, or one workspace's id.
    pub scope: String,
    /// Whether the key operates web pages in Browser without asking each time.
    pub pages: bool,
    pub expires_at_ns: Option<u64>,
}

/// Read the sign-in `location` an approval link leads to.
pub fn asked(location: &str) -> Result<Asked> {
    let url = tilo_net::Url::parse(location)
        .with_context(|| format!("{location} is not a sign-in address"))?;
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    let field = |name: &str| {
        query
            .get(name)
            .cloned()
            .with_context(|| format!("the sign-in asks for no {name}"))
    };
    let key = |name: &str| -> Result<[u8; 32]> {
        hex::decode(field(name)?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .with_context(|| format!("the sign-in's {name} is not 32 bytes of hex"))
    };
    let expires_at_ns = match field("grant_expires")?.as_str() {
        "never" => None,
        ms => Some(
            ms.parse::<u64>()
                .context("the sign-in's grant_expires is no time")?
                .saturating_mul(1_000_000),
        ),
    };
    Ok(Asked {
        audience: field("audience")?,
        challenge: key("challenge")?,
        return_to: field("return")?,
        pending: field("pending")?,
        client: query.get("client").cloned().unwrap_or_default(),
        grant_key: key("grant_key")?,
        server_key: query.get("grant_kind").map(String::as_str) == Some("server_key"),
        scope: query
            .get("grant_scope")
            .cloned()
            .unwrap_or_else(|| "all".to_owned()),
        pages: query.get("grant_pages").map(String::as_str) == Some("1"),
        expires_at_ns,
    })
}

/// The proof and the grant certificate `person` answers `asked` with, as
/// the page posts them back: `pending`, `proof` and `grant`. A key the person
/// makes is a server key, theirs, for all their workspaces or one; an agent's
/// grant for one workspace is certified as their Tilo certifies it on its
/// sign-in card: narrower than what this computer approves already.
///
/// # Errors
/// A scope that is neither `all` nor a workspace id, a key for one workspace
/// that names no expiry, or a certificate that does not sign.
pub fn answer(person: &Person, asked: &Asked) -> Result<Vec<(&'static str, String)>> {
    let scope = tilo_user_identity::AgentScope::parse(&asked.scope).with_context(|| {
        format!(
            "the key asks to reach {}, which is no workspace",
            asked.scope
        )
    })?;
    let capabilities = match scope {
        scope if asked.server_key => DeviceCapabilities::own_key_grant(scope),
        scope => {
            if asked.expires_at_ns.is_none() {
                bail!(
                    "a key for one workspace lasts until a time it names, and this one names none"
                );
            }
            DeviceCapabilities::agent_grant(scope)
        }
    }
    .with_browser_pages(asked.pages);
    let mut grant = DeviceCert {
        user_public_key: person.public_key(),
        device_public_key: asked.grant_key,
        device_label: tilo_server_client::sign_in::grant_label(&asked.client, &asked.audience),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: asked.expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, &person.root)?;
    let proof = SignInProof::sign(
        person.cert.clone(),
        &person.device,
        &asked.audience,
        asked.challenge,
    )?;
    Ok(vec![
        ("pending", asked.pending.clone()),
        ("proof", tilo_server_client::sign_in::encode(&proof)?),
        ("grant", tilo_server_client::sign_in::encode(&grant)?),
    ])
}

/// Approve the key `approve_url` asks for, as `person`'s Tilo on the web
/// does once they agree: the link leads to the site's sign-in, which this
/// answers with the person's proof and the key's certificate.
pub async fn approve(person: &Person, approve_url: &str) -> Result<()> {
    let http = tilo_net::Client::builder()
        .redirects(tilo_net::Redirects::None)
        .build()?;
    let led = http
        .get(approve_url)
        .send()
        .await
        .with_context(|| format!("reaching {approve_url}"))?;
    let location = led
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .with_context(|| format!("{approve_url} leads to no sign-in (HTTP {})", led.status()))?
        .to_owned();
    let asked = asked(&location)?;
    let form = answer(person, &asked)?;
    let answered = http
        .post(&asked.return_to)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("reaching {}", asked.return_to))?;
    let status = answered.status();
    if !(status.is_success() || status.is_redirection()) {
        bail!("{} refused the approval (HTTP {status})", asked.return_to);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn decoded<T: serde::de::DeserializeOwned>(text: &str) -> T {
        tilo_user_identity::canonical::from_canonical_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(text)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_new_identity_is_one_the_session_reads_and_admits() {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path())
            .set("XDG_CONFIG_HOME", home.path().join("config"))
            .set("TILO_IDENTITY_ROOT", home.path().join("system"))
            .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        let (person, made) = ensure(Some("Ada")).unwrap();
        assert!(made, "a computer with no identity makes one");
        assert!(holds_root());
        let kept = held().unwrap().expect("the person just made");
        assert_eq!(kept.public_key(), person.public_key());
        assert_eq!(kept.name, "Ada");
        // The registry the session reads names the person, and this device is
        // enrolled in their fleet: the session's broker admits their own
        // actions under the certificate.
        let profile = tilo_identity::store::identity_for_workspace("any")
            .unwrap()
            .expect("the registry names the person");
        assert_eq!(profile.user_public_key, person.public_key());
        let fleet = tilo_identity::device_fleet::DeviceFleetStore::from_env()
            .unwrap()
            .load(&profile.id)
            .unwrap();
        assert!(
            fleet
                .certificates
                .iter()
                .any(|known| known.device_public_key == kept.cert.device_public_key),
            "this device is in the identity's fleet"
        );
        let (again, made) = ensure(Some("Someone else")).unwrap();
        assert!(!made, "the person is kept");
        assert_eq!(again.public_key(), person.public_key());
        assert_eq!(again.name, "Ada");
    }

    #[test]
    fn a_person_restored_from_their_recovery_words_is_who_they_were() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let at = |env: &mut tilo_test_env::ScopedEnv, home: &std::path::Path| {
            env.set("HOME", home)
                .set("XDG_CONFIG_HOME", home.join("config"))
                .set("TILO_IDENTITY_ROOT", home.join("system"))
                .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        };
        let first = tempfile::tempdir().unwrap();
        at(&mut env, first.path());
        let (ada, _) = ensure(Some("Ada")).unwrap();
        let active = tilo_identity::store::resolve_active_identity(&dir().unwrap()).unwrap();
        let words = tilo_identity::store::recovery_words(&active).unwrap();
        let other = tempfile::tempdir().unwrap();
        at(&mut env, other.path());
        ensure(Some("Bea")).unwrap();
        let active = tilo_identity::store::resolve_active_identity(&dir().unwrap()).unwrap();
        let bea_words = tilo_identity::store::recovery_words(&active).unwrap();

        // A computer account with nothing of Tilo on it: the words make it Ada's again, with a
        // device of its own that her root certified.
        let fresh = tempfile::tempdir().unwrap();
        at(&mut env, fresh.path());
        assert!(!holds_root());
        let restored = restore(&format!("  {}\n", words.as_str())).unwrap();
        assert_eq!(restored.public_key(), ada.public_key(), "the same person");
        assert_eq!(restored.cert.user_public_key, ada.public_key());
        tilo_user_identity::verify_device_cert(&restored.cert).unwrap();
        assert_ne!(
            restored.cert.device_public_key, ada.cert.device_public_key,
            "this computer is a device of its own"
        );
        let profile = tilo_identity::store::identity_for_workspace("any")
            .unwrap()
            .expect("the session's registry names her");
        assert_eq!(profile.user_public_key, ada.public_key());

        // Her words again keep her, on the same device; someone else's are refused.
        let again = restore(&words).unwrap();
        assert_eq!(again.public_key(), ada.public_key());
        assert_eq!(
            again.cert.device_public_key,
            restored.cert.device_public_key
        );
        let refused = restore(&bea_words).err().expect("another person's words");
        assert!(
            refused.to_string().contains("another Tilo identity"),
            "{refused:#}"
        );
        assert_eq!(held().unwrap().unwrap().public_key(), ada.public_key());
        let wrong = restore("abandon ability able").err().expect("three words");
        assert!(wrong.to_string().contains("24 words"), "{wrong:#}");
    }

    /// A computer account kept in `home`.
    fn at(env: &mut tilo_test_env::ScopedEnv, home: &std::path::Path) {
        env.set("HOME", home)
            .set("XDG_CONFIG_HOME", home.join("config"))
            .set("XDG_STATE_HOME", home.join("state"))
            .set("TILO_IDENTITY_ROOT", home.join("system"))
            .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    }

    /// A computer account where an earlier tilo kept `root`'s person: their root and public key
    /// as files in its state folder, and its login beside them, with a key of its own.
    fn earlier_files(home: &std::path::Path, root: &SigningKey) {
        use tilo_device_identity::persist::{write_public_key, write_signing_key};
        let user = home.join("state/tilo/identity/user-identity");
        std::fs::create_dir_all(&user).unwrap();
        write_signing_key(&user.join("privkey.bin"), root).unwrap();
        write_public_key(&user.join("pubkey.bin"), &root.verifying_key().to_bytes()).unwrap();
        let cert_of = |user_public_key: [u8; 32], device: [u8; 32], label: &str| DeviceCert {
            user_public_key,
            device_public_key: device,
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let hexed = |cert: &DeviceCert| hex::encode(postcard::to_allocvec(cert).unwrap());
        let device = home.join("state/tilo/device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("key"), [9; 32]).unwrap();
        std::fs::write(
            device.join("device.json"),
            serde_json::json!({
                "site": "https://site.test",
                "host_origin": "https://host.test",
                "workspace": "ws-ada",
                "workspace_name": "Ada's",
                "agent": "tilo",
                "cert": hexed(&cert_of(root.verifying_key().to_bytes(), [9; 32], "tilo")),
                "server_cert": hexed(&cert_of([4; 32], [5; 32], "Host")),
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_person_and_login_an_earlier_tilo_kept_are_converted_once_under_the_same_key() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let ada = SigningKey::from_bytes(&[7; 32]);
        let home = tempfile::tempdir().unwrap();
        at(&mut env, home.path());
        earlier_files(home.path(), &ada);
        assert!(!holds_root(), "the store holds no one yet");

        // Asking for a new identity converts hers instead of making a second person.
        let (person, made) = ensure(Some("Someone")).unwrap();
        assert!(!made, "no second person is made beside her");
        assert_eq!(
            person.public_key(),
            ada.verifying_key().to_bytes(),
            "the same key"
        );
        tilo_user_identity::verify_device_cert(&person.cert).unwrap();
        let place = login::Place::here(paths()).unwrap();
        let (key, device) = login::load(&place)
            .unwrap()
            .expect("her login is converted");
        assert_eq!(
            key.verifying_key().to_bytes(),
            person.cert.device_public_key
        );
        assert_eq!(device.workspace, "ws-ada");
        assert_eq!(device.server_cert.device_public_key, [5; 32]);
        for gone in [
            "state/tilo/identity/user-identity/privkey.bin",
            "state/tilo/device/device.json",
            "state/tilo/device/key",
        ] {
            assert!(!home.path().join(gone).exists(), "{gone} is removed");
        }
        // Once converted, nothing is read again and she is kept.
        convert_earlier().unwrap();
        assert_eq!(held().unwrap().unwrap().public_key(), person.public_key());
    }

    #[test]
    fn an_earlier_root_with_its_login_is_acted_as_beside_another_kept_person() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let ada = SigningKey::from_bytes(&[7; 32]);
        // An account that keeps Bea already, as a desktop kept its own person before, and holds
        // no certificate of hers for this device. Ada's root with no login of hers is not taken.
        let other = tempfile::tempdir().unwrap();
        at(&mut env, other.path());
        let (bea, _) = ensure(Some("Bea")).unwrap();
        std::fs::remove_dir_all(other.path().join("system/device")).unwrap();
        earlier_files(other.path(), &ada);
        let device_json = other.path().join("state/tilo/device/device.json");
        let login_bytes = std::fs::read(&device_json).unwrap();
        std::fs::remove_file(&device_json).unwrap();
        let refused = convert_earlier().expect_err("another person's root, with no login");
        assert!(
            refused.to_string().contains("nothing was changed"),
            "{refused:#}"
        );
        let active = |dir: &std::path::Path| {
            let record = tilo_identity::store::resolve_active_identity(dir).unwrap();
            tilo_identity::store::load_metadata_from_path(&record)
                .unwrap()
                .group_public_key
        };
        let store = dir().unwrap();
        assert_eq!(active(&store), bea.public_key());
        let seed = other
            .path()
            .join("state/tilo/identity/user-identity/privkey.bin");
        assert!(seed.exists(), "her files are left as they were");

        // With her login beside it, Ada's root is the one this account acts as: the workspaces
        // it works in are hers. Bea's record stays in the store as it was.
        std::fs::write(&device_json, login_bytes).unwrap();
        let bea_record = std::fs::read(store.join("primary.json")).unwrap();
        convert_earlier().unwrap();
        let person = held().unwrap().expect("Ada is kept");
        assert_eq!(person.public_key(), ada.verifying_key().to_bytes());
        assert_eq!(active(&store), ada.verifying_key().to_bytes());
        tilo_user_identity::verify_device_cert(&person.cert).unwrap();
        assert_eq!(
            std::fs::read(store.join("primary.json")).unwrap(),
            bea_record,
            "Bea's record is untouched"
        );
        let place = login::Place::here(paths()).unwrap();
        let (_, device) = login::load(&place)
            .unwrap()
            .expect("her login is converted");
        assert_eq!(device.workspace, "ws-ada");
        assert_eq!(device.cert, person.cert);
        assert!(!seed.exists() && !device_json.exists());
    }

    #[test]
    fn the_sign_in_is_answered_as_the_page_answers_it() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device_key = SigningKey::from_bytes(&[2; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device_key.verifying_key().to_bytes(),
            device_label: "tilo on box".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let store = tempfile::tempdir().unwrap();
        let kept = tilo_identity::provisioning::import_single_device_root(
            store.path(),
            &root,
            "Ada",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let person = Person {
            root: tilo_identity::store::load_from_path(kept).unwrap(),
            device: device_key,
            cert,
            name: "Ada".into(),
        };
        let location = format!(
            "http://tilo.localhost:24120/sign-in?audience=http%3A%2F%2Flocalhost%3A24110\
             &challenge={}&return=http%3A%2F%2Flocalhost%3A24110%2Fsign-in&pending=p1\
             &client=Operator&grant_key={}&grant_scope=all&grant_expires=never\
             &grant_kind=server_key",
            "07".repeat(32),
            "09".repeat(32),
        );
        let read = asked(&location).unwrap();
        assert_eq!(read.audience, "http://localhost:24110");
        assert_eq!(read.return_to, "http://localhost:24110/sign-in");
        assert!(read.server_key);
        assert_eq!(read.expires_at_ns, None);
        let form = answer(&person, &read).unwrap();
        assert_eq!(form[0], ("pending", "p1".to_owned()));
        let proof: SignInProof = decoded(&form[1].1);
        assert_eq!(
            proof.verify(
                "http://localhost:24110",
                &[7; 32],
                &tilo_user_identity::RevocationList::new(),
                tilo_clock::unix_now_ns()
            ),
            Ok(person.public_key())
        );
        let grant: DeviceCert = decoded(&form[2].1);
        tilo_user_identity::verify_device_cert(&grant).unwrap();
        assert_eq!(grant.device_public_key, [9; 32]);
        assert_eq!(grant.capabilities, DeviceCapabilities::server_key_grant());
        // A key for one workspace is narrower than a server key: this computer approves it too,
        // as the person's Tilo certifies it, an agent's grant for that workspace until the time
        // it names.
        let workspace = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";
        let until_ms = tilo_clock::unix_now_ns() / 1_000_000 + 3_600_000;
        let one = asked(
            &location
                .replace("&grant_kind=server_key", "&grant_pages=1")
                .replace("grant_scope=all", &format!("grant_scope={workspace}"))
                .replace("grant_expires=never", &format!("grant_expires={until_ms}")),
        )
        .unwrap();
        assert!(!one.server_key);
        let form = answer(&person, &one).unwrap();
        let grant: DeviceCert = decoded(&form[2].1);
        tilo_user_identity::verify_device_cert(&grant).unwrap();
        assert_eq!(
            grant.capabilities,
            DeviceCapabilities::agent_grant(
                tilo_user_identity::AgentScope::parse(workspace).unwrap()
            )
            .with_browser_pages(true)
        );
        assert_eq!(grant.expires_at_ns, Some(until_ms * 1_000_000));
        // A key the person makes for one workspace is theirs, as one for all
        // of them is; an agent's for one lasts until a time.
        let narrowed =
            asked(&location.replace("grant_scope=all", &format!("grant_scope={workspace}")))
                .unwrap();
        let form = answer(&person, &narrowed).unwrap();
        let own: DeviceCert = decoded(&form[2].1);
        tilo_user_identity::verify_device_cert(&own).unwrap();
        assert!(own.capabilities.server_key);
        let endless = asked(
            &location
                .replace("&grant_kind=server_key", "")
                .replace("grant_scope=all", &format!("grant_scope={workspace}")),
        )
        .unwrap();
        assert!(answer(&person, &endless).is_err());
    }
}
