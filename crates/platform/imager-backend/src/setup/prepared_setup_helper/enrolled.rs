//! Root-owned machine-custody phase before target-user persona activation.
use super::{failed, private_parent};
use crate::setup::prepared_setup::{IdentityKind, PreparedSetupReview};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;
#[derive(Serialize)]
struct Receipt {
    identity_public_key: [u8; 32],
    enrolled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_public_key: Option<[u8; 32]>,
}
/// Authenticate the selected setup and publish only its enrolled machine custody.
/// Legacy root-key backups authenticate without changing machine files. The
/// privileged caller owns the mounted target and repairs canonical public/group
/// permissions through the existing firstboot service before user activation.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    // SAFETY: geteuid has no input pointers or preconditions.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(failed());
    }
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(name) = args.next() {
        let name = name.to_str().ok_or_else(failed)?;
        if !["--package", "--quarantine", "--system-root"].contains(&name)
            || values.contains_key(name)
        {
            return Err(failed());
        }
        let value = PathBuf::from(args.next().ok_or_else(failed)?);
        if !value.is_absolute() {
            return Err(failed());
        }
        values.insert(name.to_owned(), value);
    }
    if values.len() != 3 {
        return Err(failed());
    }
    private_parent(&values["--package"])?;
    private_parent(&values["--quarantine"])?;
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failed());
    }
    let review = PreparedSetupReview::open(
        &values["--package"],
        &password,
        &values["--quarantine"],
        &AtomicBool::new(false),
    )?;
    review.publish_enrolled_system(&values["--system-root"], &password)?;
    let device_public_key = match review.manifest().identity_kind {
        IdentityKind::SingleDeviceBackup => None,
        IdentityKind::CurrentDevice { .. } => return Err(failed()),
        IdentityKind::EnrolledDevice { device_public_key } => Some(device_public_key),
    };
    serde_json::to_writer(
        &mut *output,
        &Receipt {
            identity_public_key: review.manifest().identity_public_key,
            enrolled: device_public_key.is_some(),
            device_public_key,
        },
    )
    .map_err(|_| failed())?;
    output.write_all(b"\n")?;
    output.flush()
}
