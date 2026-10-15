//! The reclaim run against a fixture, for the destructive loopback proof
//! (`os/tests/staging_reclaim_loop.sh`) and other test harnesses: the record
//! path, root mount, lock and loop allowance come from the environment here,
//! never in the shipped `tilo-staging-reclaim`.
//!
//! `TILO_STAGING_RECLAIM_RECORD`, `TILO_STAGING_RECLAIM_ROOT_MOUNT`,
//! `TILO_STAGING_RECLAIM_SKIP_LOCK=1`, `TILO_STAGING_RECLAIM_ALLOW_LOOP=1`;
//! the tools are found on `PATH`.

use std::path::PathBuf;
use std::process::ExitCode;

use advisory_lock::FileLock;
use tilo_staging_reclaim::{Error, Host, System};

struct Fixture;

impl Host for Fixture {
    fn record(&self) -> PathBuf {
        std::env::var_os("TILO_STAGING_RECLAIM_RECORD")
            .map_or_else(|| System.record(), PathBuf::from)
    }

    fn root_mount(&self) -> PathBuf {
        std::env::var_os("TILO_STAGING_RECLAIM_ROOT_MOUNT")
            .map_or_else(|| System.root_mount(), PathBuf::from)
    }

    fn allow_loop(&self) -> bool {
        std::env::var_os("TILO_STAGING_RECLAIM_ALLOW_LOOP").is_some_and(|v| !v.is_empty())
    }

    fn lock(&self, disk: &str) -> Result<Option<FileLock>, Error> {
        if std::env::var_os("TILO_STAGING_RECLAIM_SKIP_LOCK").is_some_and(|v| !v.is_empty()) {
            return Ok(None);
        }
        System.lock(disk)
    }
}

fn main() -> ExitCode {
    let code = tilo_staging_reclaim::main(&Fixture);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
