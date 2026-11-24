//! Adding a computer to a person's devices with a ticket.
//!
//! A device of the person (a computer whose `tilo` holds their root, their desktop, a server)
//! issues a [`Ticket`]: its own iroh endpoint id, where it is reached (a relay and its direct
//! addresses), and a one-time secret. The person hands the ticket to the new computer, which
//! dials the issuing device directly over the device action service (`tilo-actions/1`) and calls
//! one action there, [`CERTIFY`], with the secret and its own device public key
//! ([`CertifyInput`]). The issuing device admits a connection from a device with no certificate
//! for that one call alone, and only with a live, unspent secret ([`Secret`]). Once the person
//! approves (the issuing device asks them, or the person runs the command that issued the ticket
//! on a computer holding their root), the person's root signs a member [`DeviceCert`] over exactly
//! that key, and the answer ([`CertifyAnswer`]) carries it with what the computer needs to enter:
//! the workspace and the certificate and address of the device it enters through ([`Entry`]).
//!
//! The device serving the ticket need not be the one that approves. It hands the question
//! ([`Asked`]: the computer's key, label and agent, and the hash of the ticket's secret) to an
//! [`Approver`]; the approver signs [`approval_digest`] with its own device key and has the
//! person's root sign the member certificate the serving device built ([`Approved`]). The serving
//! device checks both against the approver's certificate ([`check_approval`]) before it answers
//! the computer.
//!
//! The ticket is one string, however long, since the prompt the person copies carries it whole.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// The provider the certify action is published under on a device's action service.
pub const PROVIDER: &str = "tilo.device";
/// The resource the certify action acts on: the person's devices.
pub const RESOURCE: &str = "devices";
/// The action a computer with no certificate yet calls on the device that issued its ticket.
pub const CERTIFY: &str = "certify";
/// The action a daemon answers on its own workspace: serve a ticket for a new computer.
pub const ISSUE: &str = "issue";
/// The action a daemon answers on its own workspace: log in with a ticket another device issued.
pub const LOG_IN: &str = "logIn";
/// The action a daemon answers while no person's certificate names its device: start a new
/// person, whose root this device keeps and whose certificate names it. Asked by the device's own
/// account alone: a laptop's first `tilo` command, a phone's "start new".
pub const FOUND: &str = "found";
/// What [`FOUND`] is refused with on a device a person's certificate names already.
pub const ALREADY_A_PERSONS: &str = "this device is a person's already";
/// The action a daemon answers on its own workspace: what one of the person's devices is called
/// in every list.
pub const RENAME: &str = "rename";
/// The action a daemon that holds the person's root answers: end one of their devices' logins.
pub const REVOKE: &str = "revoke";
/// The action a device answers for itself: update its apps and system. Asked of a daemon naming
/// another of the person's devices, it is carried to that device, which answers it.
pub const UPDATE: &str = "update";
/// The action a device of the person's that holds their root calls on the host that serves a
/// sign-in ticket: the tab waiting for them.
pub const PENDING: &str = "pending";
/// The action a device of the person's that holds their root calls on the host that serves a
/// sign-in ticket: hand the host the root-signed certificate of the tab that is waiting.
pub const APPROVE: &str = "approve";
/// The action a certified computer calls on the host that served its ticket: what it did as it
/// logged in, step by step.
pub const REPORTED: &str = "reported";
/// The action a page calls on its host, in [`SCOPE`], for a ticket the host issued it with
/// [`ISSUE`]: what the computer did with it, and where its call stands.
pub const FOLLOW: &str = "follow";
/// The action a page calls on its host, in [`SCOPE`], for a ticket the host issued it with
/// [`ISSUE`]: its yes or no to the computer that asked with it.
pub const ANSWER: &str = "answer";
/// The scope a request names when it is that one call: no workspace, since the computer is in
/// none yet.
pub const SCOPE: &str = "tilo.new-device";
/// How long a ticket can be used, in nanoseconds.
pub const TICKET_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;
/// How long a planned machine's ticket can be used, in nanoseconds: the medium that carries it
/// may sit on a shelf a day before the machine installs from it.
pub const PLANNED_TICKET_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;
/// How many wrong secrets a ticket's issuer takes before it takes no more, so a ticket's
/// endpoint id alone gives a guesser nothing.
pub const MAX_WRONG_SECRETS: u32 = 10;
/// What every ticket starts with, so a person (or an agent) reading one knows what it is.
const PREFIX: &str = "tilo1";

/// Where a device is dialed over iroh: its endpoint id, a relay that carries it, and the
/// addresses it is reached at directly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    pub endpoint: [u8; 32],
    #[serde(default)]
    pub relay: Option<String>,
    #[serde(default)]
    pub direct: Vec<SocketAddr>,
}

/// What a ticket is for: a computer logging in as one of the person's devices, or a device
/// approving the tab that showed the ticket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// `tilo login <ticket>`: this computer becomes a device of the person.
    #[default]
    Login,
    /// `tilo device add <ticket>`: a device of the person signs the tab's certificate.
    SignIn,
}

/// What a new computer needs to be added to a person's devices: where the issuing device is
/// reached, what the ticket is for, and the one-time secret it takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub issuer: Reached,
    /// The direction the issuer serves the ticket in. Read explicitly, never guessed from the
    /// ticket's shape: `tilo login` and `tilo device add` each take their own.
    pub direction: Direction,
    pub secret: [u8; 32],
}

/// Why a string is not a ticket.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("this is not a Tilo ticket")]
pub struct NotATicket;

/// A tab asking to be signed in ([`Direction::SignIn`]): its device key, and the name it asks the
/// person's devices to list it by. The person's root signs it one of their member devices'
/// certificates, as it signs one for any device of theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    /// The tab's device public key, hex.
    pub device_public_key: String,
    /// The name it asks to be listed by, within what a certificate's label carries.
    pub label: String,
}

impl Tab {
    /// The tab's device key, hex as the certificate names it, checked to be one.
    ///
    /// # Errors
    /// The key is not 32 bytes hex.
    pub fn key(&self) -> Result<[u8; 32], NotATab> {
        hex::decode(&self.device_public_key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(NotATab)
    }

    /// The name the tab is listed by, within what a certificate's label carries.
    #[must_use]
    pub fn fitted_label(&self) -> String {
        let label = self.label.trim();
        let room = tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
        let end = (0..=room.min(label.len()))
            .rev()
            .find(|end| label.is_char_boundary(*end))
            .unwrap_or(0);
        label.get(..end).unwrap_or_default().to_owned()
    }

    /// The certificate the person's root signs for this tab, as it signs one for any device of
    /// theirs: a member of the person's, for a browser's lifetime
    /// ([`tilo_user_identity::CertificationPolicy::browser`]), issued now. Sign it with the root
    /// ([`tilo_user_identity::sign_device_cert`]); it names no issuer, since the person's own
    /// root signed it.
    ///
    /// # Errors
    /// The key is not 32 bytes hex, or the label is not one a certificate carries.
    pub fn certificate(
        &self,
        user_public_key: [u8; 32],
        issued_at_ns: u64,
    ) -> Result<DeviceCert, TabCertError> {
        let key = self.key()?;
        let label = self.fitted_label();
        let label = tilo_user_identity::display_name(&label)?;
        let policy = tilo_user_identity::CertificationPolicy::browser();
        Ok(DeviceCert {
            user_public_key,
            device_public_key: key,
            device_label: label.to_owned(),
            capabilities: policy.ceiling.clone(),
            issued_at_ns,
            expires_at_ns: Some(issued_at_ns.saturating_add(policy.lifetime_ns)),
            signature: [0; 64],
            issuer: None,
        })
    }
}

/// Why a tab cannot be certified as it asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TabCertError {
    /// Its device key is not one.
    #[error("this is not a tab asking to be signed in")]
    Key(#[from] NotATab),
    /// Its label is not one a certificate carries.
    #[error("the tab's name is not one a device may be listed by")]
    Label(#[from] tilo_user_identity::DisplayNameError),
}

/// Why a string is not a tab's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("this is not a tab asking to be signed in")]
pub struct NotATab;

/// What the computer that holds the person's root is told of the tab a sign-in ticket waits for
/// ([`PENDING`]): its key, the name it asks to be listed by, and what certifying it makes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningIn {
    /// The tab's device public key, hex.
    pub device: String,
    /// The name it asks to be listed by.
    pub label: String,
    /// What the certificate makes it, as the approval prompt says it.
    pub capability: String,
}

/// What a tab is certified as: a member of the person's, for a browser's lifetime
/// ([`tilo_user_identity::CertificationPolicy::browser`]).
#[must_use]
pub const fn tab_capability() -> &'static str {
    "a browser of yours, for 30 days"
}

/// What a computer reports as it logs in with a ticket, the tab that showed it shows each
/// ([`REPORTED`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// The ticket was taken: the agent has the `tilo` command.
    Resolved,
    /// The computer is a device of the person now.
    LoggedIn,
    /// The agent has Tilo's tools (its MCP server is registered).
    ToolsAdded,
    /// The agent's own check found Tilo's server where it keeps its servers.
    Check,
    /// The agent must restart before its tools load.
    Restart,
    /// The agent entered the person's workspace.
    Entered,
    /// The tab that asked to be signed in was approved.
    Approved,
    /// Something failed; the report's `detail` says what.
    Failed,
}

/// A computer reporting one step ([`REPORTED`]): the ticket it logged in with, what it did, the
/// agent running it, and, for [`Step::Failed`], what went wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The ticket, as the prompt carried it.
    pub ticket: String,
    pub step: Step,
    /// The agent running the computer, as `tilo login` found it (`Claude Code`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// One sentence for the person, for [`Step::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Ticket {
    /// A ticket for the device reached at `issuer`, with a fresh secret.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn new(issuer: Reached) -> Self {
        Self::in_direction(Direction::Login, issuer)
    }

    /// A ticket `direction` is served in, for the device reached at `issuer`, with a fresh
    /// secret.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn in_direction(direction: Direction, issuer: Reached) -> Self {
        use getrandom::rand_core::Rng as _;
        let mut secret = [0; 32];
        getrandom::rand_core::UnwrapErr(getrandom::SysRng).fill_bytes(&mut secret);
        Self {
            issuer,
            direction,
            secret,
        }
    }

    /// The ticket as the one string a person hands on.
    ///
    /// # Panics
    /// Never: a ticket is plain data postcard always encodes.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "a ticket is plain bytes and strings, which postcard always encodes"
    )]
    pub fn encode(&self) -> String {
        let bytes = postcard::to_allocvec(self).expect("a ticket encodes");
        format!("{PREFIX}{}", hex::encode(bytes))
    }

    /// The ticket `text` spells, as [`Self::encode`] wrote it (spaces around it ignored).
    ///
    /// # Errors
    /// [`NotATicket`] when it is not one.
    pub fn parse(text: &str) -> Result<Self, NotATicket> {
        let bytes = text
            .trim()
            .strip_prefix(PREFIX)
            .and_then(|rest| hex::decode(rest).ok())
            .ok_or(NotATicket)?;
        postcard::from_bytes(&bytes).map_err(|_| NotATicket)
    }

    /// Whether `text` looks like a ticket rather than another thing `tilo login` takes.
    #[must_use]
    pub fn looks_like(text: &str) -> bool {
        text.trim().starts_with(PREFIX)
    }

    /// The secret as [`CertifyInput::secret`] carries it.
    #[must_use]
    pub fn secret_hex(&self) -> String {
        hex::encode(self.secret)
    }
}

/// What a new computer sends with [`CERTIFY`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifyInput {
    /// The ticket's secret, hex.
    pub secret: String,
    /// The computer's device public key, hex: the key the connection comes from.
    pub device_public_key: String,
    /// The agent that runs on the computer, by the name the person's workspace shows it under.
    pub agent: String,
    /// The name the computer asks to be listed by among the person's devices
    /// (`Claude Code on build-box`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether the computer runs Tilo OS: the root certifies it native, else foreign, whose
    /// actions beyond content its person's own device approves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tilo_os: bool,
}

/// Where a computer the issuing device certified enters: the workspace, and the device it enters
/// through (the issuing device itself when it holds the workspace, else the server it uses).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// The certificate of the device the computer enters through.
    pub server_cert: DeviceCert,
    /// Where that device is dialed.
    pub server: Reached,
    /// The site and origin the person's login names, kept with the computer's login.
    pub site: String,
    pub host_origin: String,
}

/// What the issuing device answers [`CERTIFY`] with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifyAnswer {
    /// The member certificate the person's root signed over the computer's key.
    pub cert: DeviceCert,
    /// Where the computer enters.
    pub enter: Entry,
}

/// What the approval of a computer is signed under.
pub const APPROVAL_DOMAIN: &[u8] = b"tilo-device-approval-v1\0";
/// What a ticket's secret is hashed under before an approver sees it.
const TICKET_DOMAIN: &[u8] = b"tilo-device-ticket-v1\0";

/// The hash of a ticket's secret an approval names: which ticket the computer brought, without
/// the secret leaving the device that serves it.
#[must_use]
#[expect(clippy::expect_used, reason = "32 bytes always encode canonically")]
pub fn ticket_hash(secret: &[u8; 32]) -> [u8; 32] {
    tilo_user_identity::canonical::signing_digest(TICKET_DOMAIN, secret).expect("32 bytes encode")
}

/// A computer asking to be one of the person's devices, as the device serving its ticket hands
/// it to the approver ([`Approver`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asked {
    /// The person's root the computer asks to be certified by.
    pub user_public_key: [u8; 32],
    /// The computer's device public key.
    pub device_public_key: [u8; 32],
    /// The name it asks to be listed by.
    pub label: String,
    /// The agent that runs on it.
    pub agent: String,
    /// [`ticket_hash`] of the ticket it brought.
    pub ticket: [u8; 32],
    /// The member certificate the serving device built over its key, unsigned: what the root
    /// signs for it.
    pub cert: DeviceCert,
    /// [`approval_digest`] of all this: what the approver signs with its own device key.
    pub digest: [u8; 32],
}

/// An approver's yes ([`Approver::approve`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approved {
    /// [`Asked::cert`], signed by the person's root.
    pub cert: DeviceCert,
    /// The approving device's own certificate, signed by the person's root.
    pub approver: DeviceCert,
    /// The approving device's signature over [`Asked::digest`].
    #[serde(with = "serde_signature")]
    pub signature: [u8; 64],
}

mod serde_signature {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        signature: &[u8; 64],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(signature))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(deserializer)?;
        let mut signature = [0; 64];
        hex::decode_to_slice(&text, &mut signature)
            .map_err(|_| serde::de::Error::custom("a signature is 64 bytes hex"))?;
        Ok(signature)
    }
}

/// An approver's no: the sentence the computer is told.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Declined(pub String);

/// What an approver's answer arrives as.
pub type Approval =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Approved, Declined>> + Send>>;

/// Who approves a computer for the device serving its ticket: the person, through one of their
/// devices that holds their root. A computer whose `tilo` holds the root approves the ticket it
/// issued itself; a device without the root hands the question to one that has it (the person's
/// page).
pub trait Approver: Send + Sync {
    /// Approve or decline `asked`.
    fn approve(&self, asked: Asked) -> Approval;
}

/// The digest an approver signs for a computer: the person, the computer's key, its label and
/// agent, and the ticket it brought, under [`APPROVAL_DOMAIN`].
///
/// # Errors
/// The fields do not encode, which plain bytes and strings always do.
pub fn approval_digest(
    user_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
    label: &str,
    agent: &str,
    capability: &tilo_user_identity::DeviceCapabilities,
    ticket: &[u8; 32],
) -> Result<[u8; 32], tilo_user_identity::canonical::CanonicalError> {
    #[derive(Serialize)]
    struct View<'a> {
        user_public_key: &'a [u8; 32],
        device_public_key: &'a [u8; 32],
        label: &'a str,
        agent: &'a str,
        capability: &'a tilo_user_identity::DeviceCapabilities,
        ticket: &'a [u8; 32],
    }
    tilo_user_identity::canonical::signing_digest(
        APPROVAL_DOMAIN,
        &View {
            user_public_key,
            device_public_key,
            label,
            agent,
            capability,
            ticket,
        },
    )
}

/// What the serving device asks the approver for a computer with `device_public_key`, named
/// `label`, running `agent`, which brought the ticket whose secret is `secret`, to be a member
/// device of `user_public_key` from `now_ns`.
///
/// # Errors
/// The question does not encode, which plain bytes and strings always do.
pub fn asked(
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    label: &str,
    agent: &str,
    capability: tilo_user_identity::DeviceCapabilities,
    secret: &[u8; 32],
    now_ns: u64,
) -> Result<Asked, tilo_user_identity::canonical::CanonicalError> {
    let ticket = ticket_hash(secret);
    let digest = approval_digest(
        &user_public_key,
        &device_public_key,
        label,
        agent,
        &capability,
        &ticket,
    )?;
    Ok(Asked {
        user_public_key,
        device_public_key,
        label: label.to_owned(),
        agent: agent.to_owned(),
        ticket,
        cert: DeviceCert {
            user_public_key,
            device_public_key,
            device_label: label.to_owned(),
            capabilities: capability,
            // A second back, so a computer whose clock is a little behind takes it at once.
            issued_at_ns: now_ns.saturating_sub(1_000_000_000),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        },
        digest,
    })
}

/// Why an approval does not answer what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotApproved {
    #[error("the approving device is no live device of the person's own")]
    Approver,
    #[error("the approving device is revoked")]
    Revoked,
    #[error("the approval is not the approving device's signature over what was asked")]
    Signature,
    #[error("the certificate is not the person's root's over exactly what was asked")]
    Certificate,
}

/// The certificate `approved` gives for `asked`, checked: the approver is a live, unrevoked
/// device the person's root certified (no agent's key, no device it issued), it signed exactly
/// `asked`'s digest, and the certificate is the root's over exactly the asked key and label with
/// member capabilities and nothing more.
///
/// # Errors
/// What does not hold.
pub fn check_approval(
    asked: &Asked,
    approved: &Approved,
    revocations: &tilo_user_identity::RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, NotApproved> {
    let by = &approved.approver;
    if by.user_public_key != asked.user_public_key
        || !by.is_root_signed_device()
        || tilo_user_identity::verify_device_cert_at(by, now_ns).is_err()
    {
        return Err(NotApproved::Approver);
    }
    if revocations.refuses(by) {
        return Err(NotApproved::Revoked);
    }
    ed25519_dalek::VerifyingKey::from_bytes(&by.device_public_key)
        .and_then(|key| {
            key.verify_strict(
                &asked.digest,
                &ed25519_dalek::Signature::from_bytes(&approved.signature),
            )
        })
        .map_err(|_| NotApproved::Signature)?;
    let mut unsigned = approved.cert.clone();
    unsigned.signature = [0; 64];
    if unsigned != asked.cert
        || tilo_user_identity::verify_device_cert_at(&approved.cert, now_ns).is_err()
    {
        return Err(NotApproved::Certificate);
    }
    Ok(approved.cert.clone())
}

/// Why a ticket's secret was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("this ticket was used already: ask for a new one")]
    Spent,
    #[error("this ticket ran out: ask for a new one")]
    Expired,
    #[error("this is not the ticket's secret")]
    Wrong,
}

/// The one-time secret of a ticket its issuer serves: spent by the first computer that brings it,
/// while it is live.
#[derive(Debug)]
pub struct Secret {
    value: [u8; 32],
    expires_at_ns: u64,
    state: std::sync::Mutex<SecretState>,
}

#[derive(Debug, Default)]
struct SecretState {
    spent: bool,
    wrong: u32,
}

impl Secret {
    /// `ticket`'s secret, issued at `now_ns`, live for `lifetime_ns`.
    #[must_use]
    pub fn of(ticket: &Ticket, now_ns: u64, lifetime_ns: u64) -> Self {
        Self {
            value: ticket.secret,
            expires_at_ns: now_ns.saturating_add(lifetime_ns),
            state: std::sync::Mutex::default(),
        }
    }

    /// Whether `offered` (hex) is this secret, compared in constant time and without spending
    /// it: which of several served tickets a call is for.
    #[must_use]
    pub fn is(&self, offered: &str) -> bool {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(offered.trim(), &mut bytes).is_ok()
            && bytes
                .iter()
                .zip(self.value.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    /// Spend the secret for `offered` (hex) at `now_ns`: once, while it is live. A ticket that
    /// was offered too many wrong secrets is spent as well, so nothing guesses it.
    ///
    /// # Errors
    /// Why `offered` does not spend it.
    pub fn spend(&self, offered: &str, now_ns: u64) -> Result<(), Refused> {
        let mut bytes = [0u8; 32];
        let decoded = hex::decode_to_slice(offered.trim(), &mut bytes).is_ok();
        // Compared in constant time: how long a refusal takes says nothing of the secret.
        let differs = bytes
            .iter()
            .zip(self.value.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let spent = if state.spent {
            Err(Refused::Spent)
        } else if now_ns >= self.expires_at_ns {
            Err(Refused::Expired)
        } else if !decoded || differs != 0 {
            state.wrong = state.wrong.saturating_add(1);
            state.spent = state.wrong >= MAX_WRONG_SECRETS;
            Err(Refused::Wrong)
        } else {
            state.spent = true;
            Ok(())
        };
        drop(state);
        spent
    }

    /// Whether the secret is still waiting for its computer at `now_ns`.
    #[must_use]
    pub fn waiting(&self, now_ns: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.spent && now_ns < self.expires_at_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> Ticket {
        Ticket::new(Reached {
            endpoint: [7; 32],
            relay: Some("https://relay.example./".into()),
            direct: vec!["192.0.2.1:4433".parse().unwrap()],
        })
    }

    #[test]
    fn a_ticket_reads_back_as_the_one_string_it_is_handed_on_as() {
        let ticket = ticket();
        let text = ticket.encode();
        assert!(Ticket::looks_like(&text));
        assert_eq!(Ticket::parse(&format!("  {text}\n")), Ok(ticket));
        assert_eq!(Ticket::parse("K7Q-MX2P"), Err(NotATicket));
        assert_eq!(
            Ticket::parse(text.get(..text.len() - 4).unwrap()),
            Err(NotATicket)
        );
    }

    #[test]
    fn a_secret_is_spent_once_while_it_is_live_and_never_guessed() {
        let ticket = ticket();
        let secret = Secret::of(&ticket, 0, TICKET_LIFETIME_NS);
        assert_eq!(secret.spend(&hex::encode([1; 32]), 1), Err(Refused::Wrong));
        assert_eq!(secret.spend("not hex", 1), Err(Refused::Wrong));
        assert!(secret.waiting(1));
        assert!(secret.is(&ticket.secret_hex()) && !secret.is(&hex::encode([1; 32])));
        assert_eq!(secret.spend(&ticket.secret_hex(), 2), Ok(()));
        assert_eq!(secret.spend(&ticket.secret_hex(), 3), Err(Refused::Spent));

        let late = Secret::of(&ticket, 0, TICKET_LIFETIME_NS);
        assert_eq!(
            late.spend(&ticket.secret_hex(), TICKET_LIFETIME_NS),
            Err(Refused::Expired)
        );

        let guessed = Secret::of(&ticket, 0, TICKET_LIFETIME_NS);
        for _ in 0..MAX_WRONG_SECRETS {
            assert_eq!(guessed.spend(&hex::encode([2; 32]), 1), Err(Refused::Wrong));
        }
        assert_eq!(
            guessed.spend(&ticket.secret_hex(), 1),
            Err(Refused::Spent),
            "too many wrong secrets spend the ticket"
        );
    }

    /// A person's root, one of their live devices it certified, and the question a
    /// computer brings: everything an approving device answers, before it is altered.
    #[test]
    fn an_approval_is_the_persons_live_device_signing_over_exactly_what_was_asked() {
        use ed25519_dalek::{Signer as _, SigningKey};
        use tilo_user_identity::{
            AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList,
            RevocationReason, sign_device_cert, sign_revocation_entry,
        };

        const NOW: u64 = 1_000_000_000_000;
        let root = SigningKey::from_bytes(&[1; 32]);
        let user = root.verifying_key().to_bytes();
        let approver_key = SigningKey::from_bytes(&[2; 32]);
        let certified = |key: &SigningKey| {
            let mut cert = DeviceCert {
                user_public_key: user,
                device_public_key: key.verifying_key().to_bytes(),
                device_label: "The laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: NOW - 60_000_000_000,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, &root).unwrap();
            cert
        };
        let approver = certified(&approver_key);
        let asked = asked(
            user,
            [9; 32],
            "The phone",
            "",
            DeviceCapabilities::member(),
            &[3; 32],
            NOW,
        )
        .unwrap();
        let signed = |mut cert: DeviceCert| {
            sign_device_cert(&mut cert, &root).unwrap();
            cert
        };
        let answered = |cert: DeviceCert, approver: DeviceCert| Approved {
            signature: approver_key.sign(&asked.digest).to_bytes(),
            cert,
            approver,
        };
        let cert = signed(asked.cert.clone());
        let answer = answered(cert.clone(), approver.clone());
        let live = RevocationList::new();

        // The approving device is the person's live, root-signed device, it signed the
        // question, and the certificate is the root's over exactly what was asked.
        assert_eq!(
            check_approval(&asked, &answer, &live, NOW),
            Ok(cert.clone())
        );

        // Revoked: the same signature admits nothing once the person revoked the approver.
        let mut revocation = RevocationEntry {
            user_public_key: user,
            revoked_device_public_key: approver.device_public_key,
            revoked_at_ns: NOW,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        let mut revocations = RevocationList::new();
        revocations.insert(revocation).unwrap();
        assert_eq!(
            check_approval(&asked, &answer, &revocations, NOW),
            Err(NotApproved::Revoked)
        );

        // Another person's device, and an agent's key this person's root issued: neither
        // is one of the person's own devices.
        let stranger = SigningKey::from_bytes(&[4; 32]);
        let mut other_users = certified(&approver_key);
        let other_root = SigningKey::from_bytes(&[5; 32]);
        other_users.user_public_key = other_root.verifying_key().to_bytes();
        sign_device_cert(&mut other_users, &other_root).unwrap();
        assert_eq!(
            check_approval(&asked, &answered(cert.clone(), other_users), &live, NOW),
            Err(NotApproved::Approver)
        );
        let mut agent = certified(&stranger);
        agent.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
        agent.expires_at_ns = Some(NOW + 60 * 60 * 1_000_000_000);
        sign_device_cert(&mut agent, &root).unwrap();
        assert_eq!(
            check_approval(&asked, &answered(cert, agent), &live, NOW),
            Err(NotApproved::Approver)
        );

        // A signature by a key that is not the approver's.
        let mut forged = answer;
        forged.signature = stranger.sign(&asked.digest).to_bytes();
        assert_eq!(
            check_approval(&asked, &forged, &live, NOW),
            Err(NotApproved::Signature)
        );

        // The root's certificate over another device than the one asked for.
        let altered = signed(DeviceCert {
            device_label: "The impostor".into(),
            ..asked.cert.clone()
        });
        assert_eq!(
            check_approval(&asked, &answered(altered, approver), &live, NOW),
            Err(NotApproved::Certificate)
        );
    }
}
