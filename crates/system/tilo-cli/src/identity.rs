//! A person whose root this computer holds (`tilo login --new-identity`),
//! and what that root lets this computer do without a browser: approve the
//! key `tilo keys create` asks for, as the person's Tilo on the web does.
//!
//! The identity is kept in Tilo's own layout ([`IdentityPaths`]) under
//! `$XDG_STATE_HOME/tilo/identity`: the user root, this computer's device
//! key, and the member certificate the root signed for it. `tilo-enroll
//! certify --identity-root` reads the same files. A host lets the person in
//! once its operator declares their user public key an owner (the local
//! stack's `stack.sh owner`).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde_json::Value;
use tilo_relay_client::https::{self, Origin};
use tilo_session_client::device::{self, Entering, Held};
use tilo_user_identity::persist::{
    read_device_cert, read_public_key, read_signing_key, write_device_cert, write_public_key,
    write_signing_key,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths, SignInProof};

use crate::login;

/// Names the directory the identity is kept in, instead of
/// `$XDG_STATE_HOME/tilo/identity`.
pub const DIR_ENV: &str = "TILO_IDENTITY_DIR";
/// How long approving waits for the site's door to let the person in: an
/// operator's roles file reaches the host within seconds.
const DOOR_WAIT: Duration = Duration::from_secs(60);

/// The person this computer holds the root of.
pub struct Person {
    pub root: SigningKey,
    pub device: SigningKey,
    /// The member certificate the root signed for [`Self::device`].
    pub cert: DeviceCert,
    /// How the person is named.
    pub name: String,
}

impl Person {
    /// Their user public key, hex.
    pub fn user(&self) -> String {
        hex::encode(self.root.verifying_key().to_bytes())
    }
}

/// Where the identity is kept: [`DIR_ENV`], else
/// `$XDG_STATE_HOME/tilo/identity`.
pub fn dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DIR_ENV).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(tilo_paths::state_dir()
        .context("this account has no state directory (no HOME)")?
        .join("tilo/identity"))
}

/// The person kept here, if this computer holds one.
pub fn held() -> Result<Option<Person>> {
    let dir = dir()?;
    let paths = IdentityPaths::under(&dir);
    if !paths.user_privkey.exists() {
        return Ok(None);
    }
    let root = read_signing_key(&paths.user_privkey)?;
    if read_public_key(&paths.user_pubkey)? != root.verifying_key().to_bytes() {
        bail!("{} is damaged: its keys disagree", dir.display());
    }
    Ok(Some(Person {
        root,
        device: read_signing_key(&paths.device_privkey)?,
        cert: read_device_cert(&paths.device_cert)?,
        name: std::fs::read_to_string(dir.join("name"))
            .map(|name| name.trim().to_owned())
            .unwrap_or_default(),
    }))
}

/// The person kept here, made first when there is none: whether it was made
/// now. `name` names a new one; one already kept stays as it is.
pub fn ensure(name: Option<&str>) -> Result<(Person, bool)> {
    if let Some(person) = held()? {
        return Ok((person, false));
    }
    let dir = dir()?;
    let paths = IdentityPaths::under(&dir);
    paths.ensure_directories()?;
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| std::env::var("USER").ok().filter(|name| !name.is_empty()))
        .unwrap_or_else(|| "Tilo user".to_owned());
    let root = SigningKey::from_bytes(&rand::random());
    let device = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: login::label("tilo", &login::hostname()),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root)?;
    std::fs::write(dir.join("name"), format!("{name}\n"))?;
    write_signing_key(&paths.device_privkey, &device)?;
    write_device_cert(&paths.device_cert, &cert)?;
    write_public_key(&paths.user_pubkey, &root.verifying_key().to_bytes())?;
    // Last: the identity is there once its root is.
    write_signing_key(&paths.user_privkey, &root)?;
    Ok((
        Person {
            root,
            device,
            cert,
            name,
        },
        true,
    ))
}

/// `tilo login --new-identity`: make the person, or say there is one, and
/// print their user public key.
pub fn new_identity(name: Option<&str>) -> Result<std::process::ExitCode> {
    let (person, made) = ensure(name)?;
    let dir = dir()?;
    if made {
        eprintln!(
            "Made {}'s Tilo identity on this computer ({}). Once a host's operator declares \
             this key an owner, `tilo keys create` approves its key here.",
            person.name,
            dir.display()
        );
    } else {
        eprintln!(
            "This computer already holds {}'s Tilo identity ({}).",
            person.name,
            dir.display()
        );
    }
    println!("{}", person.user());
    Ok(std::process::ExitCode::SUCCESS)
}

/// What the person's Tilo is asked to sign at `<site>/sign-in?…`.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    pub site: String,
    pub audience: String,
    pub challenge: [u8; 32],
    pub return_to: String,
    pub pending: String,
    pub client: String,
    pub grant_key: [u8; 32],
    pub server_key: bool,
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
        site: url.origin().ascii_serialization(),
        audience: field("audience")?,
        challenge: key("challenge")?,
        return_to: field("return")?,
        pending: field("pending")?,
        client: query.get("client").cloned().unwrap_or_default(),
        grant_key: key("grant_key")?,
        server_key: query.get("grant_kind").map(String::as_str) == Some("server_key"),
        expires_at_ns,
    })
}

/// The proof and the grant certificate `person` answers `asked` with, as
/// the page posts them back: `pending`, `proof` and `grant`.
///
/// # Errors
/// A key for one workspace, which the person approves in their Tilo, or a
/// certificate that does not sign.
pub fn answer(person: &Person, asked: &Asked) -> Result<Vec<(&'static str, String)>> {
    if !asked.server_key {
        bail!("this computer approves keys for all your workspaces; approve this one in your Tilo");
    }
    let mut grant = DeviceCert {
        user_public_key: person.root.verifying_key().to_bytes(),
        device_public_key: asked.grant_key,
        device_label: tilo_relay_client::sign_in::grant_label(&asked.client, &asked.audience),
        capabilities: DeviceCapabilities::server_key_grant(),
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
        ("proof", tilo_relay_client::sign_in::encode(&proof)?),
        ("grant", tilo_relay_client::sign_in::encode(&grant)?),
    ])
}

/// The person in the site's home workspace, through its door, as their
/// page comes in: the host knows them from then on. Waits for an operator's
/// roles file to reach the host.
async fn through_door(person: &Person, site: &str) -> Result<()> {
    let http = login::http();
    let (status, body) = Origin::new(&http, site)
        .get("/entry.json")
        .await
        .map_err(|error| anyhow!("{site} was not reached: {error}"))?;
    if status != 200 {
        bail!("{site} names no Tilo host (HTTP {status})");
    }
    let entry: Value = serde_json::from_slice(&body)?;
    let host = entry
        .pointer("/home/host/origin")
        .and_then(Value::as_str)
        .with_context(|| format!("{site} names no Tilo host"))?;
    let Some(workspace) = tilo_relay_client::login::site_door(&entry, host)
        .and_then(|(_, path)| tilo_relay_client::login::Door { path }.workspace())
    else {
        return Ok(());
    };
    let info = https::host_info(&Origin::new(&http, host)).await?;
    let address = info
        .address
        .as_ref()
        .context("the host takes no device connections")?;
    let held = Held {
        cert: person.cert.clone(),
        host_cert: Some(info.host_cert.clone()),
    };
    let began = std::time::Instant::now();
    loop {
        let (cert, entry) = device::door_entry(
            &info.host_cert,
            &workspace,
            &person.device,
            Entering {
                held: Some(&held),
                root: Some(&person.root),
                label: &person.cert.device_label,
                name: &person.name,
                token: None,
            },
        )
        .await
        .map_err(|refused| anyhow!(refused.message))?;
        let session = tilo_session_client::start_session(
            tilo_session_client::PeerConfig {
                host: tilo_relay_client::Host::published(address),
                cert,
                host_cert: info.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: Some(entry),
            },
            std::sync::Arc::new(person.device.clone()),
            std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
        );
        let entered = device::entered(&session).await;
        session.close();
        match entered {
            Ok(_) => return Ok(()),
            Err(refused) if refused.code == "invitation-only" && began.elapsed() < DOOR_WAIT => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(refused) if refused.code == "invitation-only" => bail!(
                "{site}'s door does not let {} in: its operator declares {} an owner first \
                 (on the local stack: stack.sh owner)",
                person.name,
                person.user()
            ),
            Err(refused) => bail!("{site}'s door: {}", refused.message),
        }
    }
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
    through_door(person, &asked.site).await?;
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
        let person = Person {
            root,
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
        assert_eq!(read.site, "http://tilo.localhost:24120");
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
            Ok(person.root.verifying_key().to_bytes())
        );
        let grant: DeviceCert = decoded(&form[2].1);
        tilo_user_identity::verify_device_cert(&grant).unwrap();
        assert_eq!(grant.device_public_key, [9; 32]);
        assert_eq!(grant.capabilities, DeviceCapabilities::server_key_grant());
        // A key for one workspace is the person's to approve in their Tilo.
        let one = asked(&location.replace("&grant_kind=server_key", "")).unwrap();
        assert!(answer(&person, &one).is_err());
    }
}
