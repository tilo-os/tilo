//! Recovery keys as `systemd-cryptenroll --recovery-key` prints them: 32 bytes
//! in "modhex", eight groups of eight characters joined by dashes. The same
//! string is the passphrase of the recovery keyslot the installer enrolls.

use zeroize::Zeroizing;

const MODHEX: &[u8; 16] = b"cbdefghijklnrtuv";
pub const KEY_LEN: usize = 32;

pub struct RecoveryKey(Zeroizing<[u8; KEY_LEN]>);

impl RecoveryKey {
    /// Parse what a person typed: case, dashes and spaces are forgiven.
    pub fn parse(typed: &str) -> Option<Self> {
        let mut bytes = Zeroizing::new([0_u8; KEY_LEN]);
        let mut nibbles = typed
            .chars()
            .filter(|c| !matches!(c, '-' | ' ' | '\t' | '\n'))
            .map(|c| {
                let lower = u8::try_from(c.to_ascii_lowercase()).ok()?;
                MODHEX
                    .iter()
                    .position(|&m| m == lower)
                    .and_then(|i| u8::try_from(i).ok())
            });
        for byte in bytes.iter_mut() {
            let high = nibbles.next()??;
            let low = nibbles.next()??;
            *byte = (high << 4) | low;
        }
        if nibbles.next().is_some() {
            return None;
        }
        Some(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The canonical text, which is also the recovery keyslot's passphrase.
    pub fn formatted(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(KEY_LEN * 2 + 7));
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 && i % 4 == 0 {
                out.push('-');
            }
            for nibble in [byte >> 4, byte & 0x0f] {
                if let Some(&c) = MODHEX.get(usize::from(nibble)) {
                    out.push(char::from(c));
                }
            }
        }
        out
    }
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_keys_round_trip_through_their_printed_form() {
        let mut bytes = [0_u8; KEY_LEN];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::try_from(i * 7 % 256).unwrap();
        }
        let key = RecoveryKey::from_bytes(bytes);
        let text = key.formatted();
        assert_eq!(text.len(), 71);
        assert_eq!(text.split('-').count(), 8);
        assert!(text.split('-').all(|group| group.len() == 8));
        assert_eq!(RecoveryKey::parse(&text).unwrap().expose(), &bytes);
        assert_eq!(
            RecoveryKey::parse(&text.to_uppercase().replace('-', " "))
                .unwrap()
                .expose(),
            &bytes
        );
    }

    #[test]
    fn anything_else_is_not_a_recovery_key() {
        assert!(RecoveryKey::parse("123456").is_none());
        assert!(RecoveryKey::parse("correct horse battery staple").is_none());
        let full = RecoveryKey::from_bytes([1; KEY_LEN]).formatted();
        assert!(RecoveryKey::parse(full.get(..full.len() - 1).unwrap()).is_none());
        assert!(RecoveryKey::parse(&format!("{}c", *full)).is_none());
    }

    #[test]
    fn the_first_byte_maps_to_systemds_alphabet() {
        let mut bytes = [0_u8; KEY_LEN];
        bytes[0] = 0x0f;
        assert!(RecoveryKey::from_bytes(bytes).formatted().starts_with("cv"));
    }
}
