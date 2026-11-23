//! The code a new device shows, which the person compares on the device that
//! approves it before the new device joins.

const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";

/// The six-digit code a pairing device shows: the same on the new device,
/// the device that found it and every device asked to approve it.
#[must_use]
pub fn pairing_code(device_public_key: [u8; 32], server_node_id: [u8; 32]) -> String {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(device_public_key);
    input.extend(server_node_id);
    let digest = blake3::hash(&input);
    let [first, second, third, fourth, ..] = *digest.as_bytes();
    let number = u32::from_le_bytes([first, second, third, fourth]);
    format!("{:06}", number % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::pairing_code;

    #[test]
    fn every_screen_shows_the_same_six_digits() {
        let code = pairing_code([1; 32], [2; 32]);
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(code, pairing_code([1; 32], [2; 32]));
        assert_ne!(code, pairing_code([1; 32], [3; 32]));
    }
}
