//! What a Bluetooth address is, written as people and `BlueZ` write one: six hex octets separated by
//! colons, `AA:BB:CC:DD:EE:FF`, in either case.
//!
//! The record of a person's accessories refuses a move that names anything else, and the radio's
//! own actions refuse to reach a device by anything else, with this one check.

/// Whether `address` is a Bluetooth address: six colon-separated hex octets.
#[must_use]
pub fn valid_address(address: &str) -> bool {
    address.len() == 17
        && address.split(':').count() == 6
        && address
            .split(':')
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::valid_address;

    #[test]
    fn only_six_colon_separated_hex_octets_read_as_an_address() {
        assert!(valid_address("AA:BB:CC:DD:EE:FF"));
        assert!(valid_address("aa:bb:cc:dd:ee:0f"));
        for wrong in [
            "",
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:0G",
            "GG:BB:CC:DD:EE:FF",
            "AABBCCDDEE01",
            "AA:BB:CC:DD:EE:011",
            "AA:BB:CC:DD:EE:FF:",
            "/org/bluez/hci0/dev_AA",
        ] {
            assert!(!valid_address(wrong), "{wrong}");
        }
    }
}
