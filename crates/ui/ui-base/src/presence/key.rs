//! Presence keys are wire types and live in `tilo_presence`.

pub use tilo_presence::{DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange};

#[cfg(test)]
mod scope_digest_tests {
    use super::*;

    #[test]
    fn digest_form_is_stable_distinct_and_self_describing() {
        let key = |id: &str| PresenceKey::Widget {
            path: vec![
                PathSegment::Field("wp_block".into()),
                PathSegment::Field(id.into()),
            ],
        };
        let long = format!("{}.7", "ab".repeat(64));
        let a = key(&long);
        assert_eq!(a.scope_digest(), key(&long).scope_digest());
        assert_ne!(a.scope_digest(), key("other").scope_digest());
        // Known vector shared with the LIVE_V1 anchor tests (host + browser).
        assert_eq!(
            hex::encode(key("b1").scope_digest().unwrap()),
            SCOPE_DIGEST_B1
        );
        let PresenceKey::Widget { path } = a.digest_form().unwrap() else {
            panic!()
        };
        assert_eq!(path[0], PathSegment::Field(DIGEST_KEY_FIELD.into()));
        assert_eq!(
            a.digest_form(),
            Some(PresenceKey::from_scope_digest(a.scope_digest().unwrap()))
        );
    }

    /// `hex(scope_digest(Widget{[wp_block, "b1"]}))`.
    const SCOPE_DIGEST_B1: &str = "9c59c4ded04a69ccf5b2ac58797c3b77";
}
