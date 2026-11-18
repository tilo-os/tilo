//! Who is reading and writing: the member a view is for.
//!
//! The id is the one the host gives the view (`ViewCx::me`), the same id
//! every action that member takes is signed with, so authorship,
//! reactions and read watermarks are attributed alike on every replica. What
//! is resolved here is only how to show that member before the workspace
//! names them: a display name, a monogram and a colour.
//!
//! The demo used by screenshots and tests reads as [`LocalIdentity::demo`],
//! the seeded demo's own person (`u_me` / "Quinn Davis").

use ui::tilo_resource::MemberId;

use crate::model;

/// What the demo's own person is called: a name, like anyone else's.
pub const DEMO_NAME: &str = "Quinn Davis";

/// The member a view is for, as it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    pub id: MemberId,
    pub name: String,
    pub initials: String,
}

impl LocalIdentity {
    /// The demo's own person — identical to the seeded demo's `u_me`.
    pub fn demo() -> Self {
        Self {
            id: MemberId::person(model::ME),
            name: DEMO_NAME.to_string(),
            initials: "QD".to_string(),
        }
    }

    /// Member `id` as this device shows its own person: the display
    /// name of the person's public identity resource.
    pub fn for_member(id: &MemberId) -> Self {
        let name = ui::local_identity::own_name().unwrap_or_else(|| unnamed_label(id));
        Self {
            initials: initials_of(&name),
            id: id.clone(),
            name,
        }
    }
}

/// Distinguish unnamed members without exposing their complete routing id.
pub(crate) use chat_model::chat::unnamed_label;

/// Shared person rows cannot use the current-reader label without an identity tag.
/// Preserve the saved name and id; only its presentation is disambiguated.
pub(crate) fn shared_name_label(id: &MemberId, name: &str) -> String {
    if name.trim().is_empty() {
        unnamed_label(id)
    } else if name.trim() == "You" {
        crate::i18n::t_args(
            "chat-person-relative-name",
            &[("name", name.trim().into()), ("tag", member_tag(id).into())],
        )
    } else {
        name.to_owned()
    }
}

/// The person `acting` acted for, when it acted for one.
pub(crate) fn acted_for(acting: &ui::tilo_resource::Acting) -> Option<&MemberId> {
    acting
        .acted_for
        .first()?
        .member_id()
        .filter(|member| member.is_person())
}

pub(crate) use chat_model::chat::member_tag;

/// Up-to-two-letter uppercase initials for an avatar monogram. Delegates the
/// word-split to the shared toolkit helper (so every app derives a monogram
/// the same way) and applies chat's own "Y" fallback for a nameless peer.
fn initials_of(name: &str) -> String {
    let s = ui::widget::initials_for_name(name);
    if s.is_empty() { "Y".to_string() } else { s }
}

/// `initials_of`, for a caller that has just learned the name the workspace
/// gave this member and has to move the monogram with it.
pub fn initials_for(name: &str) -> String {
    initials_of(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_derivation_is_stable_and_tidy() {
        let me = LocalIdentity::demo();
        assert_eq!(me.id, MemberId::person(model::ME), "case demo: id");
        assert_eq!(me.name, DEMO_NAME, "case demo: name");

        let alice = MemberId::person("alice");
        let a = LocalIdentity::for_member(&alice);
        assert_eq!(a.id, alice);
        assert_eq!(a, LocalIdentity::for_member(&alice), "stable");

        assert_eq!(
            initials_of("Ada Lovelace"),
            "AL",
            "case initials: two words"
        );
        assert_eq!(initials_of("mononym"), "M", "case initials: mononym");
    }
}
