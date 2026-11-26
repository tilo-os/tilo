//! The code of this moment from a code generator's seed.
//!
//! A code generator's seed is never handed out
//! ([`ItemError::NeverReleased`]). What a sign-in needs of it is the code a
//! site asks for as its second step: digits that stand for a span of seconds
//! (RFC 6238 over HMAC-SHA-1, SHA-256 or SHA-512, which is what the setup key
//! a site shows is for). The code is made here, inside the vault. A sign-in
//! fills it into the field that asks for it and the use is written down first,
//! as every use is; the Authenticator shows the code of the moment where the
//! vault is and writes nothing down ([`VaultItems::code_for`]): a use that
//! changes nothing is a read.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use tilo_clock::unix_now;
use zeroize::Zeroizing;

use super::{ItemError, ItemId, ItemKind, Place, VaultItems};

/// How long one code stands, in seconds, unless the item says otherwise.
pub const CODE_SECS: u64 = 30;
/// How many digits a code has, unless the item says otherwise.
pub const CODE_DIGITS: u8 = 6;
/// The most and fewest digits a code has.
pub const MOST_DIGITS: u8 = 10;
/// The fewest digits a code has.
pub const FEWEST_DIGITS: u8 = 4;
/// The most and fewest seconds a code stands.
pub const MOST_PERIOD: u64 = 300;
/// The fewest seconds a code stands.
pub const FEWEST_PERIOD: u64 = 5;

/// The hash a code is made with, as a service's setup key names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Algorithm {
    /// `SHA1`, which nearly every service uses.
    #[default]
    Sha1,
    /// `SHA256`.
    Sha256,
    /// `SHA512`.
    Sha512,
}

impl Algorithm {
    /// The word a setup link and the vault keep it as.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
        }
    }

    /// The hash `word` names, in any case, or `None` for a name that is no hash.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_uppercase().as_str() {
            "SHA1" | "SHA-1" => Some(Self::Sha1),
            "SHA256" | "SHA-256" => Some(Self::Sha256),
            "SHA512" | "SHA-512" => Some(Self::Sha512),
            _ => None,
        }
    }
}

/// How a code generator makes its code: the hash, how many digits, and how
/// long one code stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeParams {
    pub algorithm: Algorithm,
    pub digits: u8,
    pub period: u64,
}

impl Default for CodeParams {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::default(),
            digits: CODE_DIGITS,
            period: CODE_SECS,
        }
    }
}

impl CodeParams {
    /// The parameters a service's setup link named, refused where one is out
    /// of the range a code has.
    ///
    /// # Errors
    /// A hash no code is made with, or digits or a period outside their bounds.
    pub fn checked(algorithm: &str, digits: u8, period: u64) -> Result<Self, ItemError> {
        let algorithm = Algorithm::parse(algorithm).ok_or(ItemError::Invalid(
            "A code is made with SHA1, SHA256 or SHA512",
        ))?;
        if !(FEWEST_DIGITS..=MOST_DIGITS).contains(&digits) {
            return Err(ItemError::Invalid("A code has 4 to 10 digits"));
        }
        if !(FEWEST_PERIOD..=MOST_PERIOD).contains(&period) {
            return Err(ItemError::Invalid("A code changes every 5 to 300 seconds"));
        }
        Ok(Self {
            algorithm,
            digits,
            period,
        })
    }

    /// One more than the largest number that many digits write.
    fn modulus(self) -> u32 {
        10_u32
            .checked_pow(u32::from(self.digits))
            .unwrap_or(u32::MAX)
    }
}

/// A code for a second step, made from a code generator's seed. Its digits
/// are wiped from memory when it is dropped.
pub struct Code {
    digits: Zeroizing<String>,
    /// When it stops being good, in seconds since the Unix epoch.
    pub until_unix: u64,
    /// How the item makes its codes.
    pub params: CodeParams,
}

impl Code {
    /// The digits, for the code that fills them into the sign-in.
    #[must_use]
    pub fn digits(&self) -> &str {
        &self.digits
    }
}

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Code")
            .field("until_unix", &self.until_unix)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl VaultItems {
    /// The code of this moment from the code generator `id` names, for the
    /// sign-in at `place`, for `by`, as [`VaultItems::use_at`] takes it.
    ///
    /// Refused anywhere the item does not belong, and for an item that is no
    /// code generator. The use is written down before the code is handed
    /// out: a use that cannot be recorded does not happen. The seed stays
    /// where it is.
    pub fn code_at(&mut self, id: &ItemId, place: &Place, by: &str) -> Result<Code, ItemError> {
        let mut sealed = self.sealed(id)?;
        if !sealed.places.contains(place) {
            return Err(ItemError::WrongPlace);
        }
        if sealed.kind != ItemKind::CodeGenerator {
            return Err(ItemError::WrongKind);
        }
        let now = unix_now();
        let params = sealed.code_params();
        let code = code_of(&sealed.secret, now, params).ok_or(ItemError::Damaged)?;
        sealed.used(now, std::slice::from_ref(place), by);
        self.write_uses(id, &sealed);
        Ok(code_until(code, now, params))
    }

    /// The code of this moment from the code generator `id` names, for
    /// showing where the vault is: a read, so it writes nothing down and
    /// keeps no use. Its parameters are the item's own, as the service's
    /// setup key named them.
    ///
    /// # Errors
    /// No such item, an item that is no code generator, or a seed that does
    /// not read.
    pub fn code_for(&self, id: &ItemId) -> Result<Code, ItemError> {
        let sealed = self.sealed(id)?;
        if sealed.kind != ItemKind::CodeGenerator {
            return Err(ItemError::WrongKind);
        }
        let now = unix_now();
        let params = sealed.code_params();
        let code = code_of(&sealed.secret, now, params).ok_or(ItemError::Damaged)?;
        Ok(code_until(code, now, params))
    }

    /// How the code generator `id` names makes its codes.
    ///
    /// # Errors
    /// No such item, or an item that is no code generator.
    pub fn code_params(&self, id: &ItemId) -> Result<CodeParams, ItemError> {
        let sealed = self.sealed(id)?;
        if sealed.kind != ItemKind::CodeGenerator {
            return Err(ItemError::WrongKind);
        }
        Ok(sealed.code_params())
    }
}

/// The step of `period` seconds since the Unix epoch `unix_secs` is in.
fn step(unix_secs: u64, period: u64) -> u64 {
    unix_secs.checked_div(period).unwrap_or_default()
}

/// `code` as the item hands it out, with the instant it stops being good.
fn code_until(digits: Zeroizing<String>, now: u64, params: CodeParams) -> Code {
    let until = step(now, params.period)
        .saturating_add(1)
        .saturating_mul(params.period);
    Code {
        digits,
        until_unix: until,
        params,
    }
}

/// The code the seed `seed`, in base32 as the vault keeps it, gives at
/// `unix_secs` with `params`.
fn code_of(seed: &str, unix_secs: u64, params: CodeParams) -> Option<Zeroizing<String>> {
    let key = base32(seed)?;
    let counter = step(unix_secs, params.period).to_be_bytes();
    let digest = match params.algorithm {
        Algorithm::Sha1 => {
            let mut mac = <Hmac<Sha1>>::new_from_slice(&key).ok()?;
            mac.update(&counter);
            mac.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha256 => {
            let mut mac = <Hmac<Sha256>>::new_from_slice(&key).ok()?;
            mac.update(&counter);
            mac.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha512 => {
            let mut mac = <Hmac<Sha512>>::new_from_slice(&key).ok()?;
            mac.update(&counter);
            mac.finalize().into_bytes().to_vec()
        }
    };
    let digest = Zeroizing::new(digest);
    // RFC 4226, 5.3: the low four bits of the last byte choose four bytes,
    // and the top bit of those is dropped.
    let offset = usize::from(*digest.last()? & 0x0f);
    let chosen: [u8; 4] = digest
        .get(offset..offset.saturating_add(4))?
        .try_into()
        .ok()?;
    let value = (u32::from_be_bytes(chosen) & 0x7fff_ffff).checked_rem(params.modulus())?;
    Some(Zeroizing::new(format!(
        "{value:0width$}",
        width = usize::from(params.digits)
    )))
}

/// The bytes a base32 seed stands for (RFC 4648, unpadded). Bits left over
/// in its last symbol are dropped, as the apps that generate codes drop
/// them.
fn base32(seed: &str) -> Option<Zeroizing<Vec<u8>>> {
    // Sized for every byte at once, so no shorter copy is left behind.
    let mut bytes = Zeroizing::new(Vec::with_capacity(seed.len()));
    let (mut held, mut bits) = (0_u32, 0_u32);
    for symbol in seed.bytes() {
        let value = match symbol {
            b'A'..=b'Z' => symbol.wrapping_sub(b'A'),
            b'2'..=b'7' => symbol.wrapping_sub(b'2').wrapping_add(26),
            _ => return None,
        };
        // Five more bits behind those not yet written out, which are fewer
        // than eight.
        held = held.wrapping_shl(5) | u32::from(value);
        bits = bits.wrapping_add(5);
        if bits >= 8 {
            bits = bits.wrapping_sub(8);
            bytes.push(u8::try_from(held.wrapping_shr(bits) & 0xff).ok()?);
            held &= 1_u32.wrapping_shl(bits).wrapping_sub(1);
        }
    }
    (!bytes.is_empty()).then_some(bytes)
}
