//! `tilo-staging-handoff` against fixtures, for the destructive loopback
//! proofs (`os/tests/staging_helper_loop.sh`, `os/tests/staging_reclaim_loop.sh`)
//! and other test harnesses: the firmware variables, the staging ESP, lsblk's
//! output and efibootmgr come from the environment here, never in the shipped
//! binary.
//!
//! `TILO_STAGING_EFIVARS` (an efivarfs directory), `TILO_STAGING_ESP_DIR`
//! (the staging ESP's contents), `TILO_STAGING_LSBLK_JSON` (lsblk's JSON) and
//! `TILO_STAGING_EFIBOOTMGR` (an efibootmgr program); each unset one is the
//! system's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use tilo_install_helpers::staging_handoff::{self, Fail, Host, System, read_bounded};

struct Fixture;

fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

impl Host for Fixture {
    fn efivars(&self) -> PathBuf {
        env("TILO_STAGING_EFIVARS").map_or_else(|| System.efivars(), PathBuf::from)
    }

    fn efibootmgr(&self) -> String {
        env("TILO_STAGING_EFIBOOTMGR").map_or_else(
            || System.efibootmgr(),
            |value| value.to_string_lossy().into_owned(),
        )
    }

    fn esp_handoff(&self, esp_partuuid: &str) -> Result<Vec<u8>, Fail> {
        match env("TILO_STAGING_ESP_DIR") {
            Some(directory) => {
                read_bounded(&Path::new(&directory).join("EFI/tilo-staging/handoff.json"))
            }
            None => System.esp_handoff(esp_partuuid),
        }
    }

    fn lsblk(&self, disk: &str) -> Result<Value, Fail> {
        match env("TILO_STAGING_LSBLK_JSON") {
            Some(path) => {
                let text = std::fs::read_to_string(path)?;
                serde_json::from_str(&text).map_err(|error| Fail::Os(error.to_string()))
            }
            None => System.lsblk(disk),
        }
    }
}

/// `decode-load-option EFIVAR`: the installer's decoding of a `Boot####`
/// efivarfs file (attributes first), as JSON, or `null` when it is not a
/// staging entry. The staging loop proof checks the imager's entry with it.
fn decode(path: &Path) -> ExitCode {
    let Ok(raw) = std::fs::read(path) else {
        eprintln!("cannot read {}", path.display());
        return ExitCode::FAILURE;
    };
    let entry = raw
        .get(4..)
        .and_then(staging_handoff::decode_load_option)
        .map_or(Value::Null, |entry| {
            serde_json::json!({
                "partition_number": entry.partition_number,
                "start_lba": entry.start_lba,
                "size_lba": entry.size_lba,
                "partuuid": entry.partuuid,
                "path": entry.path,
            })
        });
    println!("{entry}");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let [command, path] = args.as_slice()
        && command == "decode-load-option"
    {
        return decode(Path::new(path));
    }
    staging_handoff::main(&Fixture, &args)
}
