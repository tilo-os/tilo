//! The code of this moment from a code generator's seed.
//!
//! A code generator's seed is never handed out
//! ([`ItemError::NeverReleased`]). What a sign-in needs of it is the code a
//! site asks for as its second step: six digits that stand for half a minute
//! (RFC 6238 over HMAC-SHA-1, which is what the setup key a site shows is
//! for). The code is made here, inside the vault, only for a place the item
//! names, and the use is written down first, as every use is. It is good for
//! one sign-in and goes where a password goes: into the field that asks for
//! it, never into an answer, a message or a log.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use tilo_clock::unix_now;
use zeroize::Zeroizing;

use super::{ItemError, ItemId, ItemKind, Place, VaultItems};

/// How long one code stands, in seconds.
pub const CODE_SECS: u64 = 30;
/// How many digits a code has.
const DIGITS: usize = 6;
/// One more than the largest number that many digits write.
const MODULUS: u32 = 1_000_000;

/// A code for a second step, made from a code generator's seed. Its digits
/// are wiped from memory when it is dropped.
pub struct Code {
    digits: Zeroizing<String>,
    /// When it stops being good, in seconds since the Unix epoch.
    pub until_unix: u64,
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
        let digits = code_of(&sealed.secret, now).ok_or(ItemError::Damaged)?;
        sealed.used(now, std::slice::from_ref(place), by);
        self.write_uses(id, &sealed);
        Ok(Code {
            digits,
            until_unix: half_minute(now).saturating_add(1).saturating_mul(CODE_SECS),
        })
    }
}

/// Which half minute since the Unix epoch `unix_secs` is in.
fn half_minute(unix_secs: u64) -> u64 {
    unix_secs.checked_div(CODE_SECS).unwrap_or_default()
}

/// The code the seed `seed`, in base32 as the vault keeps it, gives at
/// `unix_secs`.
fn code_of(seed: &str, unix_secs: u64) -> Option<Zeroizing<String>> {
    let key = base32(seed)?;
    let mut mac = <Hmac<Sha1>>::new_from_slice(&key).ok()?;
    mac.update(&half_minute(unix_secs).to_be_bytes());
    let digest = mac.finalize().into_bytes();
    // RFC 4226, 5.3: the low four bits of the last byte choose four bytes,
    // and the top bit of those is dropped.
    let offset = usize::from(*digest.last()? & 0x0f);
    let chosen: [u8; 4] = digest
        .get(offset..offset.saturating_add(4))?
        .try_into()
        .ok()?;
    let value = (u32::from_be_bytes(chosen) & 0x7fff_ffff).checked_rem(MODULUS)?;
    Some(Zeroizing::new(format!("{value:0DIGITS$}")))
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

    /// The seed RFC 6238 tests with, the bytes `12345678901234567890`, as
    /// the setup key a site would show for it.
    const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// The codes are the standard's own: RFC 6238's table for SHA-1, by its
    /// last six digits, each good for its whole half minute.
    #[test]
    fn a_seed_gives_the_code_the_standard_gives() {
        for (at, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(code_of(SEED, at).unwrap().as_str(), code, "at {at}");
        }
        assert_eq!(code_of(SEED, 30), code_of(SEED, 59), "one half minute");
        assert_ne!(code_of(SEED, 59), code_of(SEED, 60), "the next one");
        // A last symbol that fills no whole byte adds nothing, and what is
        // no base32 gives no code.
        assert_eq!(code_of(&format!("{SEED}A"), 59), code_of(SEED, 59));
        assert_eq!(code_of("not a seed", 59), None);
        assert_eq!(code_of("", 59), None);
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
        let either = [code_of(SEED, before), code_of(SEED, after)];
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
