//! An install plan for a machine at another place, made the one way the
//! command line and the installer's window both make it.
//!
//! [`Choices`] holds what the person decided now; every choice they did not
//! decide stays [`None`] and is open, so the machine asks one of their devices
//! for it against its own hardware. [`Choices::plan`] writes the document and
//! [`open`](Choices::open) is the list of what it still asks, computed here so
//! the plan's fields and its `open` list cannot disagree.
//!
//! The plan itself is the machine's entry in the person's Servers workspace
//! (`tilo_workspace_service::workspaces::servers`): this crate only writes the
//! document, and where it goes is the session's to say. [`mint`] makes the
//! single-use enrollment token the medium carries; the plan names the token by
//! its nonce and expiry only, so the document travels through the person's
//! devices, a log or an action's answer without exposing the token. The token
//! itself goes to a file only its account can read ([`store_token`]), the one
//! file this crate keeps.

use std::path::{Path, PathBuf};

use install_profile_model::plan::{self, DiskRule, Holding, Network, Plan, Token};
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::{load_device_key, read_device_cert, read_public_key};
use tilo_user_identity::{
    DeviceCapabilities, Dial, encode_enrollment_token, enrollment_endpoint, mint_enrollment_token,
};

pub use install_profile_model::plan::{Commands, commands, installer_image};

/// How long a machine's token stays valid: a medium may sit on a shelf a day.
pub const TOKEN_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Where the person's devices serve the installer, unless something names
/// another site.
pub const INSTALLER_URL: &str = "https://tilo.sh";

/// The choices a plan makes now, with `None` for one it leaves open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    /// The OS it installs, one of [`plan::DESTINATIONS`].
    pub destination: String,
    /// The machine's name; open when absent.
    pub name: Option<String>,
    /// How its network interfaces are set up; open when absent.
    pub network: Option<Network>,
    /// The rule its disk is picked by; open when absent.
    pub disk: Option<DiskRule>,
    /// How it unlocks after a boot; open when absent.
    pub unlock: Option<String>,
    /// The updates it follows; open when absent.
    pub updates: Option<String>,
    /// A server's workspaces: `Some(vec![])` answers "holds none", and `None`
    /// leaves the choice open.
    pub hosting: Option<Vec<Holding>>,
}

impl Choices {
    /// The choices this plan will leave for the machine, in
    /// [`plan::OPEN_CHOICES`] order.
    #[must_use]
    pub fn open(&self) -> Vec<&'static str> {
        let mut open = Vec::new();
        if self
            .name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
        {
            open.push("name");
        }
        if self.network.is_none() {
            open.push("network");
        }
        if self.disk.is_none() {
            open.push("disk");
        }
        if self.unlock.is_none() {
            open.push("unlock");
        }
        if self.updates.is_none() {
            open.push("updates");
        }
        if self.destination == "tilo-server" && self.hosting.is_none() {
            open.push("hosting");
        }
        open
    }

    /// The plan these choices make, under `id`, with `token`. The open list is
    /// this structure's own, so a document and the machine reading it cannot
    /// disagree about what is still asked.
    ///
    /// # Errors
    ///
    /// The choices do not make a plan the machine could install: the OS or a
    /// value is outside what [`plan`] names, or a static network names no
    /// address.
    pub fn plan(&self, id: &str, token: Token) -> Result<Plan, String> {
        let plan = Plan {
            schema: plan::SCHEMA,
            plan: id.to_owned(),
            revision: 1,
            destination: self.destination.clone(),
            name: self.name.clone().unwrap_or_default().trim().to_owned(),
            network: self.network.clone().unwrap_or_default(),
            disk: self.disk.clone().unwrap_or_default(),
            unlock: self.unlock.clone().unwrap_or_default(),
            updates: self.updates.clone().unwrap_or_default(),
            hosting: self.hosting.clone().unwrap_or_default(),
            open: self.open().iter().map(|name| (*name).to_owned()).collect(),
            token,
        };
        // Whatever is built here has to be a plan the machine can read, so one
        // rule checks it, not two.
        let checked: Plan =
            serde_json::from_value(serde_json::to_value(&plan).map_err(|error| error.to_string())?)
                .map_err(|_| "that plan cannot be used as written".to_owned())?;
        Ok(checked)
    }
}

/// A fresh plan id: 16 random bytes in lowercase hex.
#[must_use]
pub fn new_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// The single-use token a plan goes with, minted by this device: the encoded
/// token for the file the medium carries, and the nonce and expiry the plan
/// keeps.
///
/// # Errors
///
/// This computer holds no identity or device key, has no certificate that may
/// admit a device, or cannot say what time it is.
pub fn mint(system: &IdentityPaths, home_peers: &Path) -> Result<(String, Token), String> {
    let user = read_public_key(&system.user_pubkey)
        .map_err(|_| "this computer holds no identity; start `tilo login` first".to_owned())?;
    let device = load_device_key(system)
        .map_err(|_| "this computer holds no device key; start `tilo login` first".to_owned())?;
    let cert = read_device_cert(&system.device_cert_for_user(&user))
        .or_else(|_| read_device_cert(&system.device_cert))
        .map_err(|_| "this computer holds no certificate for its device".to_owned())?;
    if !cert.capabilities.can_admit_devices {
        return Err(
            "this device may not admit another device; plan it from one of your own devices"
                .to_owned(),
        );
    }
    let now_ns = tilo_clock::try_unix_now_ns()
        .ok_or_else(|| "this computer cannot say what time it is".to_owned())?;
    let expires_at_ns = now_ns
        .checked_add(TOKEN_LIFETIME_NS)
        .ok_or_else(|| "the token's expiry is out of range".to_owned())?;
    let nonce = rand::random::<[u8; 16]>();
    let token = mint_enrollment_token(
        user,
        DeviceCapabilities::member(),
        now_ns,
        expires_at_ns,
        nonce,
        dial_peers(system, home_peers, user),
        &device,
    )
    .map_err(|error| format!("cannot mint the machine's token: {error}"))?;
    let encoded = encode_enrollment_token(&token)
        .map_err(|error| format!("cannot encode the machine's token: {error}"))?;
    Ok((
        encoded,
        Token {
            nonce: hex::encode(nonce),
            expires_at_ns,
        },
    ))
}

/// The person's devices that answer enrollments, as a token names them: each
/// device's enrollment endpoint ([`enrollment_endpoint`]), which its session
/// serves while it is up, and the relay that reaches it. A session's own
/// endpoint answers devices, not enrollments, so a token never names that.
///
/// This device is named first: the session on the computer that made the plan
/// answers the machine even when nothing else of the person's is up. Then each
/// of their devices that may admit one, as the home-peer registry knows them.
#[must_use]
pub fn dial_peers(system: &IdentityPaths, home_peers: &Path, user: [u8; 32]) -> Vec<Dial> {
    let mut dials = Vec::new();
    if let Ok(device) = load_device_key(system) {
        dials.push(Dial {
            endpoint: enrollment_endpoint(&device.verifying_key().to_bytes()),
            relay_url: own_relay(),
        });
    }
    let Ok(peers) = tilo_server_protocol::read_home_peers(home_peers) else {
        return dials;
    };
    for peer in peers {
        if peer.user_public_key != hex::encode(user) {
            continue;
        }
        let Some(cert) = peer.device_cert else {
            continue;
        };
        if !cert.capabilities.can_admit_devices {
            continue;
        }
        let Some(endpoint) = peer.server_endpoint else {
            continue;
        };
        let relay_url = endpoint
            .relay_urls()
            .next()
            .map(ToString::to_string)
            .unwrap_or_default();
        let dial = Dial {
            endpoint: enrollment_endpoint(&cert.device_public_key),
            relay_url,
        };
        if !dials.contains(&dial) {
            dials.push(dial);
        }
    }
    dials
}

/// The relay this device's endpoints are reached through, empty when it is
/// reached on its own network only.
fn own_relay() -> String {
    tilo_endpoint_reachability::relays()
        .relays
        .urls::<Vec<_>>()
        .first()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// Write the encoded `token` for the plan `plan_id` where this computer keeps
/// it, readable only by this account; `token_file` says where when the person
/// wants it somewhere else, such as the medium itself.
///
/// The plan itself is not kept here: it is the machine's entry in the
/// person's Servers workspace, and every one of their devices reads it there.
///
/// # Errors
///
/// The token's directory cannot be made or the file cannot be written.
pub fn store_token(
    identity_dir: &Path,
    plan_id: &str,
    token: &str,
    token_file: Option<&Path>,
) -> Result<PathBuf, String> {
    let plans = plans_dir(identity_dir);
    std::fs::create_dir_all(&plans)
        .map_err(|error| format!("cannot make {}: {error}", plans.display()))?;
    let token_file =
        token_file.map_or_else(|| plans.join(format!("{plan_id}.token")), Path::to_path_buf);
    tilo_files::native::atomic::write_private(&token_file, token.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", token_file.display()))?;
    Ok(token_file)
}

/// The directory this computer's plan tokens live in.
#[must_use]
pub fn plans_dir(identity_dir: &Path) -> PathBuf {
    identity_dir.join("install-plans")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Token {
        Token {
            nonce: "00112233445566778899aabbccddeeff".to_owned(),
            expires_at_ns: 1_700_000_000_000_000_000,
        }
    }

    fn server() -> Choices {
        Choices {
            destination: "tilo-server".to_owned(),
            name: Some("cupboard".to_owned()),
            network: Some(Network {
                addressing: "dhcp".to_owned(),
                ..Network::default()
            }),
            disk: Some(DiskRule {
                kind: "largest".to_owned(),
                identity: String::new(),
            }),
            unlock: Some("tpm".to_owned()),
            updates: Some("stable".to_owned()),
            hosting: Some(Vec::new()),
        }
    }

    #[test]
    fn every_choice_made_is_a_complete_plan() {
        let plan = server().plan(&"a".repeat(32), token()).unwrap();
        assert!(plan.complete());
        assert_eq!(plan.destination, "tilo-server");
        assert!(plan.open.is_empty());
    }

    #[test]
    fn each_choice_left_out_is_named_open_by_the_plan() {
        let choices = Choices {
            destination: "tilo-server".to_owned(),
            ..Choices::default()
        };
        let plan = choices.plan(&"a".repeat(32), token()).unwrap();
        assert_eq!(
            plan.open_choices(),
            vec!["name", "network", "disk", "unlock", "updates", "hosting"]
        );
        assert!(!plan.complete());
    }

    #[test]
    fn a_server_that_holds_nothing_is_answered_not_open() {
        let plan = server().plan(&"a".repeat(32), token()).unwrap();
        assert!(plan.hosting.is_empty());
        assert!(!plan.open_choices().contains(&"hosting"));

        let unheld = Choices {
            hosting: None,
            ..server()
        };
        assert!(unheld.open().contains(&"hosting"));
        assert!(!unheld.plan(&"a".repeat(32), token()).unwrap().complete());
    }

    #[test]
    fn an_os_plan_never_asks_for_workspaces() {
        let choices = Choices {
            destination: "tilo-os".to_owned(),
            hosting: None,
            ..server()
        };
        assert!(!choices.open().contains(&"hosting"));
        assert!(choices.plan(&"a".repeat(32), token()).is_ok());
    }

    #[test]
    fn choices_a_machine_could_not_use_make_no_plan() {
        let static_without_address = Choices {
            network: Some(Network {
                addressing: "static".to_owned(),
                ..Network::default()
            }),
            ..server()
        };
        assert!(
            static_without_address
                .plan(&"a".repeat(32), token())
                .is_err()
        );

        let unknown_unlock = Choices {
            unlock: Some("password".to_owned()),
            ..server()
        };
        assert!(unknown_unlock.plan(&"a".repeat(32), token()).is_err());

        assert!(server().plan("not a plan id", token()).is_err());
    }

    #[test]
    fn the_token_is_the_only_file_kept_and_only_its_account_reads_it() {
        let directory = tempfile::tempdir().unwrap();
        let id = "a".repeat(32);
        let token_file = store_token(directory.path(), &id, "token-bytes", None).unwrap();
        assert_eq!(
            token_file,
            plans_dir(directory.path()).join(format!("{id}.token"))
        );
        assert_eq!(std::fs::read_to_string(&token_file).unwrap(), "token-bytes");
        assert_eq!(
            std::fs::read_dir(plans_dir(directory.path()))
                .unwrap()
                .count(),
            1,
            "no plan or hardware file is kept beside it"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_token_can_sit_where_the_person_put_it() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("on-a-usb-drive.token");
        let stored = store_token(
            directory.path(),
            &"a".repeat(32),
            "token-bytes",
            Some(&token_file),
        )
        .unwrap();
        assert_eq!(stored, token_file);
        assert_eq!(std::fs::read_to_string(&token_file).unwrap(), "token-bytes");
    }

    /// The token names devices' enrollment endpoints, never a session's own
    /// endpoint: a machine dials what answers enrollments.
    #[test]
    fn a_token_names_the_enrollment_endpoint_of_the_device_that_made_it() {
        use tilo_device_identity::persist::{
            write_device_cert, write_public_key, write_signing_key,
        };
        let root = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(root.path());
        paths.ensure_directories().unwrap();
        let user = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        write_public_key(&paths.user_pubkey, &user.verifying_key().to_bytes()).unwrap();
        write_signing_key(&paths.device_privkey, &device).unwrap();
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: user.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &user).unwrap();
        write_device_cert(&paths.device_cert, &cert).unwrap();
        write_device_cert(
            &paths.device_cert_for_user(&user.verifying_key().to_bytes()),
            &cert,
        )
        .unwrap();

        let home_peers = tilo_server_protocol::home_peer_registry_path(root.path());
        let (encoded, _) = mint(&paths, &home_peers).unwrap();
        let token = tilo_user_identity::decode_enrollment_token(&encoded).unwrap();
        assert!(
            token.dial.iter().any(
                |dial| dial.endpoint == enrollment_endpoint(&device.verifying_key().to_bytes())
            ),
            "the token names the device's enrollment endpoint: {:?}",
            token.dial
        );
        assert!(
            token
                .dial
                .iter()
                .all(|dial| dial.endpoint != device.verifying_key().to_bytes()),
            "and not the session's own endpoint"
        );
    }
}
