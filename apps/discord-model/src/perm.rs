//! Discord permission bits the bridge sets (discord-bridge.md §5, "Roles").

pub const KICK_MEMBERS: u64 = 1 << 1;
pub const BAN_MEMBERS: u64 = 1 << 2;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
pub const ADD_REACTIONS: u64 = 1 << 6;
pub const VIEW_AUDIT_LOG: u64 = 1 << 7;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const ATTACH_FILES: u64 = 1 << 15;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
pub const CONNECT: u64 = 1 << 20;
pub const SPEAK: u64 = 1 << 21;
pub const MANAGE_ROLES: u64 = 1 << 28;
pub const MANAGE_WEBHOOKS: u64 = 1 << 29;
pub const USE_APPLICATION_COMMANDS: u64 = 1 << 31;
pub const SEND_MESSAGES_IN_THREADS: u64 = 1 << 38;
pub const MODERATE_MEMBERS: u64 = 1 << 40;

/// `perms` as Discord writes permission sets: a decimal string.
#[must_use]
pub fn bits(perms: u64) -> String {
    perms.to_string()
}

/// A decimal permission string, `0` when it is not one.
#[must_use]
pub fn parse(bits: &str) -> u64 {
    bits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_as_decimal_strings() {
        let everyone = VIEW_CHANNEL | SEND_MESSAGES;
        assert_eq!(bits(everyone), "3072");
        assert_eq!(parse("3072"), everyone);
        assert_eq!(parse("not a number"), 0);
        assert_eq!(MODERATE_MEMBERS, 1 << 40);
    }
}
