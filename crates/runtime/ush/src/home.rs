//! `~`: the workspace's folder, which every environment of the workspace
//! sees as its home, on the Tilo side and in its guest (docs/app-model/ush.md,
//! "Home").

use std::path::{Path, PathBuf};

use tilo_environment::{Environment, HomePolicy};

use crate::guest::GuestUser;
use crate::records::{RecordError, Records};
use crate::store::Store;
use crate::supervisor::{Resolved, Session};

impl Session {
    /// The record this session names ([`Records::resolve`]), with its home.
    ///
    /// # Errors
    ///
    /// The record could not be resolved or given its home.
    pub fn resolve_record(&self) -> anyhow::Result<(String, Environment)> {
        let records = self.records();
        let (key, environment) =
            records.resolve(self.opts.environment.as_deref(), &*self.images)?;
        let environment = with_home(&records, &self.store, &key, environment)?;
        Ok((key, environment))
    }
}

/// `environment` (the record `key`) with its home: the workspace's folder,
/// which every environment of the workspace sees at `~`. The first time a
/// device with the workspace opens an environment that has no home, the
/// folder becomes its home and is granted to it ([`Environment::SET_HOME`]);
/// a grant the person revokes later stays revoked. Nothing an environment
/// held is moved or deleted: what its home in the store held before stays
/// where it is, granted read-only so the shell still reads it at that path.
///
/// # Errors
///
/// The record refused the change or could not be written.
pub fn with_home(
    records: &Records,
    store: &Store,
    key: &str,
    environment: Environment,
) -> Result<Environment, RecordError> {
    if environment.home != HomePolicy::Persistent || !environment.home_dir.is_empty() {
        return Ok(environment);
    }
    let Some(workspace) = store.workspace() else {
        return Ok(environment);
    };
    let mut environment = records.apply(
        key,
        Environment::SET_HOME.with(workspace.to_string_lossy().into_owned()),
    )?;
    let old = store.environment_home(key);
    if std::fs::read_dir(&old).is_ok_and(|mut entries| entries.next().is_some()) {
        environment = records.apply(
            key,
            Environment::GRANT_DIR.with((old.to_string_lossy().into_owned(), false)),
        )?;
    }
    Ok(environment)
}

/// Where `~` is for `environment`'s shell, on the Tilo side and in its
/// guest alike: its home, the workspace's folder; else, where this device
/// has no workspace, the home its guest binds at `/home/<user>`.
#[must_use]
pub fn home_of(environment: &Environment, user: &GuestUser) -> PathBuf {
    match environment.home {
        tilo_environment::HomePolicy::Persistent if !environment.home_dir.is_empty() => {
            PathBuf::from(&environment.home_dir)
        }
        _ => user.home(),
    }
}

/// Where `~` is in `resolved`'s guest, the directory on this device that
/// is, and whether the guest reaches it through its grants (the workspace's
/// folder) rather than a bind of its own at `/home/<user>` (no workspace
/// here, or an ephemeral home).
#[must_use]
pub fn guest_home(
    resolved: &Resolved,
    store: &Store,
    session_dir: &Path,
    user: &GuestUser,
) -> (PathBuf, PathBuf, bool) {
    let environment = &resolved.environment;
    match environment.home {
        HomePolicy::Persistent if !environment.home_dir.is_empty() => {
            let home = PathBuf::from(&environment.home_dir);
            let source = match store.grant_root() {
                Some(root) => root.join(home.strip_prefix("/").unwrap_or(&home)),
                None => home.clone(),
            };
            (home, source, true)
        }
        HomePolicy::Persistent => (user.home(), store.environment_home(&resolved.key), false),
        HomePolicy::Ephemeral => (user.home(), session_dir.join("home"), false),
    }
}

/// Give a fresh guest home the dotfiles that make a shell usable. Never
/// seeded from the user's own home: an environment's home is its own.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "the `${VAR}` below is shell syntax in the .bashrc content, not a Rust format placeholder"
)]
pub(crate) fn seed_guest_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let bashrc = home.join(".bashrc");
    if bashrc.exists() {
        return Ok(());
    }
    std::fs::write(
        &bashrc,
        "# ush guest shell\n\
         [ -f /etc/bashrc ] && . /etc/bashrc\n\
         PS1='[\\[\\e[36m\\]${TILO_USH_ENVIRONMENT:-ush}\\[\\e[0m\\] \\W]\\$ '\n\
         alias ls='ls --color=auto'\n",
    )
}
