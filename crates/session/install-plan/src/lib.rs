//! An install plan for a machine at another place, made the one way the
//! command line and the installer's window both make it.
//!
//! [`Choices`] holds what the person decided now; every choice they did not
//! decide stays [`None`] and is open, so the machine asks one of their devices
//! for it against its own hardware. [`Choices::plan`] writes the document and
//! [`open`](Choices::open) is the list of what it still asks, computed here so
//! the plan's fields and its `open` list cannot disagree.
//!
//! [`mint`] makes the single-use enrollment token the medium carries. The plan
//! names the token by its nonce and expiry only, so the document travels
//! through the person's devices, a log or an action's answer without exposing
//! the token; the token itself goes to a file only its account can read
//! ([`store`]).

use std::path::{Path, PathBuf};

use install_profile_model::plan::{self, DiskRule, Holding, Network, Plan, Token};
use tilo_user_identity::persist::{read_device_cert, read_public_key, read_signing_key};
use tilo_user_identity::{
    DeviceCapabilities, IdentityPaths, encode_enrollment_token, mint_enrollment_token,
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
    let device = read_signing_key(&system.device_privkey)
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
        dial_peers(home_peers, user),
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

/// The person's devices this computer knows, as an enrollment token names
/// them: where each is reached over the network, so a machine handed only a
/// token can dial the devices that admit it from anywhere.
#[must_use]
pub fn dial_peers(home_peers: &Path, user: [u8; 32]) -> Vec<tilo_user_identity::Dial> {
    let Ok(peers) = tilo_server_protocol::read_home_peers(home_peers) else {
        return Vec::new();
    };
    peers
        .into_iter()
        .filter(|peer| peer.user_public_key == hex::encode(user))
        .filter_map(|peer| peer.server_endpoint)
        .filter(|endpoint| !endpoint.is_empty())
        .map(|endpoint| tilo_user_identity::Dial {
            endpoint: *endpoint.id.as_bytes(),
            relay_url: endpoint
                .relay_urls()
                .next()
                .map(ToString::to_string)
                .unwrap_or_default(),
        })
        .collect()
}

/// Where the person's devices keep a plan and its token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The plan document, readable by the person's devices and agents.
    pub plan_file: PathBuf,
    /// The single-use token, readable and writable only by this account.
    pub token_file: PathBuf,
}

/// Write `plan` and the encoded `token` where this computer keeps them, both
/// readable only by this account. `token_file` says where the token goes when
/// the person wants it somewhere else.
///
/// # Errors
///
/// The store cannot be made or a file cannot be written.
pub fn store(
    identity_dir: &Path,
    plan: &Plan,
    token: &str,
    token_file: Option<&Path>,
) -> Result<Stored, String> {
    let plans = plans_dir(identity_dir);
    std::fs::create_dir_all(&plans)
        .map_err(|error| format!("cannot make {}: {error}", plans.display()))?;
    let plan_file = plans.join(format!("{}.json", plan.plan));
    let bytes = serde_json::to_vec_pretty(plan).map_err(|error| error.to_string())?;
    write_private(&plan_file, &bytes)?;
    let token_file = token_file.map_or_else(
        || plans.join(format!("{}.token", plan.plan)),
        Path::to_path_buf,
    );
    write_private(&token_file, token.as_bytes())?;
    Ok(Stored {
        plan_file,
        token_file,
    })
}

/// The directory a computer's plans and tokens live in.
#[must_use]
pub fn plans_dir(identity_dir: &Path) -> PathBuf {
    identity_dir.join("install-plans")
}

/// The plan files this computer keeps, in name order.
///
/// # Errors
///
/// The store exists and cannot be read.
pub fn plan_files(identity_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let plans = plans_dir(identity_dir);
    let mut found = Vec::new();
    match std::fs::read_dir(&plans) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    found.push(path);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    found.sort();
    Ok(found)
}

/// The plan at `path`.
///
/// # Errors
///
/// The file cannot be read or is not a plan.
pub fn read(path: &Path) -> Result<Plan, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("{} is not a plan", path.display()))
}

/// The machines this computer planned, as the person's devices keep them.
///
/// # Errors
///
/// The store exists and cannot be read.
pub fn waiting(identity_dir: &Path) -> std::io::Result<Vec<Plan>> {
    let mut plans = Vec::new();
    for path in plan_files(identity_dir)? {
        if let Ok(plan) = read(&path) {
            plans.push(plan);
        }
    }
    Ok(plans)
}

/// What the destination reported about its own hardware, beside its plan:
/// the drives a disk rule is answered against.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hardware {
    /// The destination's real drives.
    #[serde(default)]
    pub drives: Vec<plan::Drive>,
}

/// Where a destination's hardware report is kept beside its plan.
#[must_use]
pub fn hardware_file(identity_dir: &Path, plan_id: &str) -> PathBuf {
    plans_dir(identity_dir).join(format!("{plan_id}.hardware.json"))
}

/// The destination's reported hardware, when it has reported any.
#[must_use]
pub fn read_hardware(identity_dir: &Path, plan_id: &str) -> Option<Hardware> {
    let bytes = std::fs::read(hardware_file(identity_dir, plan_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write `bytes` to `path`, readable and writable only by this account.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot make {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
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
    fn the_store_keeps_a_plan_and_the_token_only_its_account_reads() {
        let directory = tempfile::tempdir().unwrap();
        let plan = server().plan(&"a".repeat(32), token()).unwrap();
        let stored = store(directory.path(), &plan, "token-bytes", None).unwrap();
        assert_eq!(
            stored.plan_file,
            plans_dir(directory.path()).join(format!("{}.json", plan.plan))
        );
        assert_eq!(read(&stored.plan_file).unwrap(), plan);
        assert_eq!(
            std::fs::read_to_string(&stored.token_file).unwrap(),
            "token-bytes"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&stored.token_file)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(waiting(directory.path()).unwrap(), vec![plan]);
    }

    #[test]
    fn a_token_can_sit_where_the_person_put_it() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("on-a-usb-drive.token");
        let plan = server().plan(&"a".repeat(32), token()).unwrap();
        let stored = store(directory.path(), &plan, "token-bytes", Some(&token_file)).unwrap();
        assert_eq!(stored.token_file, token_file);
        assert_eq!(std::fs::read_to_string(&token_file).unwrap(), "token-bytes");
    }

    #[test]
    fn the_hardware_beside_a_plan_answers_its_disk_question() {
        let directory = tempfile::tempdir().unwrap();
        let id = "a".repeat(32);
        let hardware = Hardware {
            drives: vec![
                plan::Drive {
                    model: "boot".to_owned(),
                    identity: "s-boot".to_owned(),
                    bytes: 512,
                    holds_data: true,
                    boots: true,
                },
                plan::Drive {
                    model: "fresh".to_owned(),
                    identity: "s-fresh".to_owned(),
                    bytes: 4096,
                    holds_data: false,
                    boots: false,
                },
            ],
        };
        std::fs::create_dir_all(plans_dir(directory.path())).unwrap();
        std::fs::write(
            hardware_file(directory.path(), &id),
            serde_json::to_vec(&hardware).unwrap(),
        )
        .unwrap();
        let read = read_hardware(directory.path(), &id).unwrap();
        assert_eq!(read, hardware);
        let rule = DiskRule {
            kind: "largest".to_owned(),
            identity: String::new(),
        };
        assert_eq!(
            plan::choose(&rule, &read.drives),
            plan::Choice::Drive(&read.drives[1])
        );
        assert!(read_hardware(directory.path(), &"b".repeat(32)).is_none());
    }
}
