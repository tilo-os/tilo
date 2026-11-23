//! The sibling way: another of the owner's devices releases its half of the
//! volume's sibling keyslot for this boot (docs/device-reach.md, "Unlocking
//! it after a reboot"). One attempt goes from the token to the answered
//! prompt, while the volume's other ways stay open.

use crate::luks;
use crate::sibling::exchange::{self, AttestationSource, Peers, Progress};
use crate::sibling::material::Material;
use crate::sibling::network;
use crate::sibling::token::{Sibling, Token};
use crate::tpm;
use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tilo_ask_password as askpass;
use tilo_rollback::exec::{Real, Runner};
use tilo_rollback::policy::Workspace;
use tilo_unlock_protocol::opened::UnlockMethod;
use tilo_unlock_protocol::{BootId, Half, RefusalReason, SlotId, derive_slot_key};
use tilo_user_identity::DeviceCert;

/// Why no sibling released its half. Each is in the owner's history on the
/// sibling's side too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Denied,
    /// A sibling refused before asking the owner.
    Refused(RefusalReason),
    WrongHalf,
    Unreachable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => f.write_str("the owner denied the unlock"),
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::WrongHalf => f.write_str("the released half does not open this volume"),
            Self::Unreachable(reason) => write!(f, "no device answered: {reason}"),
        }
    }
}

pub trait Siblings {
    /// Asks the token's devices for the sibling half of `slot`, for boot
    /// `boot_id`.
    fn release(
        &self,
        token: &Token,
        cert: &DeviceCert,
        boot_key: &SigningKey,
        boot_id: BootId,
        slot: SlotId,
    ) -> impl Future<Output = Result<Half, Refusal>> + Send;
}

pub trait Volume {
    /// Whether `passphrase` opens the volume's `keyslot`.
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool;
    /// The volume's LUKS2 UUID, as its 16 bytes.
    ///
    /// # Errors
    /// When the volume's header cannot be read.
    fn uuid(&self) -> Result<[u8; 16]>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Unlocked,
    Refused(Refusal),
}

/// This boot's id: the kernel's random boot id, which the booted system can
/// read too.
pub fn boot_id(path: &Path) -> Result<BootId> {
    let text = std::fs::read_to_string(path).context("this boot has no boot id")?;
    let hex_digits: String = text.trim().chars().filter(|c| *c != '-').collect();
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(&hex_digits, &mut bytes).context("the boot id is not a UUID")?;
    Ok(BootId(bytes))
}

/// Runs one attempt for the volume on `device`.
///
/// # Errors
/// When the token or material is malformed, or answering the prompt fails;
/// every refusal is an [`Outcome`].
pub async fn attempt(
    device: &Path,
    token: &Token,
    material: &Material,
    boot_id: BootId,
    siblings: &(impl Siblings + Sync),
    volume: &(impl Volume + Sync),
    ask_dir: &Path,
    answer_within: Duration,
) -> Result<Outcome> {
    let keyslot = token.keyslot()?;
    let cert = token.cert()?;
    let boot_key = material.boot_key()?;
    let volume_uuid = volume.uuid()?;
    let slot = SlotId {
        volume_uuid,
        keyslot: u16::try_from(keyslot).context("key slot out of range")?,
    };
    let sibling_half = match siblings
        .release(token, &cert, &boot_key, boot_id, slot)
        .await
    {
        Ok(half) => half,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    drop(boot_key);
    let passphrase = derive_slot_key(&material.device_half()?, &sibling_half, &volume_uuid);
    drop(sibling_half);
    if !volume.opens(keyslot, &passphrase) {
        return Ok(Outcome::Refused(Refusal::WrongHalf));
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(answer_within)
        .context("the answer deadline is out of range")?;
    loop {
        if let Some(ask) = askpass::pending(ask_dir, device).first() {
            askpass::answer(ask, &passphrase)?;
            return Ok(Outcome::Unlocked);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("the passphrase prompt never asked");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub fn ask_dir() -> PathBuf {
    PathBuf::from(askpass::ASK_DIR)
}

/// A line on the console (through the kernel log, which the initramfs
/// forwards there) and in the journal, as `tilo-unlock`.
pub fn say(message: &str) {
    eprintln!("{message}");
    if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(kmsg, "<5>tilo-unlock: {message}");
    }
}

/// The boot screen's part in an attempt, and what its answers say of the
/// unlock: a sibling that asked the owner released after their answer, and
/// one that released without asking did so on the boot chain this machine
/// attested.
struct BootScreen {
    asked: AtomicBool,
}

impl BootScreen {
    /// Which way a sibling released: the owner's answer after being asked, or
    /// the boot chain's own, released with nobody asked.
    fn method(asked: bool) -> UnlockMethod {
        if asked {
            UnlockMethod::PersonDevice
        } else {
            UnlockMethod::SiblingServer
        }
    }
}

/// Show `message` on the boot screen, where plymouth is up.
fn screen(message: &str) {
    let _ = std::process::Command::new("plymouth")
        .args(["display-message", &format!("--text={message}")])
        .status();
}

/// How the boot screen names the device that answered.
fn device_name(sibling: &Sibling) -> &str {
    if sibling.label.is_empty() {
        "your other device"
    } else {
        &sibling.label
    }
}

/// What the boot screen shows once the person's device answered: its answer,
/// word for word, so a boot that hears it is not left thinking no device
/// answered.
fn decided_message(name: &str, allowed: bool) -> String {
    if allowed {
        format!("{name} released the key for this disk")
    } else {
        format!("{}; enter the passphrase to unlock", Refusal::Denied)
    }
}

impl Progress for BootScreen {
    fn pending(&self, sibling: &Sibling, approval_code: &str) {
        self.asked.store(true, Ordering::Relaxed);
        let message = format!(
            "Approve unlocking on {}: code {approval_code}",
            device_name(sibling)
        );
        say(&message);
        screen(&message);
    }

    fn decided(&self, sibling: &Sibling, allowed: bool) {
        let message = decided_message(device_name(sibling), allowed);
        say(&message);
        screen(&message);
    }
}

struct RealVolume<'a> {
    runner: &'a Real,
    ws: &'a Workspace,
    device: &'a Path,
}

impl Volume for RealVolume<'_> {
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool {
        luks::opens(
            self.runner,
            self.ws,
            self.device,
            passphrase.as_bytes(),
            keyslot,
        )
        .unwrap_or(false)
    }

    fn uuid(&self) -> Result<[u8; 16]> {
        luks::uuid(self.runner, self.device)
    }
}

/// Whether `device` carries a sibling-unlock token.
pub fn enrolled(runner: &dyn Runner, device: &Path) -> bool {
    matches!(luks::sibling(runner, device), Ok(Some(_)))
}

/// Ask the owner's other devices to unlock `device` for this boot, and
/// answer its passphrase prompt with the result. A volume without the
/// token has nothing to ask; a SIGTERM (the volume opened another way) ends
/// the attempt, and everything it held is wiped on the way out.
///
/// # Errors
///
/// The token or material is malformed, or answering the prompt fails.
pub async fn unlock(runner: &Real, device: &Path) -> Result<()> {
    let Some(token) = luks::sibling(runner, device)? else {
        return Ok(());
    };
    token.validate()?;
    let material = Material::from_json(&tpm::unseal_material(
        runner,
        token.sealing,
        &token.material,
    )?)?;
    let boot_id = boot_id(Path::new("/proc/sys/kernel/random/boot_id"))?;
    let _network = match network::Network::up(runner, &material.networks) {
        Ok(network) => Some(network),
        Err(error) => {
            say(&format!("could not bring up the network: {error:#}"));
            None
        }
    };
    say("asking your other devices to unlock this disk");
    let attestation = material.attestation.clone().map(|key| AttestationSource {
        tpm: runner.clone(),
        key,
    });
    let screen = BootScreen {
        asked: AtomicBool::new(false),
    };
    let peers = Peers {
        progress: &screen,
        transcript: Some(Path::new(exchange::TRANSCRIPT)),
        attestation: attestation.as_ref(),
    };
    let ask_dir = ask_dir();
    let dir = crate::scratch()?;
    let ws = Workspace::new(dir.path());
    let volume = RealVolume {
        runner,
        ws: &ws,
        device,
    };
    let attempt = attempt(
        device,
        &token,
        &material,
        boot_id,
        &peers,
        &volume,
        &ask_dir,
        Duration::from_secs(600),
    );
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("no SIGTERM handler")?;
    let outcome = tokio::select! {
        outcome = attempt => outcome?,
        _ = terminate.recv() => {
            say("the disk was opened another way");
            return Ok(());
        }
    };
    match outcome {
        Outcome::Unlocked => {
            // The volume is open: how it opened is what the booted system
            // reports, and a note that cannot be written stops nothing.
            let asked = screen.asked.load(Ordering::Relaxed);
            if let Err(error) = super::note_opening(BootScreen::method(asked)) {
                say(&format!("how the volume opened is not noted ({error:#})"));
            }
            say("unlocked with the consent of your other device");
        }
        Outcome::Refused(refusal) => say(&format!("{refusal}; enter the passphrase to unlock")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_unlock_says_whether_the_owner_answered_it() {
        assert_eq!(
            BootScreen::method(false),
            UnlockMethod::SiblingServer,
            "released on the boot chain this machine attested, nobody asked"
        );
        assert_eq!(
            BootScreen::method(true),
            UnlockMethod::PersonDevice,
            "released after the owner answered on one of their devices"
        );
    }

    #[test]
    fn a_denial_is_shown_as_the_person_denying_it() {
        let denied = decided_message("The phone", false);
        assert_eq!(
            denied,
            "the owner denied the unlock; enter the passphrase to unlock"
        );
        assert!(
            !denied.contains("no device answered"),
            "a boot that heard the answer is not left thinking nobody answered"
        );
        assert_eq!(
            decided_message("The phone", true),
            "The phone released the key for this disk"
        );
    }

    #[test]
    fn reads_the_kernel_boot_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot_id");
        std::fs::write(&path, "5b1c0a2e-0000-4000-8000-00000000000f\n").unwrap();
        let id = boot_id(&path).unwrap();
        assert_eq!(id.0[0], 0x5b);
        assert_eq!(id.0[15], 0x0f);
        std::fs::write(&path, "nope\n").unwrap();
        assert!(boot_id(&path).is_err());
    }
}
