//! The marks of the services a workspace may be bridged to, drawn where a person a bridge speaks for
//! appears: a small glyph in place of words, so "this came from Discord" reads at a glance without a
//! label beside every name. A mark is a filled icon of the set ([`Icon::is_brand`]).

use super::Icon;

/// The mark of `service` (its lowercase id, `discord`), if the set has one; a service without one is
/// named in words instead. Draw it with [`Icon::draw_filled`].
#[must_use]
pub fn service_mark(service: &str) -> Option<Icon> {
    match service {
        "discord" => Some(Icon::Discord),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_has_a_mark_and_others_have_none() {
        let mark = service_mark("discord").expect("a mark");
        assert!(mark.is_brand());
        assert_eq!(service_mark("slack"), None);
    }
}
