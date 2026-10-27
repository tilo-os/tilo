//! One early-boot unlock attempt, from the token to the answered prompt.
//!
//! 1. The caller reads the token from the volume's header and opens its
//!    material (the TPM unseals it where there is one), and brings up the
//!    network.
//! 2. [`attempt`] asks the siblings ([`Siblings`]) for their half, for this
//!    boot, signing each sibling's fresh challenge with the boot key.
//! 3. It derives the slot passphrase, checks it against the slot, and answers
//!    systemd-cryptsetup's passphrase request with it.
//!
//! The passphrase prompt is up the whole time and is never touched unless
//! the derived passphrase opens the slot. Any failure ends the attempt at
//! that prompt.

use crate::askpass;
use crate::material::Material;
use crate::token::Token;
use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tilo_unlock_protocol::{BootId, BootKeyCert, Half, RefusalReason, derive_slot_key};

/// Why no sibling released its half. Each is in the owner's history on the
/// sibling's side too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The owner denied the request, or nobody decided in time.
    Denied,
    /// A sibling refused before asking the owner.
    Refused(RefusalReason),
    /// What was released does not open the slot.
    WrongHalf,
    /// No sibling could be reached.
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

/// The person's other devices, as early boot reaches them.
pub trait Siblings {
    /// Asks the token's siblings for the sibling half of `cert`'s slot, for
    /// boot `boot_id`.
    fn release(
        &self,
        token: &Token,
        cert: &BootKeyCert,
        boot_key: &SigningKey,
        boot_id: BootId,
    ) -> impl Future<Output = Result<Half, Refusal>> + Send;
}

/// Checks a derived passphrase against the slot. A seam for tests.
pub trait Volume {
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool;
}

/// How the attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A sibling's half opened the slot, and the prompt was answered.
    Unlocked,
    /// No sibling released the half; the prompt stays for the person.
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
    let sibling_half = match siblings.release(token, &cert, &boot_key, boot_id).await {
        Ok(half) => half,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    drop(boot_key);
    let passphrase = derive_slot_key(
        &material.device_half()?,
        &sibling_half,
        &cert.slot.volume_uuid,
    );
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

/// The real ask directory.
pub fn ask_dir() -> PathBuf {
    PathBuf::from(askpass::ASK_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{Sealing, Sibling, TOKEN_TYPE, TOKEN_VERSION};
    use std::os::unix::net::UnixDatagram;
    use std::sync::Mutex;
    use tilo_unlock_protocol::{HALF_LEN, PROTOCOL_VERSION, SlotId};

    struct FakeSiblings {
        answer: Result<[u8; HALF_LEN], Refusal>,
        seen: Mutex<Vec<BootId>>,
    }

    impl Siblings for FakeSiblings {
        async fn release(
            &self,
            _token: &Token,
            cert: &BootKeyCert,
            boot_key: &SigningKey,
            boot_id: BootId,
        ) -> Result<Half, Refusal> {
            assert_eq!(boot_key.verifying_key().to_bytes(), cert.boot_public_key);
            self.seen.lock().unwrap().push(boot_id);
            self.answer.clone().map(Half::from_bytes)
        }
    }

    struct FakeVolume(String);

    impl Volume for FakeVolume {
        fn opens(&self, keyslot: u32, passphrase: &str) -> bool {
            keyslot == 5 && passphrase == self.0
        }
    }

    const UUID: [u8; 16] = [7; 16];

    fn material() -> Material {
        Material {
            device_half: "11".repeat(HALF_LEN),
            boot_secret: "22".repeat(HALF_LEN),
            networks: vec![],
        }
    }

    fn token() -> Token {
        let boot = SigningKey::from_bytes(&[0x22; 32]);
        let cert = BootKeyCert {
            version: PROTOCOL_VERSION,
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            boot_public_key: boot.verifying_key().to_bytes(),
            slot: SlotId {
                volume_uuid: UUID,
                keyslot: 5,
            },
            attestation_key: None,
            issued_at_ns: 1,
            signature: [0; 64],
        };
        Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec!["5".into()],
            tilo_version: TOKEN_VERSION,
            cert: Token::encode_cert(&cert).unwrap(),
            siblings: vec![Sibling {
                device: "aa".repeat(32),
                label: String::new(),
                endpoint: String::new(),
            }],
            sealing: Sealing::Cleartext,
            material: String::new(),
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf, UnixDatagram) {
        let dir = tempfile::tempdir().unwrap();
        let ask_dir = dir.path().join("ask");
        std::fs::create_dir(&ask_dir).unwrap();
        let socket = dir.path().join("sck");
        let receiver = UnixDatagram::bind(&socket).unwrap();
        std::fs::write(
            ask_dir.join("ask.x"),
            format!(
                "[Ask]\nSocket={}\nNotAfter=0\nId=cryptsetup:/dev/vda3\n",
                socket.display()
            ),
        )
        .unwrap();
        (dir, ask_dir, receiver)
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

    #[tokio::test]
    async fn a_released_half_answers_the_prompt() {
        let (_dir, ask_dir, receiver) = setup();
        let expected = derive_slot_key(
            &Half::from_bytes([0x11; HALF_LEN]),
            &Half::from_bytes([0x33; HALF_LEN]),
            &UUID,
        );
        let siblings = FakeSiblings {
            answer: Ok([0x33; HALF_LEN]),
            seen: Mutex::default(),
        };
        let outcome = attempt(
            Path::new("/dev/vda3"),
            &token(),
            &material(),
            BootId([9; 16]),
            &siblings,
            &FakeVolume(expected.to_string()),
            &ask_dir,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Unlocked);
        assert_eq!(siblings.seen.lock().unwrap().as_slice(), &[BootId([9; 16])]);
        let mut buffer = [0_u8; 80];
        let n = receiver.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], format!("+{}", *expected).as_bytes());
    }

    #[tokio::test]
    async fn a_refusal_or_a_wrong_half_leaves_the_prompt_alone() {
        let (_dir, ask_dir, receiver) = setup();
        receiver.set_nonblocking(true).unwrap();
        let volume = FakeVolume("never".into());
        for (answer, expected) in [
            (Err(Refusal::Denied), Refusal::Denied),
            (
                Err(Refusal::Refused(RefusalReason::Revoked)),
                Refusal::Refused(RefusalReason::Revoked),
            ),
            (Ok([0x44; HALF_LEN]), Refusal::WrongHalf),
        ] {
            let siblings = FakeSiblings {
                answer,
                seen: Mutex::default(),
            };
            let outcome = attempt(
                Path::new("/dev/vda3"),
                &token(),
                &material(),
                BootId([9; 16]),
                &siblings,
                &volume,
                &ask_dir,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
            assert_eq!(outcome, Outcome::Refused(expected));
        }
        assert!(
            receiver.recv(&mut [0_u8; 8]).is_err(),
            "the prompt was answered"
        );
    }
}
