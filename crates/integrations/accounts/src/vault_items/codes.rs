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
    let until = step(now, params.period).saturating_add(1).saturating_mul(params.period);
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

#[cfg(test)]
mod tests {
    use super::super::NewItem;
    use super::*;

    /// The seed RFC 6238 tests SHA-1 with, the bytes `12345678901234567890`,
    /// as the setup key a site would show for it.
    const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    /// The standard's SHA-256 seed: the same twenty bytes, repeated to the
    /// thirty-two bytes that hash takes.
    const SEED_256: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA";
    /// The standard's SHA-512 seed: the same twenty bytes, repeated to the
    /// sixty-four bytes that hash takes.
    const SEED_512: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA";

    /// The codes are the standard's own: RFC 6238's table, by its last six
    /// digits, each good for its whole period.
    #[test]
    fn a_seed_gives_the_code_the_standard_gives() {
        let params = CodeParams::default();
        for (at, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(code_of(SEED, at, params).unwrap().as_str(), code, "at {at}");
        }
        assert_eq!(
            code_of(SEED, 30, params),
            code_of(SEED, 59, params),
            "one period"
        );
        assert_ne!(
            code_of(SEED, 59, params),
            code_of(SEED, 60, params),
            "the next one"
        );
        // A last symbol that fills no whole byte adds nothing, and what is
        // no base32 gives no code.
        assert_eq!(
            code_of(&format!("{SEED}A"), 59, params),
            code_of(SEED, 59, params)
        );
        assert_eq!(code_of("not a seed", 59, params), None);
        assert_eq!(code_of("", 59, params), None);
        // RFC 6238's SHA-256 and SHA-512 rows, by their last six digits.
        let sha256 = CodeParams {
            algorithm: Algorithm::Sha256,
            ..CodeParams::default()
        };
        let sha512 = CodeParams {
            algorithm: Algorithm::Sha512,
            ..CodeParams::default()
        };
        let eight_256 = CodeParams {
            digits: 8,
            ..sha256
        };
        let eight_512 = CodeParams {
            digits: 8,
            ..sha512
        };
        for (at, sha256_code, sha512_code) in [
            (59, "46119246", "90693936"),
            (1_111_111_109, "68084774", "25091201"),
        ] {
            assert_eq!(
                &code_of(SEED_256, at, eight_256).unwrap()[..],
                sha256_code,
                "SHA-256 at {at}"
            );
            assert_eq!(
                &code_of(SEED_512, at, eight_512).unwrap()[..],
                sha512_code,
                "SHA-512 at {at}"
            );
        }
        // Six digits are the same code without its first two.
        assert_eq!(&code_of(SEED_256, 59, sha256).unwrap()[..], "119246");
        assert_eq!(&code_of(SEED_512, 59, sha512).unwrap()[..], "693936");
    }

    /// A code generator gives the code of this moment at its own site and
    /// nowhere else, each time written down, and its seed is still never
    /// handed out; what is no code generator gives no code.
    #[test]
    fn a_code_generator_gives_its_code_at_its_site_and_never_its_seed() {
        let site = Place::parse("https://example.test").unwrap();
        let mut items = VaultItems::default();
        let generator = items
            .add(NewItem::new(
                ItemKind::CodeGenerator,
                vec![site.clone()],
                String::new(),
                SEED.to_owned(),
            ))
            .unwrap();
        let login = items
            .add(NewItem::new(
                ItemKind::Login,
                vec![site.clone()],
                "ada@example.test".to_owned(),
                "correct horse battery staple".to_owned(),
            ))
            .unwrap();

        let before = unix_now();
        let code = items.code_at(&generator.id, &site, " Assistant ").unwrap();
        let after = unix_now();
        let params = CodeParams::default();
        let either = [code_of(SEED, before, params), code_of(SEED, after, params)];
        assert!(
            either
                .iter()
                .flatten()
                .any(|good| good.as_str() == code.digits()),
            "the code of the moment it was asked for"
        );
        assert_eq!(code.digits().len(), 6);
        assert!(code.digits().bytes().all(|digit| digit.is_ascii_digit()));
        assert!(code.until_unix > before && code.until_unix <= after.saturating_add(CODE_SECS));
        let printed = format!("{code:?}");
        assert!(!printed.contains(code.digits()), "{printed}");

        // Anywhere else, and for anything else, there is no code.
        for elsewhere in [
            "https://login.example.test",
            "http://example.test",
            "https://example.test:8443",
            "https://example.test.elsewhere.test",
        ] {
            let elsewhere = Place::parse(elsewhere).unwrap();
            assert_eq!(
                items.code_at(&generator.id, &elsewhere, "Assistant").err(),
                Some(ItemError::WrongPlace),
                "{elsewhere}"
            );
        }
        assert_eq!(
            items.code_at(&login.id, &site, "Assistant").err(),
            Some(ItemError::WrongKind)
        );
        let nobody = ItemId::parse(&"0".repeat(32)).unwrap();
        assert_eq!(
            items.code_at(&nobody, &site, "Assistant").err(),
            Some(ItemError::NotFound)
        );
        // The seed itself still leaves for nobody.
        assert_eq!(
            items.use_at(&generator.id, &site, "Assistant").err(),
            Some(ItemError::NeverReleased)
        );

        // The one code that was given is the one use written down, and what
        // was refused left nothing.
        let uses = items.uses(&generator.id).unwrap();
        assert_eq!(uses.len(), 1, "{uses:?}");
        let written = uses.first().unwrap();
        assert_eq!(written.places, vec![site.clone()]);
        assert_eq!(written.by, "Assistant");
        assert!(written.at_unix >= before && written.at_unix <= after);
        assert!(items.uses(&login.id).unwrap().is_empty());
        let facts = VaultItems::of_entries(items.kept()).facts();
        let kept = facts.iter().find(|item| item.id == generator.id).unwrap();
        assert_eq!(kept.last_used_unix, Some(written.at_unix));
    }
}
