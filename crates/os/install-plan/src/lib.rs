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
//! document, and where it goes is the session's to say. The medium carries the
//! single-use ticket the device that made the plan serves for it; the plan names
//! it by a nonce and expiry only ([`token`]), so the document travels through
//! the person's devices, a log or an action's answer without exposing it. The
//! ticket itself goes to a file only its account can read ([`store_token`]), the
//! one file this crate keeps.

use std::path::{Path, PathBuf};

use install_profile_model::plan::{self, DiskRule, Holding, Network, Plan, Token};
use tilo_device_identity::login::PLANNED_TICKET_LIFETIME_NS;

pub use install_profile_model::plan::{Commands, commands, installer_image};

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

/// What a plan names its ticket by: a nonce and the expiry of the ticket this device's daemon
/// serves for it ([`PLANNED_TICKET_LIFETIME_NS`]). The ticket itself is the daemon's to issue,
/// once the plan exists.
///
/// # Errors
///
/// This computer cannot say what time it is.
pub fn token() -> Result<Token, String> {
    let now_ns = tilo_clock::try_unix_now_ns()
        .ok_or_else(|| "this computer cannot say what time it is".to_owned())?;
    let expires_at_ns = now_ns
        .checked_add(PLANNED_TICKET_LIFETIME_NS)
        .ok_or_else(|| "the ticket's expiry is out of range".to_owned())?;
    Ok(Token {
        nonce: hex::encode(rand::random::<[u8; 16]>()),
        expires_at_ns,
    })
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
}
