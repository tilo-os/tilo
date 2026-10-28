//! Local accounts, and the old password database people are moved off.

use anyhow::{Context as _, Result, bail};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub display_name: String,
}

/// `/etc/passwd` and `/etc/group` under `root`.
pub struct Accounts {
    root: PathBuf,
}

impl Accounts {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read(&self, file: &str) -> Result<String> {
        let path = self.root.join(file);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    fn accounts(&self) -> Result<Vec<Account>> {
        Ok(self
            .read("etc/passwd")?
            .lines()
            .filter_map(parse_passwd)
            .collect())
    }

    pub fn by_name(&self, name: &str) -> Result<Option<Account>> {
        Ok(self.accounts()?.into_iter().find(|a| a.name == name))
    }

    pub fn by_uid(&self, uid: u32) -> Result<Option<Account>> {
        Ok(self.accounts()?.into_iter().find(|a| a.uid == uid))
    }

    fn in_group(&self, account: &Account, group: &str) -> Result<bool> {
        Ok(self.read("etc/group")?.lines().any(|line| {
            let mut fields = line.split(':');
            let (Some(name), Some(_), Some(gid), Some(members)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return false;
            };
            name == group
                && (gid.parse() == Ok(account.gid) || members.split(',').any(|m| m == account.name))
        }))
    }

    /// The device admin: in `wheel` and in `tilo-identity`.
    pub fn is_admin(&self, account: &Account) -> Result<bool> {
        Ok(self.in_group(account, "wheel")? && self.in_group(account, "tilo-identity")?)
    }
}

fn parse_passwd(line: &str) -> Option<Account> {
    let mut fields = line.split(':');
    let name = fields.next()?.to_owned();
    let _password = fields.next()?;
    let uid = fields.next()?.parse().ok()?;
    let gid = fields.next()?.parse().ok()?;
    let gecos = fields.next()?;
    let display = gecos.split(',').next().unwrap_or_default().trim();
    Some(Account {
        display_name: if display.is_empty() {
            name.clone()
        } else {
            display.to_owned()
        },
        name,
        uid,
        gid,
    })
}

/// The password check people are moved off, and locking it once they are.
pub trait Legacy: Send + Sync {
    /// Whether `typed` is the person's old password.
    fn verify(&self, name: &str, typed: &str) -> Result<bool>;
    /// Whether the person still has a password hash some check would accept.
    fn has_password(&self, name: &str) -> Result<bool>;
    /// Remove the person's password hash, leaving the account locked to
    /// password checks other than this daemon's.
    fn lock_password(&self, name: &str) -> Result<()>;
}

/// `pam_unix` through the unlock helper, run as root against a service that
/// holds only `pam_unix`, `/etc/shadow` and `usermod`.
pub struct SystemLegacy {
    pub helper: PathBuf,
    pub service: String,
    pub shadow: PathBuf,
    pub usermod: PathBuf,
}

impl Default for SystemLegacy {
    fn default() -> Self {
        Self {
            helper: PathBuf::from("/usr/libexec/tilo-unlock-helper"),
            service: "tilo-auth-legacy".into(),
            shadow: PathBuf::from("/etc/shadow"),
            usermod: PathBuf::from("/usr/sbin/usermod"),
        }
    }
}

impl Legacy for SystemLegacy {
    fn verify(&self, name: &str, typed: &str) -> Result<bool> {
        if typed.contains('\n') {
            return Ok(false);
        }
        let mut child = Command::new(&self.helper)
            .arg(name)
            .env("TILO_UNLOCK_PAM_SERVICE", &self.service)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("running {}", self.helper.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(typed.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .context("handing the password to the check")?;
        }
        match child.wait()?.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            other => bail!("the old password check could not run (exit {other:?})"),
        }
    }

    fn has_password(&self, name: &str) -> Result<bool> {
        let shadow = std::fs::read_to_string(&self.shadow)
            .with_context(|| format!("reading {}", self.shadow.display()))?;
        Ok(shadow_has_password(&shadow, name))
    }

    fn lock_password(&self, name: &str) -> Result<()> {
        let status = Command::new(&self.usermod)
            .args(["--password", "!", name])
            .stdin(Stdio::null())
            .status()
            .with_context(|| format!("running {}", self.usermod.display()))?;
        if !status.success() {
            bail!("locking {name}'s old password failed ({status})");
        }
        Ok(())
    }
}

/// Whether `name`'s entry in a shadow file holds anything but a locked or
/// disabled password: an empty one, which `nullok` accepts, counts.
fn shadow_has_password(shadow: &str, name: &str) -> bool {
    shadow.lines().any(|line| {
        let mut fields = line.split(':');
        fields.next() == Some(name)
            && fields
                .next()
                .is_some_and(|hash| !hash.starts_with('!') && !hash.starts_with('*'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\n\
             greetd:x:977:977::/var/lib/greetd:/sbin/nologin\n\
             ada:x:1000:1000:Ada Lovelace,,,:/home/ada:/bin/bash\n\
             bob:x:1001:1001::/home/bob:/bin/bash\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("etc/group"),
            "wheel:x:10:ada,bob\n\
             tilo-identity:x:960:ada\n\
             ada:x:1000:\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn accounts_resolve_by_name_and_uid_with_their_display_name() {
        let dir = root();
        let accounts = Accounts::new(dir.path());
        let ada = accounts.by_name("ada").unwrap().unwrap();
        assert_eq!(ada.uid, 1000);
        assert_eq!(ada.display_name, "Ada Lovelace");
        assert_eq!(accounts.by_uid(1001).unwrap().unwrap().display_name, "bob");
        assert!(accounts.by_name("eve").unwrap().is_none());
    }

    #[test]
    fn the_admin_is_in_wheel_and_tilo_identity() {
        let dir = root();
        let accounts = Accounts::new(dir.path());
        assert!(
            accounts
                .is_admin(&accounts.by_name("ada").unwrap().unwrap())
                .unwrap()
        );
        assert!(
            !accounts
                .is_admin(&accounts.by_name("bob").unwrap().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn only_a_locked_or_disabled_shadow_entry_holds_no_password() {
        let shadow = "root:!:19000::::::\n\
                      ada:$y$j9T$salt$hash:19000:0:99999:7:::\n\
                      bob:!$y$j9T$salt$hash:19000::::::\n\
                      greetd:*:19000::::::\n\
                      eve::19000::::::\n";
        assert!(shadow_has_password(shadow, "ada"));
        assert!(shadow_has_password(shadow, "eve"));
        assert!(!shadow_has_password(shadow, "root"));
        assert!(!shadow_has_password(shadow, "bob"));
        assert!(!shadow_has_password(shadow, "greetd"));
        assert!(!shadow_has_password(shadow, "mallory"));
    }
}
