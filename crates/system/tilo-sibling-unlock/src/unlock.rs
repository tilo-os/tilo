//! One early-boot unlock attempt, from the token to the answered prompt.
//!
//! 1. Read the token from the volume's header and open its material (the TPM
//!    unseals it where there is one).
//! 2. Bring up the network: wired links, and the owner's unlock networks.
//! 3. Ask the siblings ([`Siblings`]) for their half, for this boot.
//! 4. Derive the slot passphrase, check it against the slot, and answer
//!    systemd-cryptsetup's passphrase request with it.
//!
//! The passphrase prompt is up the whole time and is never touched unless
//! the derived passphrase opens the slot. Any failure ends the attempt
//! quietly at that prompt. Whatever happens, the material, the sibling half,
//! the network profiles and the links are wiped before this returns.

use crate::askpass;
use crate::material::{self, HALF_LEN, Material};
use crate::token::Token;
use anyhow::{Context, Result};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

/// What a sibling exchange is bound to.
#[derive(Debug, Clone)]
pub struct Request {
    /// This machine's device id, hex.
    pub device: String,
    /// This boot's random id (`/proc/sys/kernel/random/boot_id`).
    pub boot_id: String,
    /// The volume.
    pub luks_uuid: String,
    /// The sibling key slot.
    pub keyslot: u32,
}

/// Why no sibling released its half. Each is in the owner's history on the
/// sibling's side too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The owner denied the request.
    Denied,
    /// A sibling refused before asking the owner, with its reason.
    Refused(String),
    /// No sibling could be reached, or none answered in time.
    Unreachable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => f.write_str("the owner denied the unlock"),
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::Unreachable(reason) => write!(f, "no device answered: {reason}"),
        }
    }
}

/// The person's other devices, as early boot reaches them.
pub trait Siblings {
    /// Asks the token's siblings for the sibling half, signing each fresh
    /// challenge with the boot key.
    fn release(
        &self,
        token: &Token,
        boot_secret: &[u8; HALF_LEN],
        request: &Request,
    ) -> impl Future<Output = Result<Zeroizing<[u8; HALF_LEN]>, Refusal>> + Send;
}

/// Where the attempt reads and answers.
#[derive(Debug, Clone)]
pub struct Paths {
    /// systemd's password request directory.
    pub ask_dir: PathBuf,
    /// The kernel's boot id.
    pub boot_id: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            ask_dir: PathBuf::from(askpass::ASK_DIR),
            boot_id: PathBuf::from("/proc/sys/kernel/random/boot_id"),
        }
    }
}

/// How the attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A sibling's half opened the slot, and the prompt was answered.
    Unlocked,
    /// The volume was opened another way (a typed passphrase) first.
    OpenedElsewhere,
    /// No sibling released the half; the prompt stays for the person.
    Refused(Refusal),
}

/// Checks a derived passphrase against the slot. A seam for tests.
pub trait Volume {
    /// Whether `passphrase` opens the sibling slot.
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool;
    /// Whether the volume is open already.
    fn is_open(&self) -> bool;
}

/// Runs one attempt. `material` is the opened token material.
///
/// # Errors
/// When the boot id cannot be read; every other failure is an [`Outcome`].
pub async fn attempt(
    device: &Path,
    token: &Token,
    material: &Material,
    luks_uuid: &str,
    siblings: &impl Siblings,
    volume: &impl Volume,
    paths: &Paths,
    answer_within: Duration,
) -> Result<Outcome> {
    let keyslot = token.keyslot()?;
    let boot_id = std::fs::read_to_string(&paths.boot_id)
        .context("this boot has no boot id")?
        .trim()
        .to_owned();
    let request = Request {
        device: token.device.clone(),
        boot_id,
        luks_uuid: luks_uuid.to_owned(),
        keyslot,
    };
    let boot_secret = material.boot_secret()?;
    let sibling_half = match siblings.release(token, &boot_secret, &request).await {
        Ok(half) => half,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    drop(boot_secret);
    let passphrase =
        material::slot_passphrase(&*material.device_half()?, &sibling_half, luks_uuid, keyslot);
    drop(sibling_half);
    if !volume.opens(keyslot, &passphrase) {
        return Ok(Outcome::Refused(Refusal::Refused(
            "the released half does not open this volume's slot".into(),
        )));
    }
    let deadline = tokio::time::Instant::now() + answer_within;
    loop {
        if volume.is_open() {
            return Ok(Outcome::OpenedElsewhere);
        }
        if let Some(ask) = askpass::pending(&paths.ask_dir, device).first() {
            askpass::answer(ask, &passphrase)?;
            return Ok(Outcome::Unlocked);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(Outcome::Refused(Refusal::Unreachable(
                "the passphrase prompt never asked".into(),
            )));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{Sealing, Sibling, TOKEN_TYPE, TOKEN_VERSION};
    use std::os::unix::net::UnixDatagram;
    use std::sync::Mutex;

    struct FakeSiblings {
        answer: Result<[u8; HALF_LEN], Refusal>,
        seen: Mutex<Vec<Request>>,
    }

    impl Siblings for FakeSiblings {
        async fn release(
            &self,
            _token: &Token,
            boot_secret: &[u8; HALF_LEN],
            request: &Request,
        ) -> Result<Zeroizing<[u8; HALF_LEN]>, Refusal> {
            assert_eq!(boot_secret, &[0x22; HALF_LEN]);
            self.seen.lock().unwrap().push(request.clone());
            self.answer.clone().map(Zeroizing::new)
        }
    }

    struct FakeVolume(String);

    impl Volume for FakeVolume {
        fn opens(&self, keyslot: u32, passphrase: &str) -> bool {
            keyslot == 5 && passphrase == self.0
        }
        fn is_open(&self) -> bool {
            false
        }
    }

    fn token() -> Token {
        Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec!["5".into()],
            tilo_version: TOKEN_VERSION,
            device: "dd".repeat(32),
            boot_public: "ee".repeat(32),
            siblings: vec![Sibling {
                device: "aa".repeat(32),
                addrs: vec![],
                relay: None,
            }],
            sealing: Sealing::Cleartext,
            material: String::new(),
        }
    }

    fn material() -> Material {
        Material {
            device_half: "11".repeat(HALF_LEN),
            boot_secret: "22".repeat(HALF_LEN),
            networks: vec![],
        }
    }

    const UUID: &str = "0f0f0f0f-1111-2222-3333-444455556666";

    fn setup() -> (tempfile::TempDir, Paths, UnixDatagram) {
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
        let boot_id = dir.path().join("boot_id");
        std::fs::write(&boot_id, "5b1c\n").unwrap();
        (dir, Paths { ask_dir, boot_id }, receiver)
    }

    #[tokio::test]
    async fn a_released_half_answers_the_prompt() {
        let (_dir, paths, receiver) = setup();
        let expected = material::slot_passphrase(&[0x11; HALF_LEN], &[0x33; HALF_LEN], UUID, 5);
        let siblings = FakeSiblings {
            answer: Ok([0x33; HALF_LEN]),
            seen: Mutex::default(),
        };
        let outcome = attempt(
            Path::new("/dev/vda3"),
            &token(),
            &material(),
            UUID,
            &siblings,
            &FakeVolume(expected.to_string()),
            &paths,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Unlocked);
        let request = siblings.seen.lock().unwrap()[0].clone();
        assert_eq!((request.boot_id.as_str(), request.keyslot), ("5b1c", 5));
        let mut buffer = [0_u8; 80];
        let n = receiver.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], format!("+{}", *expected).as_bytes());
    }

    #[tokio::test]
    async fn a_refusal_or_a_wrong_half_leaves_the_prompt_alone() {
        let (_dir, paths, receiver) = setup();
        receiver.set_nonblocking(true).unwrap();
        let denied = FakeSiblings {
            answer: Err(Refusal::Denied),
            seen: Mutex::default(),
        };
        let volume = FakeVolume("never".into());
        let outcome = attempt(
            Path::new("/dev/vda3"),
            &token(),
            &material(),
            UUID,
            &denied,
            &volume,
            &paths,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Refused(Refusal::Denied));

        let wrong = FakeSiblings {
            answer: Ok([0x44; HALF_LEN]),
            seen: Mutex::default(),
        };
        let outcome = attempt(
            Path::new("/dev/vda3"),
            &token(),
            &material(),
            UUID,
            &wrong,
            &volume,
            &paths,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, Outcome::Refused(Refusal::Refused(_))),
            "{outcome:?}"
        );
        assert!(
            receiver.recv(&mut [0_u8; 8]).is_err(),
            "the prompt was answered"
        );
    }
}
