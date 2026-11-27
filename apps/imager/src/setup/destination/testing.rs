//! The live installer's backend as its tests run it: the disks it reads and the installs and
//! power actions it was asked for, with no disk touched.

use std::sync::{Arc, Mutex};

use crate::backend::destination::backend::InstallBackend;
use crate::backend::destination::disks::Disk;
use crate::backend::destination::install::{InstallOptions, PowerAction};

/// The recovery key every fake install prints.
pub(crate) const KEY: &str =
    "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
pub(crate) const SECRET: &str = "correct horse battery staple";

/// One install as the backend was asked for it: disk, account, password and options.
pub(crate) type Install = (String, String, String, InstallOptions);

pub(crate) struct FakeBackend {
    disks: Vec<Disk>,
    pub(crate) installs: Mutex<Vec<Install>>,
    pub(crate) power: Mutex<Vec<PowerAction>>,
    succeed: bool,
}

impl FakeBackend {
    /// The sample disks, each install succeeding.
    pub(crate) fn new() -> Arc<Self> {
        Self::finishing(true)
    }

    /// The sample disks, each install ending as `succeed` says.
    pub(crate) fn finishing(succeed: bool) -> Arc<Self> {
        Arc::new(Self {
            disks: Disk::samples(),
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed,
        })
    }

    /// `disks`, each install succeeding.
    pub(crate) fn with_disks(disks: Vec<Disk>) -> Arc<Self> {
        Arc::new(Self {
            disks,
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed: true,
        })
    }
}

impl InstallBackend for FakeBackend {
    fn read_disks(&self) -> Vec<Disk> {
        self.disks.clone()
    }

    fn run_install(
        &self,
        disk: &str,
        user: &str,
        password: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        self.installs.lock().unwrap().push((
            disk.to_owned(),
            user.to_owned(),
            password.to_owned(),
            opts.clone(),
        ));
        on_line("==> Installing Tilo onto the target".to_owned());
        on_line(format!(
            "{}{KEY}",
            super::installer_app::RECOVERY_KEY_PREFIX
        ));
        // A helper that printed the key raw must not leak it either.
        on_line(format!("cryptenroll: new key {KEY}"));
        on_line("==> Setting hostname 'lab-1' and time zone 'UTC'".to_owned());
        Ok(self.succeed)
    }

    fn run_alongside_preflight(
        &self,
        _disk: &str,
        _opts: &InstallOptions,
        _on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        Ok(true)
    }

    fn run_power(&self, action: PowerAction) -> std::io::Result<bool> {
        self.power.lock().unwrap().push(action);
        Ok(true)
    }
}
